//! `upgrade_library`: bring a `.kicad_sym` library or a `.pretty` footprint
//! library to the running KiCad's file format with `kicad-cli sym upgrade` or
//! `fp upgrade`, and replace the originals only when the upgrade kept what they
//! hold (#628).
//!
//! The upgrade always runs into a temporary copy. Each file older than the
//! format kicad-cli writes is compared with its upgraded copy by content, as
//! KiCad reads both: what a library holds survives an upgrade, its formatting
//! does not (new UUIDs, default flags written out, pins in number order, and
//! the conversions KiCad's own loader makes on an older file).

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tool;
use crate::tools::{invalid_arg, ToolContext, ToolDef};
use copperpour_sexp::{try_commit_file_transaction, FileTransition, SexpError, SexpNode};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const OPERATION: &str = "upgrade_library";

/// The symbol library version from which `~` is text, not "empty"
/// (`eeschema/sch_file_versions.h`, 20250318).
const TILDE_IS_TEXT: u32 = 20250318;

/// The version below which KiCad converts `~text~` overbars to `~{text}`, in
/// symbol libraries (`SCH_IO_KICAD_SEXPR_PARSER`, pins and the Value field) and
/// footprints (`PCB_IO_KICAD_SEXPR_PARSER::parseEDA_TEXT`, every text).
const NEW_OVERBARS: u32 = 20210606;

/// The footprint version below which KiCad reads `(attr virtual)` as both
/// exclusion flags, and a footprint without attributes as through-hole
/// (`PCB_IO_KICAD_SEXPR_PARSER::parseFOOTPRINT_unchecked`).
const NEW_ATTRIBUTES: u32 = 20200826;

/// The footprint version up to which a margin or clearance of 0 meant
/// "inherit", which KiCad reads as no override ("In pre-9.0 files").
const NULLABLE_OVERRIDES: u32 = 20240201;

/// KiCad's schematic and board unit, 0.0001 mm: coordinates are compared in it.
const UNITS_PER_MM: f64 = 1e4;

/// How many differences one file reports before it says how many more there are.
const SHOWN_DIFFERENCES: usize = 20;

/// KiCad's default pin name offset, 20 mils (`DEFAULT_PIN_NAME_OFFSET`).
const DEFAULT_PIN_NAME_OFFSET: i64 = 5080;

/// The first symbol file version that declares its body styles; KiCad infers
/// De Morgan styles from the drawings of an older root symbol that declares
/// none (`SCH_IO_KICAD_SEXPR_PARSER::parseLibSymbol`).
const DECLARED_BODY_STYLES: u32 = 20250827;

pub fn tools() -> Vec<ToolDef> {
    vec![tool()]
}

fn tool() -> ToolDef {
    tool!(
        "upgrade_library",
        "Upgrade a symbol library (.kicad_sym) or footprint library (.pretty directory) to the \
         running KiCad's file format with kicad-cli, and replace the originals only when the \
         upgrade kept their content. The upgrade runs into a temporary copy; each file older than \
         the format kicad-cli writes is compared with its copy as KiCad reads both. Symbols: \
         extends, power, flags, fields (value and visibility), pin name and number display, unit \
         names, pins (unit, body style, number, name, type, style, position, length, hidden, \
         alternates), jumper pin groups, body style names, embedded files and graphic counts. \
         Footprints: description, tags, fields (value and visibility), attributes, margins, \
         clearance, zone connection, net-tie and jumper pad groups, private layers, pads \
         (number, type, shape, position, size, drill, layers, padstack, margins, clearance, zone, \
         thermal and teardrop settings, corner ratios, custom primitives count and options, \
         fabrication property), graphic counts, hidden texts, zones, groups, embedded files and \
         3D models. Graphics, zones and groups are compared by count, not geometry, and text \
         positions not at all. A footprint's name inside its file is not compared, since KiCad \
         uses the file name; a mismatch is a note. A dry run (the default) reports and writes \
         nothing. An apply settles an earlier interrupted transaction in the library's directory \
         first, then replaces the older files in one journalled transaction: every file is \
         checked against what was read, and refused if read-only, before any is written, and a \
         transaction interrupted after its journal is completed by the next call. Each file is \
         locked for the transaction, so a library with more older files than the process may \
         open is refused with nothing written. It is refused as plan_blocked when any file's \
         content differs. Older KiCad versions cannot open an upgraded file.",
        json!({
            "type": "object",
            "properties": {
                "library_path": { "type": "string", "description": "Path to a .kicad_sym symbol library file, or to a .pretty footprint library directory" },
                "dry_run": { "type": "boolean", "default": true, "description": "Report what the upgrade would change and whether the content is kept, writing nothing. false replaces the older files with their upgraded copies, refused when any file's content differs" },
                "limit": { "type": "integer", "minimum": 0, "default": 50, "description": "Maximum files listed in files, those that differ first, then those with notes, upgraded and current ones. counts gives every status; shown/truncated describe the list" }
            },
            "required": ["library_path"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_upgrade_library(args, ctx).await }
    )
}

/// Which library a path names, and the kicad-cli command that upgrades it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Symbol,
    Footprint,
}

impl Kind {
    fn command(self) -> &'static str {
        match self {
            Kind::Symbol => "sym",
            Kind::Footprint => "fp",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Symbol => "symbol",
            Kind::Footprint => "footprint",
        }
    }
}

/// One library file as read: its name in the library, path and text.
struct LibraryFile {
    name: String,
    path: PathBuf,
    text: String,
}

/// Whether `path` has `extension` as KiCad matches a library's files: in any
/// case on Windows, exactly elsewhere (wxDir's pattern match).
fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|found| found.to_str())
        .is_some_and(|found| {
            if cfg!(windows) {
                found.eq_ignore_ascii_case(extension)
            } else {
                found == extension
            }
        })
}

/// Where a library's transaction journal goes: the `.pretty` directory, or
/// the directory holding the `.kicad_sym` (`.` for a bare file name).
fn journal_directory(path: &Path, kind: Kind) -> PathBuf {
    match kind {
        Kind::Footprint => path.to_path_buf(),
        Kind::Symbol => match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        },
    }
}

/// The library a path names, or why it names none.
fn library_kind(path: &Path) -> Result<Kind, CallToolResult> {
    if !path.exists() {
        return Err(CallToolResult::error_kind(
            ToolErrorKind::file_not_found(path),
            format!("Library not found: {}", path.display()),
        ));
    }
    if path.is_file() && has_extension(path, "kicad_sym") {
        Ok(Kind::Symbol)
    } else if path.is_dir() && has_extension(path, "pretty") {
        Ok(Kind::Footprint)
    } else {
        Err(invalid_arg(
            "library_path",
            &format!(
                "'{}' is neither a .kicad_sym file nor a .pretty directory",
                path.display()
            ),
        ))
    }
}

/// The library's files: the `.kicad_sym` itself, or each `.kicad_mod` of the
/// `.pretty` directory, in name order.
fn read_library(path: &Path, kind: Kind) -> anyhow::Result<Vec<LibraryFile>> {
    let read = |path: PathBuf| -> anyhow::Result<LibraryFile> {
        let text = std::fs::read_to_string(&path)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(LibraryFile { name, path, text })
    };
    match kind {
        Kind::Symbol => Ok(vec![read(path.to_path_buf())?]),
        Kind::Footprint => {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(path)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_file() && has_extension(path, "kicad_mod"))
                .collect();
            paths.sort();
            paths.into_iter().map(read).collect()
        }
    }
}

/// The file's top-level `(version N)`.
fn version(root: &SexpNode) -> Option<u32> {
    root.find("version")?.get(1)?.as_str()?.parse().ok()
}

fn file_version(text: &str) -> Option<u32> {
    copperpour_sexp::parse_sexp(text)
        .ok()
        .as_ref()
        .and_then(version)
}

/// A length or coordinate in KiCad's units; text that is not a number is kept
/// as a sentinel no number rounds to, so it still compares.
fn units(node: Option<&SexpNode>) -> i64 {
    node.and_then(SexpNode::as_str)
        .and_then(|text| text.parse::<f64>().ok())
        .map_or(i64::MIN, |mm| (mm * UNITS_PER_MM).round() as i64)
}

/// A ratio or angle, compared to six decimal places.
fn fine(node: Option<&SexpNode>) -> Option<i64> {
    node.and_then(SexpNode::as_str)
        .and_then(|text| text.parse::<f64>().ok())
        .map(|value| (value * 1e6).round() as i64)
}

fn string(node: Option<&SexpNode>) -> String {
    node.and_then(SexpNode::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A `yes`/`no` flag: `(head yes)`, `(head no)`, or a bare `(head)` as yes.
fn flag(node: &SexpNode, head: &str) -> Option<bool> {
    let child = node.find(head)?;
    Some(child.get(1).and_then(SexpNode::as_str) != Some("no"))
}

/// Whether `node` holds a bare `atom`.
fn has_atom(node: &SexpNode, atom: &str) -> bool {
    node.children()
        .unwrap_or(&[])
        .iter()
        .any(|child| matches!(child, SexpNode::Atom(found) if found == atom))
}

/// Whether `node` is hidden: KiCad 8 and later write `(hide yes)`; earlier
/// versions a bare `hide` in the item or in its `(effects …)`.
fn hidden(node: &SexpNode) -> bool {
    flag(node, "hide").unwrap_or(false)
        || has_atom(node, "hide")
        || node.find("effects").is_some_and(|effects| {
            has_atom(effects, "hide") || flag(effects, "hide").unwrap_or(false)
        })
}

/// KiCad's `ConvertToNewOverbarNotation` (`common/string_utils.cpp`): `~text~`
/// overbars as `~{text}`, `~~` as `~`.
fn new_overbars(old: &str) -> String {
    if old == "~" {
        return old.to_string();
    }
    let chars: Vec<char> = old.chars().collect();
    let mut out = String::with_capacity(old.len());
    let mut in_overbar = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '~' {
            match chars.get(index + 1) {
                Some('~') if chars.get(index + 2) == Some(&'{') => {
                    out.push_str("~~{}");
                    index += 1;
                    continue;
                }
                Some('~') => {
                    out.push('~');
                    index += 2;
                    continue;
                }
                Some('{') => return old.to_string(),
                _ => {
                    out.push_str(if in_overbar { "}" } else { "~{" });
                    in_overbar = !in_overbar;
                    index += 1;
                    continue;
                }
            }
        }
        if matches!(c, ' ' | '}' | ')') && in_overbar {
            out.push('}');
            in_overbar = false;
        }
        out.push(c);
        index += 1;
    }
    if in_overbar {
        out.push('}');
    }
    out
}

// ─── Symbols ────────────────────────────────────────────────────────────────

/// One pin as KiCad reads it, coordinates in KiCad's units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PinContent {
    unit: u32,
    body_style: u32,
    number: String,
    name: String,
    electrical: String,
    style: String,
    x: i64,
    y: i64,
    angle: i64,
    length: i64,
    hidden: bool,
    /// `(alternate NAME TYPE STYLE)`, in name order.
    alternates: Vec<(String, String, String)>,
}

impl std::fmt::Display for PinContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "pin {} '{}' ({} {}) in unit {} body style {} at ({}, {}) angle {} length {}{}",
            self.number,
            self.name,
            self.electrical,
            self.style,
            self.unit,
            self.body_style,
            self.x as f64 / UNITS_PER_MM,
            self.y as f64 / UNITS_PER_MM,
            self.angle,
            self.length as f64 / UNITS_PER_MM,
            if self.hidden { ", hidden" } else { "" }
        )?;
        if !self.alternates.is_empty() {
            write!(formatter, ", alternates {:?}", self.alternates)?;
        }
        Ok(())
    }
}

/// A field's value and whether it is hidden.
type Field = (String, bool);

/// What a symbol holds, as the comparison sees it.
#[derive(Debug, Default, PartialEq)]
struct SymbolContent {
    extends: Option<String>,
    power: Option<String>,
    flags: BTreeMap<&'static str, bool>,
    /// Field name to value and visibility. A mandatory field with an empty
    /// value reads as absent, since an upgrade writes an absent one empty.
    fields: BTreeMap<String, Field>,
    /// `(offset, hidden)` of the pin names, and whether pin numbers are hidden.
    pin_names: (i64, bool),
    pin_numbers_hidden: bool,
    /// `(unit, body style)` to the unit's display name.
    unit_names: BTreeMap<(u32, u32), String>,
    pins: Vec<PinContent>,
    /// `(unit, body style, kind)` to how many graphic items of that kind.
    graphics: BTreeMap<(u32, u32, String), usize>,
    /// Jumper pin groups, body style names and embedded files, as written.
    jumper_pin_groups: Option<String>,
    body_styles: Option<String>,
    embedded_files: Vec<EmbeddedFile>,
}

/// An embedded file: name, type and checksum, which identify its content.
type EmbeddedFile = (String, String, String);

/// The files of an item's `(embedded_files (file (name …) (type …) (checksum
/// …)) …)`, in name order.
fn embedded_files(node: &SexpNode) -> Vec<EmbeddedFile> {
    let mut files: Vec<EmbeddedFile> = node
        .find("embedded_files")
        .map(|files| files.find_all("file"))
        .unwrap_or_default()
        .into_iter()
        .map(|file| {
            let part = |head| string(file.find(head).and_then(|part| part.get(1)));
            (part("name"), part("type"), part("checksum"))
        })
        .collect();
    files.sort();
    files
}

/// A mandatory field name as KiCad reads it, in any case (`CmpNoCase`).
fn canonical_field_name(name: String) -> String {
    MANDATORY_FIELDS
        .iter()
        .find(|field| field.eq_ignore_ascii_case(&name))
        .map_or(name, |field| field.to_string())
}

/// The symbol flags an upgrade may write out, with the value KiCad reads when
/// one is absent (`SCH_IO_KICAD_SEXPR_PARSER::parseLibSymbol`).
const SYMBOL_FLAGS: [(&str, bool); 5] = [
    ("in_bom", true),
    ("on_board", true),
    ("exclude_from_sim", false),
    ("in_pos_files", true),
    ("duplicate_pin_numbers_are_jumpers", false),
];

const SYMBOL_GRAPHICS: [&str; 7] = [
    "rectangle",
    "polyline",
    "circle",
    "arc",
    "bezier",
    "text",
    "text_box",
];

/// KiCad's mandatory fields, which an upgrade writes even when empty.
const MANDATORY_FIELDS: [&str; 5] = [
    "Reference",
    "Value",
    "Footprint",
    "Datasheet",
    "Description",
];

/// The `NAME_<unit>_<body style>` suffix of a unit block.
fn unit_and_body_style(name: &str) -> Option<(u32, u32)> {
    let mut parts = name.rsplitn(3, '_');
    let body_style = parts.next()?.parse().ok()?;
    let unit = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((unit, body_style))
}

/// A root symbol's body styles as KiCad reads them: `demorgan` or the
/// quoted names it declares, or `demorgan` inferred from a unit block of body
/// style 2 or more in a file older than [`DECLARED_BODY_STYLES`]. KiCad writes
/// them only on a root symbol; a derived one takes its root's.
fn body_styles(symbol: &SexpNode, file_version: u32) -> Option<String> {
    if symbol.find("extends").is_some() {
        return None;
    }
    if let Some(declared) = symbol.find("body_styles") {
        let styles: Vec<String> = declared
            .children()
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .map(|style| match style {
                SexpNode::Str(name) => format!("{name:?}"),
                other => string(Some(other)),
            })
            .collect();
        return Some(styles.join(" "));
    }
    let alternate = symbol.find_all("symbol").iter().any(|unit| {
        unit.get(1)
            .and_then(SexpNode::as_str)
            .and_then(unit_and_body_style)
            .is_some_and(|(_, body_style)| body_style > 1)
    });
    (file_version < DECLARED_BODY_STYLES && alternate).then(|| "demorgan".to_string())
}

/// A symbol name as KiCad reads it: the item name of a `lib:name` identifier,
/// which early KiCad 6 libraries wrote (`LIB_ID::Parse`).
fn symbol_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(_, item)| item)
}

/// Every top-level symbol of a `.kicad_sym` by name, or why it cannot be read.
fn symbol_library_content(text: &str) -> Result<BTreeMap<String, SymbolContent>, String> {
    let root = copperpour_sexp::parse_sexp(text).map_err(|error| error.to_string())?;
    if root.head() != Some("kicad_symbol_lib") {
        return Err("it is not a (kicad_symbol_lib …) file".to_string());
    }
    let file_version = version(&root).unwrap_or(0);
    let tilde_is_empty = file_version < TILDE_IS_TEXT;
    let old_overbars = file_version < NEW_OVERBARS;
    // A pin name or number as KiCad reads it: `~` empty before 20250318,
    // overbars converted before 20210606.
    let pin_text = |node: Option<&SexpNode>| {
        let text = string(node);
        if tilde_is_empty && text == "~" {
            String::new()
        } else if old_overbars {
            new_overbars(&text)
        } else {
            text
        }
    };
    let mut symbols = BTreeMap::new();
    for symbol in root.find_all("symbol") {
        let name = symbol_name(
            symbol
                .get(1)
                .and_then(SexpNode::as_str)
                .ok_or("it holds a symbol without a name")?,
        )
        .to_string();
        let pin_names = symbol.find("pin_names");
        let mut content = SymbolContent {
            extends: symbol
                .find_str("extends")
                .map(|parent| symbol_name(parent).to_string()),
            power: symbol.find("power").map(|power| {
                power
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .unwrap_or("global")
                    .to_string()
            }),
            pin_names: (
                pin_names
                    .and_then(|names| names.find("offset"))
                    .map_or(DEFAULT_PIN_NAME_OFFSET, |offset| units(offset.get(1))),
                pin_names.is_some_and(hidden),
            ),
            pin_numbers_hidden: symbol.find("pin_numbers").is_some_and(hidden),
            jumper_pin_groups: symbol
                .find("jumper_pin_groups")
                .map(|groups| format!("{groups:?}")),
            body_styles: body_styles(symbol, file_version),
            embedded_files: embedded_files(symbol),
            ..SymbolContent::default()
        };
        for (head, default) in SYMBOL_FLAGS {
            content
                .flags
                .insert(head, flag(symbol, head).unwrap_or(default));
        }
        for property in symbol.find_all("property") {
            let key = string(property.get(1));
            // KiCad 7 and earlier kept the description in ki_description,
            // which KiCad reads as a hidden Description field.
            let legacy_description = key == "ki_description";
            let key = if legacy_description {
                "Description".to_string()
            } else {
                canonical_field_name(key)
            };
            let mut value = string(property.get(2));
            if tilde_is_empty && value == "~" {
                value.clear();
            } else if old_overbars && key == "Value" {
                value = new_overbars(&value);
            }
            // An empty mandatory field reads as absent, and KiCad drops empty
            // keywords and filters; ki_locked is always empty, and present.
            let droppable = MANDATORY_FIELDS.contains(&key.as_str())
                || matches!(key.as_str(), "ki_keywords" | "ki_fp_filters");
            if value.is_empty() && droppable {
                continue;
            }
            // ki_* properties are not drawn; only their value counts.
            let hide = legacy_description || (!key.starts_with("ki_") && hidden(property));
            content.fields.insert(key, (value, hide));
        }
        let mut collect = |node: &SexpNode, place: (u32, u32)| {
            if let Some(unit_name) = node.find_str("unit_name") {
                content.unit_names.insert(place, unit_name.to_string());
            }
            for kind in SYMBOL_GRAPHICS {
                let count = node.find_all(kind).len();
                if count > 0 {
                    *content
                        .graphics
                        .entry((place.0, place.1, kind.to_string()))
                        .or_default() += count;
                }
            }
            for pin in node.find_all("pin") {
                let at = pin.find("at");
                // KiCad reads alternates verbatim, one per name, the last
                // winning (`SCH_IO_KICAD_SEXPR_PARSER::parseSymbolPin`).
                let alternates: Vec<(String, String, String)> = pin
                    .find_all("alternate")
                    .into_iter()
                    .map(|alternate| {
                        (
                            string(alternate.get(1)),
                            (string(alternate.get(2)), string(alternate.get(3))),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(name, (electrical, style))| (name, electrical, style))
                    .collect();
                content.pins.push(PinContent {
                    unit: place.0,
                    body_style: place.1,
                    number: pin_text(pin.find("number").and_then(|number| number.get(1))),
                    name: pin_text(pin.find("name").and_then(|name| name.get(1))),
                    electrical: string(pin.get(1)),
                    style: string(pin.get(2)),
                    x: units(at.and_then(|at| at.get(1))),
                    y: units(at.and_then(|at| at.get(2))),
                    angle: at
                        .and_then(|at| at.get_f64(3))
                        .map_or(0, |angle| angle.round() as i64),
                    length: units(pin.find("length").and_then(|length| length.get(1))),
                    hidden: hidden(pin),
                    alternates,
                });
            }
        };
        // Items written directly under the symbol are KiCad's unit 1, body
        // style 1; unit blocks carry their own.
        collect(symbol, (1, 1));
        for unit in symbol.find_all("symbol") {
            let place = unit
                .get(1)
                .and_then(SexpNode::as_str)
                .and_then(unit_and_body_style)
                .ok_or_else(|| format!("symbol '{name}' holds a unit KiCad would not read"))?;
            collect(unit, place);
        }
        content.pins.sort();
        symbols.insert(name, content);
    }
    Ok(symbols)
}

// ─── Footprints ─────────────────────────────────────────────────────────────

/// One pad as the comparison sees it, coordinates in KiCad's units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PadContent {
    number: String,
    pad_type: String,
    shape: String,
    x: i64,
    y: i64,
    angle: i64,
    width: i64,
    height: i64,
    drill: Option<Drill>,
    layers: BTreeSet<String>,
    /// The pad's own settings, by KiCad's name: margins, clearance, zone and
    /// thermal settings, corner ratios, fabrication property and the like.
    settings: BTreeMap<String, String>,
}

/// A pad's drill as KiCad reads it: oval or round, its size and offset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Drill {
    oval: bool,
    width: i64,
    height: i64,
    offset: (i64, i64),
}

impl std::fmt::Display for PadContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mm = |units: i64| units as f64 / UNITS_PER_MM;
        write!(
            formatter,
            "pad '{}' ({} {}) at ({}, {}) angle {} size {} x {}",
            self.number,
            self.pad_type,
            self.shape,
            mm(self.x),
            mm(self.y),
            self.angle as f64 / 1e6,
            mm(self.width),
            mm(self.height)
        )?;
        if let Some(drill) = &self.drill {
            write!(
                formatter,
                " drill {}{} x {}",
                if drill.oval { "oval " } else { "" },
                mm(drill.width),
                mm(drill.height)
            )?;
            if drill.offset != (0, 0) {
                write!(
                    formatter,
                    " offset ({}, {})",
                    mm(drill.offset.0),
                    mm(drill.offset.1)
                )?;
            }
        }
        write!(
            formatter,
            " layers {}",
            self.layers.iter().cloned().collect::<Vec<_>>().join(" ")
        )?;
        if !self.settings.is_empty() {
            write!(formatter, " {:?}", self.settings)?;
        }
        Ok(())
    }
}

/// A 3D model reference: path, offset, scale and rotation in KiCad's units,
/// opacity, and visibility.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelContent {
    path: String,
    offset: [i64; 3],
    scale: [i64; 3],
    rotate: [i64; 3],
    opacity: i64,
    hidden: bool,
}

/// What a footprint holds, as the comparison sees it.
#[derive(Debug, Default, PartialEq)]
struct FootprintContent {
    name: String,
    description: String,
    tags: String,
    /// As for symbols, a mandatory field with an empty value reads as absent.
    fields: BTreeMap<String, Field>,
    attributes: BTreeSet<String>,
    /// The footprint's own settings: margins, clearance, zone connection,
    /// net-tie pad groups and private layers.
    settings: BTreeMap<String, String>,
    pads: Vec<PadContent>,
    /// `(kind, layer)` to how many graphic items.
    graphics: BTreeMap<(String, String), usize>,
    /// `(text, layer)` of each hidden user text and each hidden field named
    /// `Field<n>`, to how many: KiCad 8 and later load a hidden footprint text
    /// as such a field (`PCB_IO_KICAD_SEXPR_PARSER::parsePCB_TEXT`,
    /// `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp:3938-3943`), and
    /// its number depends on the fields before it.
    hidden_texts: BTreeMap<(String, String), usize>,
    zones: usize,
    groups: usize,
    models: Vec<ModelContent>,
    embedded_files: Vec<EmbeddedFile>,
}

/// The footprint version up to which KiCad kept net ties in the keywords: a
/// footprint whose keywords start with "net tie" gets one pad group of all its
/// pads (`LEGACY_NET_TIES`, `parseFOOTPRINT_unchecked`).
const LEGACY_NET_TIES: u32 = 20220815;

const FOOTPRINT_GRAPHICS: [&str; 8] = [
    "fp_line",
    "fp_rect",
    "fp_circle",
    "fp_arc",
    "fp_poly",
    "fp_curve",
    "fp_text",
    "fp_text_box",
];

/// The non-copper layer kinds KiCad writes as `*.X` when a set holds both
/// `F.X` and `B.X` (`PCB_IO_KICAD_SEXPR::formatLayers`).
const PAIRED_LAYERS: [&str; 6] = ["Adhes", "Paste", "SilkS", "Mask", "CrtYd", "Fab"];

/// A pad's layers as KiCad writes them (`PCB_IO_KICAD_SEXPR::formatLayers`,
/// `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp:1560-1625`): copper on
/// exactly front and back, or on all layers, as `*.Cu`; each other front and
/// back pair as `*.X`. An older file's `F&B.Cu`, or `F.Mask` with `B.Mask`,
/// therefore reads as the wildcard KiCad writes.
fn pad_layers<'a>(layers: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for layer in layers {
        match layer {
            "F&B.Cu" => {
                set.insert("F.Cu".to_string());
                set.insert("B.Cu".to_string());
            }
            wildcard if wildcard.starts_with("*.") && wildcard != "*.Cu" => {
                let kind = &wildcard[2..];
                set.insert(format!("F.{kind}"));
                set.insert(format!("B.{kind}"));
            }
            other => {
                set.insert(other.to_string());
            }
        }
    }
    let inner = set
        .iter()
        .any(|layer| layer.starts_with("In") && layer.ends_with(".Cu"));
    if set.contains("*.Cu") || (set.contains("F.Cu") && set.contains("B.Cu") && !inner) {
        set.retain(|layer| !layer.ends_with(".Cu"));
        set.insert("*.Cu".to_string());
    }
    for kind in PAIRED_LAYERS {
        let (front, back) = (format!("F.{kind}"), format!("B.{kind}"));
        if set.contains(&front) && set.contains(&back) {
            set.remove(&front);
            set.remove(&back);
            set.insert(format!("*.{kind}"));
        }
    }
    set
}

/// A pad's `(drill [oval] W [H] [(offset X Y)])` as KiCad reads it: a round
/// drill's height is its width, an oval's missing height too, and a drill of
/// size 0 without an offset is none (`parsePAD`).
fn drill(pad: &SexpNode) -> Option<Drill> {
    let node = pad.find("drill")?;
    let parts = node.children().unwrap_or(&[]);
    let oval = parts
        .get(1)
        .and_then(SexpNode::as_str)
        .is_some_and(|word| word == "oval");
    let sizes: Vec<i64> = parts
        .iter()
        .skip(1)
        .filter(|part| !matches!(part, SexpNode::List(_)))
        .filter(|part| part.as_str() != Some("oval"))
        .map(|part| units(Some(part)))
        .collect();
    let width = sizes.first().copied().unwrap_or(0);
    let height = if oval {
        sizes.get(1).copied().unwrap_or(width)
    } else {
        width
    };
    let offset = node.find("offset").map_or((0, 0), |offset| {
        (units(offset.get(1)), units(offset.get(2)))
    });
    if width == 0 && height == 0 && offset == (0, 0) {
        return None;
    }
    Some(Drill {
        oval,
        width,
        height,
        offset,
    })
}

/// Whether `name` is one KiCad gives a user field it creates, `Field<n>`
/// (`GetUserFieldName`).
fn is_user_field_name(name: &str) -> bool {
    name.strip_prefix("Field")
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

/// The settings a pad or footprint holds, by KiCad's current name. A margin
/// or clearance of 0 in a file up to [`NULLABLE_OVERRIDES`] meant "inherit",
/// so it reads as absent.
fn settings(
    node: &SexpNode,
    file_version: u32,
    heads: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (head, name) in heads {
        let Some(child) = node.find(head) else {
            continue;
        };
        let parts: Vec<String> = child
            .children()
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .map(|part| match part {
                SexpNode::List(_) => format!("{part:?}"),
                atom => match atom.as_str().and_then(|text| text.parse::<f64>().ok()) {
                    Some(number) => ((number * 1e6).round() as i64).to_string(),
                    None => string(Some(atom)),
                },
            })
            .collect();
        let nullable = matches!(
            *name,
            "solder_mask_margin"
                | "solder_paste_margin"
                | "solder_paste_margin_ratio"
                | "clearance"
        );
        if nullable && file_version <= NULLABLE_OVERRIDES && parts == ["0"] {
            continue;
        }
        out.insert(name.to_string(), parts.join(" "));
    }
    out
}

/// A pad's `(padstack …)` as KiCad reads it. KiCad's loader puts a copper
/// layer's `thermal_bridge_angle` on F.Cu, where the pad's own angle then
/// replaces it, so the per-layer angles KiCad 9 wrote are read as nothing
/// (`PCB_IO_KICAD_SEXPR_PARSER::parsePadstack`).
fn padstack_as_read(padstack: &SexpNode) -> String {
    let parts: Vec<SexpNode> = padstack
        .children()
        .unwrap_or(&[])
        .iter()
        .map(|part| match part {
            SexpNode::List(items) if part.head() == Some("layer") => SexpNode::List(
                items
                    .iter()
                    .filter(|item| item.head() != Some("thermal_bridge_angle"))
                    .cloned()
                    .collect(),
            ),
            other => other.clone(),
        })
        .collect();
    format!("{:?}", SexpNode::List(parts))
}

/// What a `.kicad_mod` holds, or why it cannot be read.
fn footprint_content(text: &str) -> Result<FootprintContent, String> {
    let root = copperpour_sexp::parse_sexp(text).map_err(|error| error.to_string())?;
    if !matches!(root.head(), Some("footprint" | "module")) {
        return Err("it is not a (footprint …) file".to_string());
    }
    let file_version = version(&root).unwrap_or(0);
    // A text as KiCad reads it: `%R`/`%V` as variables, overbars converted
    // before 20210606 (`parsePCB_TEXT`, `parseEDA_TEXT`).
    let text_of = |node: Option<&SexpNode>| {
        let text = string(node)
            .replace("%V", "${VALUE}")
            .replace("%R", "${REFERENCE}");
        if file_version < NEW_OVERBARS {
            new_overbars(&text)
        } else {
            text
        }
    };
    let mut attributes: BTreeSet<String> = root
        .find("attr")
        .and_then(SexpNode::children)
        .unwrap_or(&[])
        .iter()
        .skip(1)
        .filter_map(SexpNode::as_str)
        .map(str::to_string)
        .collect();
    if file_version < NEW_ATTRIBUTES {
        if attributes.remove("virtual") {
            attributes.insert("exclude_from_bom".to_string());
            attributes.insert("exclude_from_pos_files".to_string());
        }
        if attributes.is_empty() {
            attributes.insert("through_hole".to_string());
        }
    }
    let mut content = FootprintContent {
        name: string(root.get(1)),
        description: string(root.find("descr").and_then(|descr| descr.get(1))),
        tags: string(root.find("tags").and_then(|tags| tags.get(1))),
        attributes,
        settings: settings(
            &root,
            file_version,
            &[
                ("solder_mask_margin", "solder_mask_margin"),
                ("solder_paste_margin", "solder_paste_margin"),
                ("solder_paste_ratio", "solder_paste_margin_ratio"),
                ("solder_paste_margin_ratio", "solder_paste_margin_ratio"),
                ("clearance", "clearance"),
                ("zone_connect", "zone_connect"),
                ("net_tie_pad_groups", "net_tie_pad_groups"),
                ("jumper_pad_groups", "jumper_pad_groups"),
                ("private_layers", "private_layers"),
            ],
        ),
        zones: root.find_all("zone").len(),
        groups: root.find_all("group").len(),
        embedded_files: embedded_files(&root),
        ..FootprintContent::default()
    };
    if flag(&root, "duplicate_pad_numbers_are_jumpers") == Some(true) {
        content.settings.insert(
            "duplicate_pad_numbers_are_jumpers".to_string(),
            "yes".to_string(),
        );
    }
    if file_version <= LEGACY_NET_TIES && content.tags.starts_with("net tie") {
        let group = root
            .find_all("pad")
            .into_iter()
            .map(|pad| string(pad.get(1)))
            .collect::<Vec<_>>()
            .join(", ");
        if !group.is_empty() {
            content
                .settings
                .entry("net_tie_pad_groups".to_string())
                .or_insert(group);
        }
    }
    let layer_of = |item: &SexpNode| string(item.find("layer").and_then(|layer| layer.get(1)));
    let field = |content: &mut FootprintContent, name: String, value: String, hide: bool| {
        if !(value.is_empty() && MANDATORY_FIELDS.contains(&name.as_str())) {
            content.fields.insert(name, (value, hide));
        }
    };
    for property in root.find_all("property") {
        let (name, value) = (
            canonical_field_name(string(property.get(1))),
            text_of(property.get(2)),
        );
        if hidden(property) && is_user_field_name(&name) {
            *content
                .hidden_texts
                .entry((value, layer_of(property)))
                .or_default() += 1;
        } else {
            field(&mut content, name, value, hidden(property));
        }
    }
    for kind in FOOTPRINT_GRAPHICS {
        for item in root.find_all(kind) {
            if kind == "fp_text" {
                // KiCad 7 and earlier wrote Reference and Value as fp_text,
                // which KiCad 8 and later write as fields.
                let named = match item.get(1).and_then(SexpNode::as_str) {
                    Some("reference") => Some("Reference"),
                    Some("value") => Some("Value"),
                    _ => None,
                };
                if let Some(name) = named {
                    field(
                        &mut content,
                        name.to_string(),
                        text_of(item.get(2)),
                        hidden(item),
                    );
                    continue;
                }
                if hidden(item) {
                    *content
                        .hidden_texts
                        .entry((text_of(item.get(2)), layer_of(item)))
                        .or_default() += 1;
                    continue;
                }
            }
            *content
                .graphics
                .entry((kind.to_string(), layer_of(item)))
                .or_default() += 1;
        }
    }
    for pad in root.find_all("pad") {
        let at = pad.find("at");
        let size = pad.find("size");
        let mut pad_settings = settings(
            pad,
            file_version,
            &[
                ("solder_mask_margin", "solder_mask_margin"),
                ("solder_paste_margin", "solder_paste_margin"),
                ("solder_paste_margin_ratio", "solder_paste_margin_ratio"),
                ("clearance", "clearance"),
                ("zone_connect", "zone_connect"),
                ("thermal_width", "thermal_bridge_width"),
                ("thermal_bridge_width", "thermal_bridge_width"),
                ("thermal_gap", "thermal_gap"),
                ("thermal_bridge_angle", "thermal_bridge_angle"),
                ("roundrect_rratio", "roundrect_rratio"),
                ("chamfer_ratio", "chamfer_ratio"),
                ("chamfer", "chamfer"),
                ("property", "property"),
                ("die_length", "die_length"),
                ("pinfunction", "pinfunction"),
                ("pintype", "pintype"),
                ("teardrops", "teardrops"),
                ("options", "options"),
            ],
        );
        // KiCad keeps a corner ratio only on a pad shape that has corners.
        if !matches!(
            pad.get(3).and_then(SexpNode::as_str),
            Some("roundrect" | "chamfered_rect")
        ) {
            pad_settings.remove("roundrect_rratio");
            pad_settings.remove("chamfer_ratio");
            pad_settings.remove("chamfer");
        }
        if flag(pad, "remove_unused_layers") == Some(true) {
            pad_settings.insert("remove_unused_layers".to_string(), "yes".to_string());
        }
        if let Some(primitives) = pad.find("primitives") {
            pad_settings.insert(
                "primitives".to_string(),
                primitives
                    .children()
                    .unwrap_or(&[])
                    .len()
                    .saturating_sub(1)
                    .to_string(),
            );
        }
        if let Some(padstack) = pad.find("padstack") {
            pad_settings.insert("padstack".to_string(), padstack_as_read(padstack));
        }
        content.pads.push(PadContent {
            number: string(pad.get(1)),
            pad_type: string(pad.get(2)),
            shape: string(pad.get(3)),
            x: units(at.and_then(|at| at.get(1))),
            y: units(at.and_then(|at| at.get(2))),
            angle: fine(at.and_then(|at| at.get(3))).unwrap_or(0),
            width: units(size.and_then(|size| size.get(1))),
            height: units(size.and_then(|size| size.get(2))),
            drill: drill(pad),
            layers: pad_layers(
                pad.find("layers")
                    .and_then(SexpNode::children)
                    .unwrap_or(&[])
                    .iter()
                    .skip(1)
                    .filter_map(SexpNode::as_str),
            ),
            settings: pad_settings,
        });
    }
    content.pads.sort();
    for model in root.find_all("model") {
        // KiCad 5 wrote the offset as (at (xyz …)) in inches.
        let xyz = |head: &str, factor: f64| -> Option<[i64; 3]> {
            let xyz = model.find(head)?.find("xyz")?;
            let value = |index| {
                xyz.get_f64(index).map_or(i64::MIN, |value| {
                    (value * factor * UNITS_PER_MM).round() as i64
                })
            };
            Some([value(1), value(2), value(3)])
        };
        let unit = UNITS_PER_MM as i64;
        content.models.push(ModelContent {
            path: string(model.get(1)),
            offset: xyz("offset", 1.0)
                .or_else(|| xyz("at", 25.4))
                .unwrap_or([0; 3]),
            scale: xyz("scale", 1.0).unwrap_or([unit; 3]),
            rotate: xyz("rotate", 1.0).unwrap_or([0; 3]),
            opacity: fine(model.find("opacity").and_then(|opacity| opacity.get(1)))
                .unwrap_or(1_000_000),
            hidden: hidden(model),
        });
    }
    content.models.sort();
    Ok(content)
}

// ─── Comparison ─────────────────────────────────────────────────────────────

/// The differences between two maps, each as `what key: before → after`.
fn map_differences<K: Ord + std::fmt::Debug, V: PartialEq + std::fmt::Debug>(
    what: &str,
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
    out: &mut Vec<String>,
) {
    let keys: BTreeSet<&K> = before.keys().chain(after.keys()).collect();
    for key in keys {
        let (old, new) = (before.get(key), after.get(key));
        if old != new {
            out.push(format!("{what} {key:?}: {old:?} → {new:?}"));
        }
    }
}

/// The items one list has and the other lacks, one for one.
fn list_differences<T: Ord + std::fmt::Display>(before: &[T], after: &[T], out: &mut Vec<String>) {
    let mut before: Vec<&T> = before.iter().collect();
    let mut after: Vec<&T> = after.iter().collect();
    let mut index = 0;
    while index < before.len() {
        if let Some(found) = after.iter().position(|item| *item == before[index]) {
            after.remove(found);
            before.remove(index);
        } else {
            index += 1;
        }
    }
    out.extend(before.iter().map(|item| format!("lost {item}")));
    out.extend(after.iter().map(|item| format!("gained {item}")));
}

fn scalar<T: PartialEq + std::fmt::Debug>(what: &str, old: &T, new: &T, out: &mut Vec<String>) {
    if old != new {
        out.push(format!("{what} {old:?} → {new:?}"));
    }
}

fn symbol_differences(before: &SymbolContent, after: &SymbolContent) -> Vec<String> {
    let mut out = Vec::new();
    scalar("extends", &before.extends, &after.extends, &mut out);
    scalar("power", &before.power, &after.power, &mut out);
    map_differences("flag", &before.flags, &after.flags, &mut out);
    map_differences("field", &before.fields, &after.fields, &mut out);
    scalar(
        "pin names (offset, hidden)",
        &before.pin_names,
        &after.pin_names,
        &mut out,
    );
    scalar(
        "pin numbers hidden",
        &before.pin_numbers_hidden,
        &after.pin_numbers_hidden,
        &mut out,
    );
    map_differences("unit name", &before.unit_names, &after.unit_names, &mut out);
    list_differences(&before.pins, &after.pins, &mut out);
    map_differences("graphics", &before.graphics, &after.graphics, &mut out);
    scalar(
        "jumper pin groups",
        &before.jumper_pin_groups,
        &after.jumper_pin_groups,
        &mut out,
    );
    scalar(
        "body styles",
        &before.body_styles,
        &after.body_styles,
        &mut out,
    );
    scalar(
        "embedded files",
        &before.embedded_files,
        &after.embedded_files,
        &mut out,
    );
    out
}

fn footprint_differences(before: &FootprintContent, after: &FootprintContent) -> Vec<String> {
    let mut out = Vec::new();
    // KiCad names a library footprint after its file, not the name inside it
    // (`FP_CACHE::Load`, `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp:190-195`),
    // and writes that name on save: the handler notes a mismatch instead.
    scalar(
        "description",
        &before.description,
        &after.description,
        &mut out,
    );
    scalar("tags", &before.tags, &after.tags, &mut out);
    scalar(
        "attributes",
        &before.attributes,
        &after.attributes,
        &mut out,
    );
    map_differences("setting", &before.settings, &after.settings, &mut out);
    map_differences("field", &before.fields, &after.fields, &mut out);
    list_differences(&before.pads, &after.pads, &mut out);
    map_differences("graphics", &before.graphics, &after.graphics, &mut out);
    map_differences(
        "hidden text",
        &before.hidden_texts,
        &after.hidden_texts,
        &mut out,
    );
    scalar("zones", &before.zones, &after.zones, &mut out);
    scalar("groups", &before.groups, &after.groups, &mut out);
    scalar("3D models", &before.models, &after.models, &mut out);
    scalar(
        "embedded files",
        &before.embedded_files,
        &after.embedded_files,
        &mut out,
    );
    out
}

/// Every difference in content between a library file and its upgraded copy,
/// each prefixed with the symbol it is in; empty when the upgrade kept it.
fn content_differences(kind: Kind, before: &str, after: &str) -> Vec<String> {
    match kind {
        Kind::Symbol => {
            let (old, new) = match (
                symbol_library_content(before),
                symbol_library_content(after),
            ) {
                (Ok(old), Ok(new)) => (old, new),
                (Err(reason), _) => return vec![format!("the original cannot be read: {reason}")],
                (_, Err(reason)) => {
                    return vec![format!("the upgraded copy cannot be read: {reason}")]
                }
            };
            let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            names
                .into_iter()
                .flat_map(|name| match (old.get(name), new.get(name)) {
                    (Some(old), Some(new)) => symbol_differences(old, new)
                        .into_iter()
                        .map(|difference| format!("symbol '{name}': {difference}"))
                        .collect(),
                    (Some(_), None) => vec![format!("symbol '{name}' lost")],
                    (None, Some(_)) => vec![format!("symbol '{name}' gained")],
                    (None, None) => Vec::new(),
                })
                .collect()
        }
        Kind::Footprint => match (footprint_content(before), footprint_content(after)) {
            (Ok(old), Ok(new)) => footprint_differences(&old, &new),
            (Err(reason), _) => vec![format!("the original cannot be read: {reason}")],
            (_, Err(reason)) => vec![format!("the upgraded copy cannot be read: {reason}")],
        },
    }
}

// ─── The tool ───────────────────────────────────────────────────────────────

/// One library file's outcome.
struct FileReport {
    file: String,
    /// The library file this reports on, `None` for one kicad-cli made up.
    path: Option<PathBuf>,
    version: Option<u32>,
    upgraded_version: Option<u32>,
    /// The original and upgraded text, for a file older than kicad-cli's format.
    upgrade: Option<(String, String)>,
    differences: Vec<String>,
    /// What the upgrade changes that KiCad already reads so: not a difference.
    notes: Vec<String>,
}

impl FileReport {
    fn status(&self) -> &'static str {
        match (&self.upgrade, self.differences.is_empty()) {
            (_, false) => "differs",
            (None, true) => "current",
            (Some(_), true) => "upgraded",
        }
    }

    /// Where the file comes in the listing: differences first, then notes,
    /// then upgraded and current files.
    fn rank(&self) -> u8 {
        match (self.status(), self.notes.is_empty()) {
            ("differs", _) => 0,
            (_, false) => 1,
            ("upgraded", _) => 2,
            _ => 3,
        }
    }

    fn json(&self) -> serde_json::Value {
        let mut differences: Vec<String> = self
            .differences
            .iter()
            .take(SHOWN_DIFFERENCES)
            .cloned()
            .collect();
        let more = self.differences.len().saturating_sub(SHOWN_DIFFERENCES);
        if more > 0 {
            differences.push(format!("… and {more} more"));
        }
        let mut body = json!({
            "file": self.file,
            "version": self.version,
            "upgraded_version": self.upgraded_version,
            "status": self.status(),
            "differences": differences,
        });
        if !self.notes.is_empty() {
            body["notes"] = json!(self.notes);
        }
        body
    }
}

fn handler_error(message: String) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::HandlerError {
            reason: message.clone(),
        },
        message,
    )
}

async fn handle_upgrade_library(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = super::get_path(args, "library_path")?;
    // A bare file name has an empty parent, which no journal can go in.
    let path = std::path::absolute(&path).unwrap_or(path);
    let dry_run = match args.get("dry_run") {
        None | Some(serde_json::Value::Null) => true,
        Some(value) => match value.as_bool() {
            Some(dry_run) => dry_run,
            None => return Ok(invalid_arg("dry_run", "must be true or false")),
        },
    };
    let limit = match super::response_limit(args, 50) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };
    let kind = match library_kind(&path) {
        Ok(kind) => kind,
        Err(error) => return Ok(error),
    };
    // An earlier transaction interrupted in this directory could still change
    // the library's files: a dry run, which writes nothing, refuses while one
    // is pending; an apply settles it first, before reading, as
    // annotate_schematic does.
    let journal_dir = journal_directory(&path, kind);
    let shown_dir = journal_dir.display().to_string();
    let unsettled = |message: String| {
        CallToolResult::error_kind(
            ToolErrorKind::Conflict {
                paths: vec![shown_dir.clone()],
            },
            message,
        )
    };
    let settled = if dry_run {
        match copperpour_sexp::inspect_file_transactions(&journal_dir) {
            Ok(pending) if pending.is_empty() => 0,
            Ok(pending) => {
                return Ok(unsettled(format!(
                    "{} unfinished transaction(s) in {shown_dir} could still change the library; \
                     a dry run writes nothing, so it does not settle them. Apply without \
                     dry_run, which settles them first, or inspect them with `copperpour \
                     transaction status {shown_dir}`.",
                    pending.len()
                )))
            }
            Err(error) => {
                return Ok(unsettled(format!(
                    "An unfinished transaction in {shown_dir} could not be inspected ({error}); \
                     inspect it with `copperpour transaction status {shown_dir}`."
                )))
            }
        }
    } else {
        match copperpour_sexp::recover_file_transactions(&journal_dir) {
            Ok(settled) => settled.len(),
            Err(SexpError::TransactionConflict {
                path: conflict_path,
                journal,
                kind: copperpour_sexp::TransactionConflictKind::Recovery,
                ..
            }) => {
                return Ok(crate::tools::stuck_transaction_journal_result(
                    &journal,
                    &conflict_path,
                    OPERATION,
                ));
            }
            Err(error) => {
                return Ok(unsettled(format!(
                    "An unfinished transaction in {shown_dir} could not be settled ({error}), so \
                     upgrade_library read and wrote nothing of its own. Inspect it with \
                     `copperpour transaction status {shown_dir}`, then recover or abandon it."
                )))
            }
        }
    };
    let files = read_library(&path, kind)?;
    if files.is_empty() {
        return Ok(invalid_arg(
            "library_path",
            &format!("'{}' holds no .kicad_mod footprint", path.display()),
        ));
    }
    // kicad-cli refuses a whole library it cannot load and names no file
    // ("Unable to load library"): name them first.
    let unreadable: Vec<String> = files
        .iter()
        .filter_map(|file| {
            copperpour_sexp::parse_sexp(&file.text).err().map(|error| {
                // The parser's error carries the rest of the file: keep where.
                let error = error.to_string();
                let short = error.split(": Parsing Error").next().unwrap_or(&error);
                format!(
                    "{}: {}",
                    file.name,
                    short.chars().take(120).collect::<String>()
                )
            })
        })
        .collect();
    if !unreadable.is_empty() {
        return Ok(handler_error(format!(
            "upgrade_library wrote nothing: kicad-cli cannot load '{}', since these files do \
             not parse: {}",
            path.display(),
            unreadable.join("; ")
        )));
    }

    // kicad-cli upgrades a copy of exactly the text read and compared, so a
    // save to the library in between cannot reach the comparison; the
    // transaction still refuses to replace a file that changed.
    let staging = tempfile::tempdir()?;
    let library_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    let input = staging.path().join("read").join(&library_name);
    match kind {
        Kind::Symbol => {
            std::fs::create_dir_all(staging.path().join("read"))?;
            std::fs::write(&input, &files[0].text)?;
        }
        Kind::Footprint => {
            std::fs::create_dir_all(&input)?;
            for file in &files {
                std::fs::write(input.join(&file.name), &file.text)?;
            }
        }
    }
    let output = staging.path().join(&library_name);
    let written = match super::cli::upgrade_library_into(
        &ctx.config.kicad_cli,
        kind.command(),
        &input,
        &output,
    )
    .await
    {
        Ok(written) => written,
        Err(error) => {
            return Ok(handler_error(format!(
                "upgrade_library wrote nothing: kicad-cli could not upgrade '{}': {error:#}",
                path.display()
            )))
        }
    };

    let mut reports = Vec::new();
    for file in &files {
        let version = file_version(&file.text);
        let mut report = FileReport {
            file: file.name.clone(),
            path: Some(file.path.clone()),
            version,
            upgraded_version: version,
            upgrade: None,
            differences: Vec::new(),
            notes: Vec::new(),
        };
        if kind == Kind::Footprint {
            let stem = file.name[..file.name.len() - ".kicad_mod".len()].to_string();
            if let Ok(content) = footprint_content(&file.text) {
                if content.name != stem {
                    report.notes.push(format!(
                        "the name inside the file, '{}', is not its file name; KiCad uses the \
                         file name, '{stem}', and an upgrade writes it",
                        content.name
                    ));
                }
            }
        }
        if written {
            let copy = match kind {
                Kind::Symbol => output.clone(),
                Kind::Footprint => output.join(&file.name),
            };
            match std::fs::read_to_string(&copy) {
                Ok(upgraded) => {
                    let upgraded_version = file_version(&upgraded);
                    // A file already in kicad-cli's format is left as it is:
                    // kicad-cli resaves a whole footprint library at once.
                    if upgraded_version > version {
                        report.differences = content_differences(kind, &file.text, &upgraded);
                        report.upgraded_version = upgraded_version;
                        report.upgrade = Some((file.text.clone(), upgraded));
                    }
                }
                Err(_) => report
                    .differences
                    .push("kicad-cli wrote no upgraded copy of it".to_string()),
            }
        }
        reports.push(report);
    }
    if written && kind == Kind::Footprint {
        let known: BTreeSet<String> = files.iter().map(|file| file.name.to_lowercase()).collect();
        for entry in std::fs::read_dir(&output)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if has_extension(&entry.path(), "kicad_mod") && !known.contains(&name.to_lowercase()) {
                reports.push(FileReport {
                    file: name,
                    path: None,
                    version: None,
                    upgraded_version: file_version(
                        &std::fs::read_to_string(entry.path()).unwrap_or_default(),
                    ),
                    upgrade: None,
                    differences: vec![
                        "kicad-cli wrote a footprint the library does not have".to_string()
                    ],
                    notes: Vec::new(),
                });
            }
        }
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for report in &reports {
        *counts.entry(report.status()).or_default() += 1;
    }
    let differs = counts.contains_key("differs");
    let to_write: Vec<FileTransition> = reports
        .iter()
        .filter_map(|report| {
            let (original, upgraded) = report.upgrade.as_ref()?;
            Some(FileTransition::replace(
                report.path.clone()?,
                original.clone(),
                upgraded.clone(),
            ))
        })
        .collect();
    let status = if differs {
        "blocked"
    } else if to_write.is_empty() {
        "current"
    } else if dry_run {
        "ready"
    } else {
        "applied"
    };
    let format_version = reports
        .iter()
        .filter_map(|report| report.upgraded_version)
        .max();
    let mut listed: Vec<&FileReport> = reports.iter().collect();
    listed.sort_by_key(|report| (report.rank(), report.file.clone()));
    let mut body = json!({
        "library_path": path.display().to_string(),
        "kind": kind.label(),
        "dry_run": dry_run,
        "status": status,
        "format_version": format_version,
        "counts": counts,
        "files": listed.iter().take(limit).map(|report| report.json()).collect::<Vec<_>>(),
        "shown": listed.len().min(limit),
        "truncated": listed.len() > limit,
    });
    if settled > 0 {
        body["settled_transactions"] = json!(settled);
    }
    if !to_write.is_empty() {
        body["warning"] = json!(format!(
            "An upgraded file is in KiCad's format {}; KiCad versions older than the one that \
             wrote it cannot open it.",
            format_version.map_or_else(|| "?".to_string(), |version| version.to_string())
        ));
    }
    if differs && !dry_run {
        let reasons: Vec<String> = listed
            .iter()
            .filter(|report| report.status() == "differs")
            .flat_map(|report| {
                report
                    .differences
                    .iter()
                    .map(move |difference| format!("{}: {difference}", report.file))
            })
            .take(SHOWN_DIFFERENCES)
            .collect();
        let mut refusal = CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: OPERATION.to_string(),
                reasons,
            },
            format!(
                "upgrade_library wrote nothing: the upgrade would change the content of {} \
                 file(s); a dry run lists the differences",
                counts.get("differs").copied().unwrap_or(0)
            ),
        );
        refusal.content.push(ToolContent::Text {
            text: body.to_string(),
        });
        return Ok(refusal);
    }
    if dry_run || to_write.is_empty() {
        return Ok(CallToolResult::json(&body));
    }

    // A read-only file would stop the transaction after its journal, with
    // some files replaced: refuse it before anything is written.
    let read_only: Vec<String> = to_write
        .iter()
        .filter(|transition| {
            std::fs::metadata(transition.path())
                .is_ok_and(|metadata| metadata.permissions().readonly())
        })
        .map(|transition| copperpour_sexp::paths::display_path(transition.path()))
        .collect();
    if !read_only.is_empty() {
        return Ok(handler_error(format!(
            "upgrade_library wrote nothing: these files are read-only: {}",
            read_only.join(", ")
        )));
    }

    // One transaction over every file: each is checked against what was read,
    // under its lock, before any is replaced, and a journal beside the files
    // lets the next call (or `copperpour transaction`) complete one that is
    // interrupted after it was written.
    let replaced = to_write.len();
    if let Err(failure) = try_commit_file_transaction(&journal_dir, to_write) {
        if failure.journal.is_none()
            && !matches!(
                failure.error,
                SexpError::TransactionConflict {
                    kind: copperpour_sexp::TransactionConflictKind::AfterImage,
                    ..
                }
            )
        {
            // A file that changed after the read fails this transaction's own
            // before-image check; a recovery conflict belongs to an older,
            // unfinished transaction.
            let changed = match &failure.error {
                SexpError::Conflict { path }
                | SexpError::TransactionConflict {
                    path,
                    kind: copperpour_sexp::TransactionConflictKind::BeforeImage,
                    ..
                } => Some(path),
                _ => None,
            };
            if let Some(changed) = changed {
                let shown = copperpour_sexp::paths::display_path(changed);
                return Ok(CallToolResult::error_kind(
                    ToolErrorKind::Conflict {
                        paths: vec![shown.clone()],
                    },
                    format!(
                        "'{shown}' changed after upgrade_library read it, so upgrade_library \
                         wrote nothing. Run the dry run again."
                    ),
                ));
            }
            if let SexpError::TransactionConflict {
                path: conflict_path,
                journal,
                kind: copperpour_sexp::TransactionConflictKind::Recovery,
                ..
            } = &failure.error
            {
                return Ok(crate::tools::stuck_transaction_journal_result(
                    journal,
                    conflict_path,
                    OPERATION,
                ));
            }
            return Ok(handler_error(format!(
                "upgrade_library wrote nothing: {}",
                failure.error
            )));
        }
        return Ok(crate::tools::mutation_outcome_uncertain(
            &path,
            OPERATION,
            format!(
                "the transaction over {replaced} file(s) failed after its journal was written: {}; \
                 the next upgrade_library call completes it, and `copperpour transaction status \
                 {shown_dir}` shows what was applied",
                failure.error
            ),
        ));
    }
    body["replaced"] = json!(replaced);
    Ok(CallToolResult::json(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::cli::test_support::{noop_cli, write_script};
    use crate::tools::ServerConfig;

    /// A symbol as KiCad 9 writes it (version 20241209, so `~` is empty),
    /// with a KiCad 7 `ki_description`, a bare `hide` on a pin and no
    /// optional flags written. Abridged by hand.
    const SYMBOLS_KICAD9: &str = r#"(kicad_symbol_lib
	(version 20241209)
	(generator "kicad_symbol_editor")
	(symbol "D"
		(in_bom yes)
		(on_board yes)
		(property "Reference" "D" (at 0 2.54 0) (effects (font (size 1.27 1.27))))
		(property "Value" "D" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
		(property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
		(property "ki_description" "Diode" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
		(symbol "D_0_1"
			(polyline (pts (xy -1.27 1.27) (xy -1.27 -1.27)) (stroke (width 0.254) (type default)) (fill (type none)))
		)
		(symbol "D_1_1"
			(pin passive line (at 3.81 0 180) (length 2.54) hide (name "A" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
			(pin passive line (at -3.81 0 0) (length 2.54) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
		)
	)
)
"#;

    /// [`SYMBOLS_KICAD9`] in the form KiCad 10.0.8's `sym upgrade` writes:
    /// flags written out, `~` written empty, the description as a field, the
    /// pins in number order and the hidden pin as `(hide yes)`. Abridged.
    const SYMBOLS_KICAD10: &str = r#"(kicad_symbol_lib
	(version 20251024)
	(generator "kicad_symbol_editor")
	(generator_version "10.0")
	(symbol "D"
		(exclude_from_sim no)
		(in_bom yes)
		(on_board yes)
		(in_pos_files yes)
		(duplicate_pin_numbers_are_jumpers no)
		(property "Reference" "D" (at 0 2.54 0) (show_name no) (do_not_autoplace no) (effects (font (size 1.27 1.27))))
		(property "Value" "D" (at 0 -2.54 0) (show_name no) (do_not_autoplace no) (effects (font (size 1.27 1.27))))
		(property "Datasheet" "" (at 0 0 0) (hide yes) (show_name no) (do_not_autoplace no) (effects (font (size 1.27 1.27))))
		(property "Description" "Diode" (at 0 0 0) (hide yes) (show_name no) (do_not_autoplace no) (effects (font (size 1.27 1.27))))
		(symbol "D_0_1"
			(polyline (pts (xy -1.27 1.27) (xy -1.27 -1.27)) (stroke (width 0.254) (type default)) (fill (type none)))
		)
		(symbol "D_1_1"
			(pin passive line (at -3.81 0 0) (length 2.54) (name "" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
			(pin passive line (at 3.81 0 180) (length 2.54) (hide yes) (name "A" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
		)
		(embedded_fonts no)
	)
)
"#;

    /// A footprint as KiCad 7 writes it: Reference and Value as fp_text, a
    /// hidden user text with a bare `hide`, and a hole on `F&B.Cu`. Abridged.
    const FOOTPRINT_OLD: &str = r#"(footprint "R_X"
	(version 20221018)
	(generator "pcbnew")
	(layer "F.Cu")
	(descr "test resistor")
	(tags "resistor")
	(attr through_hole)
	(fp_text reference "REF**" (at 0 -1.5) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
	(fp_text value "R_X" (at 0 1.5) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
	(fp_text user "${REFERENCE}" (at 0 0) (layer "F.Fab") hide (effects (font (size 1 1) (thickness 0.15))))
	(fp_line (start -1 -1) (end 1 -1) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
	(pad "1" thru_hole circle (at -1 0) (size 1.5 1.5) (drill 0.8) (layers "*.Cu" "*.Mask"))
	(pad "" np_thru_hole circle (at 2 0) (size 1 1) (drill 1) (layers "F&B.Cu" "*.Mask"))
	(model "${KICAD6_3DMODEL_DIR}/x.wrl" (offset (xyz 0 0 1.016)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))
)
"#;

    /// [`FOOTPRINT_OLD`] in the form KiCad 10.0.8's `fp upgrade` writes:
    /// fields, UUIDs, empty Datasheet and Description, the hidden text as a
    /// hidden field `Field4`, the hole on `*.Cu`. Abridged.
    const FOOTPRINT_KICAD10: &str = r#"(footprint "R_X"
	(version 20260206)
	(generator "pcbnew")
	(generator_version "10.0")
	(layer "F.Cu")
	(descr "test resistor")
	(tags "resistor")
	(property "Reference" "REF**" (at 0 -1.5 0) (layer "F.SilkS") (uuid "a") (effects (font (size 1 1) (thickness 0.15))))
	(property "Value" "R_X" (at 0 1.5 0) (layer "F.Fab") (uuid "b") (effects (font (size 1 1) (thickness 0.15))))
	(property "Datasheet" "" (at 0 0 0) (layer "F.Fab") (hide yes) (uuid "c") (effects (font (size 1.27 1.27))))
	(property "Description" "" (at 0 0 0) (layer "F.Fab") (hide yes) (uuid "d") (effects (font (size 1.27 1.27))))
	(property "Field4" "${REFERENCE}" (at 0 0 0) (layer "F.Fab") (hide yes) (uuid "e") (effects (font (size 1 1) (thickness 0.15))))
	(attr through_hole)
	(duplicate_pad_numbers_are_jumpers no)
	(fp_line (start -1 -1) (end 1 -1) (stroke (width 0.12) (type solid)) (layer "F.SilkS") (uuid "f"))
	(pad "" np_thru_hole circle (at 2 0) (size 1 1) (drill 1) (layers "*.Cu" "*.Mask") (uuid "g"))
	(pad "1" thru_hole circle (at -1 0) (size 1.5 1.5) (drill 0.8) (layers "*.Cu" "*.Mask") (remove_unused_layers no) (uuid "h"))
	(embedded_fonts no)
	(model "${KICAD6_3DMODEL_DIR}/x.wrl" (offset (xyz 0 0 1.016)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))
)
"#;

    /// Hand-written legacy files and kicad-cli 10.0.8's upgrade of each; see
    /// the fixtures' README.
    const KICAD5_THT: &str =
        include_str!("../../tests/fixtures/library_upgrade/kicad5.pretty/THT_Legacy.kicad_mod");
    const KICAD10_THT: &str =
        include_str!("../../tests/fixtures/library_upgrade/kicad10.pretty/THT_Legacy.kicad_mod");
    const KICAD5_FIDUCIAL: &str = include_str!(
        "../../tests/fixtures/library_upgrade/kicad5.pretty/Fiducial_Legacy.kicad_mod"
    );
    const KICAD10_FIDUCIAL: &str = include_str!(
        "../../tests/fixtures/library_upgrade/kicad10.pretty/Fiducial_Legacy.kicad_mod"
    );
    const KICAD6_NET_TIE: &str =
        include_str!("../../tests/fixtures/library_upgrade/kicad5.pretty/NetTie_Legacy.kicad_mod");
    const KICAD10_NET_TIE: &str =
        include_str!("../../tests/fixtures/library_upgrade/kicad10.pretty/NetTie_Legacy.kicad_mod");
    const KICAD6_SYMBOLS: &str =
        include_str!("../../tests/fixtures/library_upgrade/kicad6_symbols/legacy.kicad_sym");
    const KICAD10_SYMBOLS: &str = include_str!(
        "../../tests/fixtures/library_upgrade/kicad6_symbols/legacy.kicad10.kicad_sym"
    );

    #[test]
    fn what_an_upgrade_rewrites_is_not_a_difference() {
        assert_eq!(
            content_differences(Kind::Symbol, SYMBOLS_KICAD9, SYMBOLS_KICAD10),
            Vec::<String>::new()
        );
        assert_eq!(
            content_differences(Kind::Footprint, FOOTPRINT_OLD, FOOTPRINT_KICAD10),
            Vec::<String>::new()
        );
    }

    /// KiCad's loader converts legacy forms: no attributes as through-hole,
    /// `virtual` as both exclusions, `%R` as `${REFERENCE}`, `~X~` overbars,
    /// `F.Mask B.Mask` as `*.Mask`, drill spellings, `(drill 0)` as none, a
    /// margin of 0 as no override, a net tie from its keywords, and `lib:name`
    /// symbols. kicad-cli 10.0.8's upgrade of hand-written files with each
    /// reads as the original.
    #[test]
    fn kicad_cli_s_upgrade_of_legacy_files_is_not_a_difference() {
        for (old, new) in [
            (KICAD5_THT, KICAD10_THT),
            (KICAD5_FIDUCIAL, KICAD10_FIDUCIAL),
            (KICAD6_NET_TIE, KICAD10_NET_TIE),
        ] {
            assert_eq!(
                content_differences(Kind::Footprint, old, new),
                Vec::<String>::new()
            );
        }
        assert_eq!(
            content_differences(Kind::Symbol, KICAD6_SYMBOLS, KICAD10_SYMBOLS),
            Vec::<String>::new()
        );
    }

    #[test]
    fn overbars_convert_as_kicad_converts_them() {
        for (old, new) in [
            ("~RST~", "~{RST}"),
            ("~RST~_N", "~{RST}_N"),
            ("~CS", "~{CS}"),
            ("~A B", "~{A} B"),
            ("A~~B", "A~B"),
            ("~", "~"),
            ("~{RST}", "~{RST}"),
            ("plain", "plain"),
        ] {
            assert_eq!(new_overbars(old), new, "{old}");
        }
    }

    #[test]
    fn a_change_in_content_is_named() {
        let renumbered = SYMBOLS_KICAD10.replace("(number \"1\"", "(number \"3\"");
        let differences = content_differences(Kind::Symbol, SYMBOLS_KICAD9, &renumbered);
        assert_eq!(differences.len(), 2, "{differences:?}");
        assert!(
            differences[0].starts_with("symbol 'D': lost pin 1 ''"),
            "{differences:?}"
        );
        assert!(
            differences[1].starts_with("symbol 'D': gained pin 3 ''"),
            "{differences:?}"
        );

        for (changed, says) in [
            (
                SYMBOLS_KICAD10.replace("\"Diode\"", "\"Rectifier\""),
                r#"symbol 'D': field "Description""#,
            ),
            (
                SYMBOLS_KICAD10.replace(
                    "(property \"Value\" \"D\" (at 0 -2.54 0)",
                    "(property \"Value\" \"D\" (at 0 -2.54 0) (hide yes)",
                ),
                r#"symbol 'D': field "Value": Some(("D", false)) → Some(("D", true))"#,
            ),
            (
                SYMBOLS_KICAD10.replace("(symbol \"D_0_1\"", "(symbol \"D_0_2\""),
                r#"symbol 'D': graphics (0, 1, "polyline"): Some(1) → None"#,
            ),
            (
                SYMBOLS_KICAD10.replace("(in_bom yes)", "(in_bom no)"),
                r#"symbol 'D': flag "in_bom": Some(true) → Some(false)"#,
            ),
            (
                SYMBOLS_KICAD10.replace("(in_bom yes)", "(in_bom yes) (pin_numbers (hide yes))"),
                "symbol 'D': pin numbers hidden false → true",
            ),
            (
                SYMBOLS_KICAD10.replace("(symbol \"D\"", "(symbol \"D2\""),
                "symbol 'D' lost",
            ),
        ] {
            let differences = content_differences(Kind::Symbol, SYMBOLS_KICAD9, &changed);
            assert!(
                differences
                    .iter()
                    .any(|difference| difference.starts_with(says)),
                "{says}: {differences:?}"
            );
        }

        for (changed, says) in [
            (
                FOOTPRINT_KICAD10.replace("(size 1.5 1.5)", "(size 1.4 1.5)"),
                "gained pad '1' (thru_hole circle) at (-1, 0) angle 0 size 1.4 x 1.5 drill 0.8 x 0.8 layers *.Cu *.Mask",
            ),
            (
                FOOTPRINT_KICAD10.replace("(remove_unused_layers no)", "(solder_mask_margin 0.05)"),
                "gained pad '1' (thru_hole circle) at (-1, 0) angle 0 size 1.5 x 1.5 drill 0.8 x 0.8 layers *.Cu *.Mask {\"solder_mask_margin\": \"50000\"}",
            ),
            (
                FOOTPRINT_KICAD10.replace("(attr through_hole)", "(attr through_hole) (clearance 0.2)"),
                r#"setting "clearance": None → Some("200000")"#,
            ),
            (
                FOOTPRINT_KICAD10.replace("(fp_line", "(fp_circle"),
                r#"graphics ("fp_line", "F.SilkS"): Some(1) → None"#,
            ),
            (
                FOOTPRINT_KICAD10.replace("\"REF**\"", "\"R**\""),
                r#"field "Reference""#,
            ),
            (
                FOOTPRINT_KICAD10.replace("(xyz 0 0 1.016)", "(xyz 0 0 1)"),
                "3D models",
            ),
            (
                FOOTPRINT_KICAD10.replace("(hide yes) (uuid \"e\")", "(uuid \"e\")"),
                "hidden text",
            ),
        ] {
            let differences = content_differences(Kind::Footprint, FOOTPRINT_OLD, &changed);
            assert!(
                differences.iter().any(|difference| difference.starts_with(says)),
                "{says}: {differences:?}"
            );
        }
    }

    #[test]
    fn pad_layers_read_as_kicad_writes_them() {
        let read = |layers: &[&str]| pad_layers(layers.iter().copied());
        let all = read(&["*.Cu", "*.Mask"]);
        assert_eq!(read(&["F&B.Cu", "*.Mask"]), all);
        assert_eq!(read(&["F.Cu", "B.Cu", "F.Mask", "B.Mask"]), all);
        assert_ne!(read(&["F.Cu", "B.Cu", "In1.Cu", "*.Mask"]), all);
        assert_ne!(read(&["*.Cu", "F.Mask"]), all);
        assert_eq!(
            read(&["F.Cu", "F.Paste", "B.Paste"]),
            read(&["F.Cu", "*.Paste"])
        );
    }

    async fn call(kicad_cli: &str, arguments: serde_json::Value) -> serde_json::Value {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            kicad_cli: kicad_cli.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 628, "method": "tools/call",
                "params": {"name": "upgrade_library", "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut body: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        if let Some(details) = response["content"][1]["text"].as_str() {
            body["details"] = serde_json::from_str(details).unwrap();
        }
        body["is_error"] = response["isError"].clone();
        body
    }

    /// A stand-in kicad-cli that copies `upgraded` (a file or a directory) to
    /// the output path, `$4` of `<sym|fp> upgrade -o OUTPUT INPUT`, and, with
    /// `touch_input`, then changes the input, as another program saving it
    /// would.
    fn copying_cli(dir: &Path, upgraded: &Path, touch_input: bool) -> String {
        let source = upgraded.display().to_string();
        let (unix_copy, windows_copy) = if upgraded.is_dir() {
            (
                format!("cp -r '{source}' \"$4\"\n"),
                format!("@xcopy /e /i /q /y \"{source}\" \"%~4\" >nul\r\n"),
            )
        } else {
            (
                format!("cp '{source}' \"$4\"\n"),
                format!("@copy /y \"{source}\" \"%~4\" >nul\r\n"),
            )
        };
        let (unix_touch, windows_touch) = if touch_input {
            ("printf ' ' >> \"$5\"\n", "@echo.>> \"%~5\"\r\n")
        } else {
            ("", "")
        };
        write_script(
            dir,
            "fake-kicad-cli",
            &format!("#!/bin/sh\n{unix_copy}{unix_touch}"),
            &format!("{windows_copy}{windows_touch}"),
        )
        .display()
        .to_string()
    }

    fn statuses(body: &serde_json::Value) -> Vec<(String, String)> {
        body["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| {
                (
                    file["file"].as_str().unwrap().to_string(),
                    file["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn a_dry_run_reports_and_an_apply_replaces_a_symbol_library() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("D.kicad_sym");
        std::fs::write(&library, SYMBOLS_KICAD9).unwrap();
        let upgraded = dir.path().join("upgraded.kicad_sym");
        std::fs::write(&upgraded, SYMBOLS_KICAD10).unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library})).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["status"], "ready", "{body}");
        assert_eq!(body["kind"], "symbol");
        assert_eq!(body["format_version"], 20251024);
        assert_eq!(body["counts"], json!({"upgraded": 1}));
        assert_eq!(
            body["files"],
            json!([{
                "file": "D.kicad_sym", "version": 20241209, "upgraded_version": 20251024,
                "status": "upgraded", "differences": []
            }])
        );
        assert_eq!(
            (&body["shown"], &body["truncated"]),
            (&json!(1), &json!(false))
        );
        assert!(
            body["warning"].as_str().unwrap().contains("cannot open"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD9);

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["status"], "applied", "{body}");
        assert_eq!(body["replaced"], 1);
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD10);
    }

    /// Only a footprint older than kicad-cli's format is replaced, in one
    /// transaction: kicad-cli resaves the whole library, and a current file is
    /// left as it was. A file named in upper case is one KiCad loads on
    /// Windows, and only there.
    #[tokio::test]
    async fn an_apply_replaces_only_the_older_footprints() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        let current = FOOTPRINT_KICAD10.replace("\"R_X\"", "\"R_Y\"");
        std::fs::write(library.join("R_X.kicad_mod"), FOOTPRINT_OLD).unwrap();
        std::fs::write(library.join("R_Y.kicad_mod"), &current).unwrap();
        let shouting = FOOTPRINT_OLD.replace("\"R_X\"", "\"R_Z\"");
        std::fs::write(library.join("R_Z.KICAD_MOD"), &shouting).unwrap();
        let upgraded = dir.path().join("upgraded.pretty");
        std::fs::create_dir(&upgraded).unwrap();
        std::fs::write(upgraded.join("R_X.kicad_mod"), FOOTPRINT_KICAD10).unwrap();
        // kicad-cli's resave of the current file, which is not written back.
        std::fs::write(
            upgraded.join("R_Y.kicad_mod"),
            current.replace("(uuid \"a\")", "(uuid \"z\")"),
        )
        .unwrap();
        let z_upgraded = FOOTPRINT_KICAD10.replace("\"R_X\"", "\"R_Z\"");
        std::fs::write(upgraded.join("R_Z.KICAD_MOD"), &z_upgraded).unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;

        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["status"], "applied", "{body}");
        assert_eq!(body["kind"], "footprint");
        let mut expected = vec![("R_X.kicad_mod".to_string(), "upgraded".to_string())];
        if cfg!(windows) {
            expected.push(("R_Z.KICAD_MOD".to_string(), "upgraded".to_string()));
        }
        expected.push(("R_Y.kicad_mod".to_string(), "current".to_string()));
        assert_eq!(statuses(&body), expected);
        let upgraded_count = if cfg!(windows) { 2 } else { 1 };
        assert_eq!(
            body["counts"],
            json!({"current": 1, "upgraded": upgraded_count})
        );
        assert_eq!(body["replaced"], upgraded_count);
        assert_eq!(
            std::fs::read_to_string(library.join("R_X.kicad_mod")).unwrap(),
            FOOTPRINT_KICAD10
        );
        assert_eq!(
            std::fs::read_to_string(library.join("R_Z.KICAD_MOD")).unwrap(),
            if cfg!(windows) { z_upgraded } else { shouting }
        );
        assert_eq!(
            std::fs::read_to_string(library.join("R_Y.kicad_mod")).unwrap(),
            current
        );
        let journals = std::fs::read_dir(&library)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".copperpour-transaction-")
            })
            .count();
        assert_eq!(journals, 0, "the transaction leaves no journal behind");
    }

    /// An upgrade that changes content is reported by a dry run as blocked
    /// and refused by an apply as plan_blocked, which writes nothing.
    #[tokio::test]
    async fn an_upgrade_that_changes_content_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("D.kicad_sym");
        std::fs::write(&library, SYMBOLS_KICAD9).unwrap();
        let upgraded = dir.path().join("upgraded.kicad_sym");
        std::fs::write(
            &upgraded,
            SYMBOLS_KICAD10.replace("(number \"2\"", "(number \"3\""),
        )
        .unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library})).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["status"], "blocked", "{body}");
        assert_eq!(body["files"][0]["status"], "differs", "{body}");
        assert_eq!(body["files"][0]["differences"].as_array().unwrap().len(), 2);

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;
        assert_eq!(body["is_error"], true, "{body}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert_eq!(body["error"]["operation"], "upgrade_library", "{body}");
        let reasons = body["error"]["reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 2, "{body}");
        assert!(
            reasons[0]
                .as_str()
                .unwrap()
                .starts_with("D.kicad_sym: symbol 'D': lost pin 2"),
            "{body}"
        );
        assert_eq!(body["details"]["status"], "blocked", "{body}");
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD9);
    }

    /// kicad-cli writes nothing for a library already in its format.
    #[tokio::test]
    async fn a_current_library_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("D.kicad_sym");
        std::fs::write(&library, SYMBOLS_KICAD10).unwrap();
        let cli = noop_cli(dir.path()).display().to_string();

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;

        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["status"], "current", "{body}");
        assert_eq!(body["files"][0]["status"], "current");
        assert!(body.get("warning").is_none(), "{body}");
        assert!(body.get("replaced").is_none(), "{body}");
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD10);
    }

    /// A library file another program saves after the read is refused as a
    /// conflict, every file is left as it was, and no journal stays behind.
    #[tokio::test]
    async fn a_library_saved_after_the_read_is_left_as_saved() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        let second = FOOTPRINT_OLD.replace("\"R_X\"", "\"R_Y\"");
        std::fs::write(library.join("R_X.kicad_mod"), FOOTPRINT_OLD).unwrap();
        std::fs::write(library.join("R_Y.kicad_mod"), &second).unwrap();
        let upgraded = dir.path().join("upgraded.pretty");
        std::fs::create_dir(&upgraded).unwrap();
        std::fs::write(upgraded.join("R_X.kicad_mod"), FOOTPRINT_KICAD10).unwrap();
        std::fs::write(
            upgraded.join("R_Y.kicad_mod"),
            FOOTPRINT_KICAD10.replace("\"R_X\"", "\"R_Y\""),
        )
        .unwrap();
        // The stand-in changes R_Y in the library while it upgrades, after
        // the read, then keeps a copy of what it was given to upgrade.
        let seen = dir.path().join("seen.pretty");
        let script = write_script(
            dir.path(),
            "fake-kicad-cli",
            &format!(
                "#!/bin/sh\ncp -r '{}' \"$4\"\nprintf ' ' >> '{}'\ncp -r \"$5\" '{}'\n",
                upgraded.display(),
                library.join("R_Y.kicad_mod").display(),
                seen.display()
            ),
            &format!(
                "@xcopy /e /i /q /y \"{}\" \"%~4\" >nul\r\n@echo.>> \"{}\"\r\n\
                 @xcopy /e /i /q /y \"%~5\" \"{}\" >nul\r\n",
                upgraded.display(),
                library.join("R_Y.kicad_mod").display(),
                seen.display()
            ),
        );

        let body = call(
            &script.display().to_string(),
            json!({"library_path": library, "dry_run": false}),
        )
        .await;

        assert_eq!(body["is_error"], true, "{body}");
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert!(
            body["error"]["paths"][0]
                .as_str()
                .unwrap()
                .ends_with("R_Y.kicad_mod"),
            "{body}"
        );
        assert_eq!(
            std::fs::read_to_string(seen.join("R_Y.kicad_mod")).unwrap(),
            second,
            "kicad-cli upgrades the text as read, not the later save"
        );
        assert_eq!(
            std::fs::read_to_string(library.join("R_X.kicad_mod")).unwrap(),
            FOOTPRINT_OLD,
            "nothing is written when one file changed"
        );
        assert_ne!(
            std::fs::read_to_string(library.join("R_Y.kicad_mod")).unwrap(),
            second
        );
        let leftovers = std::fs::read_dir(&library).unwrap().flatten().count();
        assert_eq!(leftovers, 2, "no journal stays behind");
    }

    /// A footprint whose name inside the file is not its file name is noted,
    /// not a difference: KiCad names a library footprint after its file.
    #[tokio::test]
    async fn a_name_that_is_not_the_file_name_is_noted() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        std::fs::write(
            library.join("R_X.kicad_mod"),
            FOOTPRINT_OLD.replacen("\"R_X\"", "\"Old name\"", 1),
        )
        .unwrap();
        let upgraded = dir.path().join("upgraded.pretty");
        std::fs::create_dir(&upgraded).unwrap();
        std::fs::write(upgraded.join("R_X.kicad_mod"), FOOTPRINT_KICAD10).unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library})).await;

        assert_eq!(body["status"], "ready", "{body}");
        assert_eq!(body["files"][0]["differences"], json!([]), "{body}");
        assert_eq!(
            body["files"][0]["notes"],
            json!([
                "the name inside the file, 'Old name', is not its file name; KiCad uses the \
                 file name, 'R_X', and an upgrade writes it"
            ]),
            "{body}"
        );
    }

    /// The list is limited, those that differ first; counts cover them all.
    /// A footprint kicad-cli wrote that the library lacks is a difference.
    #[tokio::test]
    async fn the_list_is_limited_and_a_made_up_footprint_differs() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        let upgraded = dir.path().join("upgraded.pretty");
        std::fs::create_dir(&upgraded).unwrap();
        for name in ["A", "B", "C"] {
            let old = FOOTPRINT_OLD.replace("\"R_X\"", &format!("\"{name}\""));
            let new = FOOTPRINT_KICAD10.replace("\"R_X\"", &format!("\"{name}\""));
            std::fs::write(library.join(format!("{name}.kicad_mod")), old).unwrap();
            std::fs::write(upgraded.join(format!("{name}.kicad_mod")), new).unwrap();
        }
        std::fs::write(upgraded.join("Extra.kicad_mod"), FOOTPRINT_KICAD10).unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library, "limit": 2})).await;

        assert_eq!(body["status"], "blocked", "{body}");
        assert_eq!(body["counts"], json!({"differs": 1, "upgraded": 3}));
        assert_eq!(
            (&body["shown"], &body["truncated"]),
            (&json!(2), &json!(true))
        );
        assert_eq!(body["files"].as_array().unwrap().len(), 2, "{body}");
        assert_eq!(body["files"][0]["file"], "Extra.kicad_mod", "{body}");
        assert_eq!(
            body["files"][0]["differences"],
            json!(["kicad-cli wrote a footprint the library does not have"])
        );
    }

    #[tokio::test]
    async fn a_library_kicad_cli_cannot_load_or_upgrade_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        std::fs::write(library.join("Good.kicad_mod"), FOOTPRINT_OLD).unwrap();
        let broken = format!(
            "(footprint \"Broken\"\n{}",
            "  (fp_line (start 0 0) (end 1 1) (layer \"F.SilkS\"))\n".repeat(200)
        );
        std::fs::write(library.join("Broken.kicad_mod"), &broken).unwrap();
        let cli = noop_cli(dir.path()).display().to_string();
        let body = call(&cli, json!({"library_path": library})).await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(reason.contains("Broken.kicad_mod"), "{reason}");
        assert!(
            reason.len() < 400,
            "the reason names where, not the file's text: {reason}"
        );

        std::fs::remove_file(library.join("Broken.kicad_mod")).unwrap();
        let failing = write_script(
            dir.path(),
            "failing-kicad-cli",
            "#!/bin/sh\necho 'Unable to load library' >&2\nexit 2\n",
            "@echo Unable to load library 1>&2\r\n@exit /b 2\r\n",
        );
        let body = call(
            &failing.display().to_string(),
            json!({"library_path": library, "dry_run": false}),
        )
        .await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(
            reason.starts_with("upgrade_library wrote nothing"),
            "{reason}"
        );
        assert!(reason.contains("Unable to load library"), "{reason}");
        assert_eq!(
            std::fs::read_to_string(library.join("Good.kicad_mod")).unwrap(),
            FOOTPRINT_OLD
        );
    }

    /// KiCad's loader also canonicalizes a mandatory field name in any case,
    /// reads ki_description as a hidden Description, drops an empty
    /// ki_keywords, reads alternates verbatim with the last of a name
    /// winning, and drops a corner ratio from a pad without corners.
    #[test]
    fn what_kicad_s_loader_normalizes_is_not_a_difference() {
        let old = r#"(kicad_symbol_lib (version 20211014)
  (symbol "A"
    (property "value" "A" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "ki_description" "Gate" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "ki_keywords" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "A_1_1"
      (pin input line (at 0 0 0) (length 2.54) (name "X") (number "1")
        (alternate "~" output line) (alternate "Y" input line) (alternate "Y" output line)))))"#;
        let new = r#"(kicad_symbol_lib (version 20251024)
  (symbol "A"
    (property "Value" "A" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Description" "Gate" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))
    (symbol "A_1_1"
      (pin input line (at 0 0 0) (length 2.54) (name "X") (number "1")
        (alternate "~" output line) (alternate "Y" output line)))))"#;
        assert_eq!(
            content_differences(Kind::Symbol, old, new),
            Vec::<String>::new()
        );

        // A body style 2 drawing in an older file is KiCad's De Morgan style,
        // which KiCad 10 declares; a declared name that changes differs.
        let old = r#"(kicad_symbol_lib (version 20241209)
  (symbol "G" (symbol "G_1_1") (symbol "G_1_2")))"#;
        let new = r#"(kicad_symbol_lib (version 20251024)
  (symbol "G" (body_styles demorgan) (symbol "G_1_1") (symbol "G_1_2")))"#;
        assert_eq!(
            content_differences(Kind::Symbol, old, new),
            Vec::<String>::new()
        );
        assert_ne!(
            content_differences(
                Kind::Symbol,
                &new.replace("demorgan", r#""A" "B""#),
                &new.replace("demorgan", r#""A" "C""#),
            ),
            Vec::<String>::new()
        );

        let with_ratio =
            FOOTPRINT_KICAD10.replace("(size 1.5 1.5)", "(size 1.5 1.5) (roundrect_rratio 0.25)");
        assert_eq!(
            content_differences(Kind::Footprint, &with_ratio, FOOTPRINT_KICAD10),
            Vec::<String>::new()
        );

        // KiCad's loader drops a copper layer's own thermal angle, which
        // KiCad 9 wrote and KiCad 10 does not; the layer's size still counts.
        let padstack = |angle: &str, size: &str| {
            FOOTPRINT_KICAD10.replace(
                "(size 1.5 1.5)",
                &format!(
                    "(size 1.5 1.5) (padstack (mode custom)                      (layer \"In1.Cu\" (shape oval) (size {size}){angle} (zone_connect -1)))"
                ),
            )
        };
        assert_eq!(
            content_differences(
                Kind::Footprint,
                &padstack(" (thermal_bridge_angle 45)", "1.9 1.1"),
                &padstack("", "1.9 1.1"),
            ),
            Vec::<String>::new()
        );
        assert_ne!(
            content_differences(
                Kind::Footprint,
                &padstack("", "1.9 1.1"),
                &padstack("", "1.9 1.2"),
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_bare_symbol_library_name_journals_in_the_working_directory() {
        assert_eq!(
            journal_directory(Path::new("R.kicad_sym"), Kind::Symbol),
            PathBuf::from(".")
        );
        assert_eq!(
            journal_directory(Path::new("lib/R.kicad_sym"), Kind::Symbol),
            PathBuf::from("lib")
        );
        assert_eq!(
            journal_directory(Path::new("lib.pretty"), Kind::Footprint),
            PathBuf::from("lib.pretty")
        );
    }

    /// An unfinished transaction in the library's directory could still
    /// change its files: a dry run refuses while it is pending, and an apply
    /// that cannot settle it refuses too, naming the CLI that inspects it.
    #[tokio::test]
    async fn an_unfinished_transaction_is_settled_or_refused_first() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("D.kicad_sym");
        std::fs::write(&library, SYMBOLS_KICAD9).unwrap();
        std::fs::write(
            dir.path().join(".copperpour-transaction-unreadable.json"),
            "{}",
        )
        .unwrap();
        let upgraded = dir.path().join("upgraded.kicad_sym");
        std::fs::write(&upgraded, SYMBOLS_KICAD10).unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        for dry_run in [true, false] {
            let body = call(&cli, json!({"library_path": library, "dry_run": dry_run})).await;
            assert_eq!(body["error"]["kind"], "conflict", "{dry_run}: {body}");
            assert!(
                body.to_string().contains("copperpour transaction status"),
                "{dry_run}: {body}"
            );
        }
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD9);

        // A transaction that can be completed: a dry run leaves it pending,
        // an apply completes it first and says so.
        std::fs::remove_file(dir.path().join(".copperpour-transaction-unreadable.json")).unwrap();
        let other = dir.path().join("other.txt");
        std::fs::write(&other, "before").unwrap();
        let journal = dir.path().join(".copperpour-transaction-left.json");
        let left = json!({
            "version": 1, "id": "left",
            "entries": [{ "path": "other.txt", "expected": "before", "replacement": "after" }]
        });
        std::fs::write(&journal, left.to_string()).unwrap();
        let body = call(&cli, json!({"library_path": library})).await;
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert!(body.to_string().contains("1 unfinished"), "{body}");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "before");
        assert!(journal.exists());

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;
        assert_eq!(body["status"], "applied", "{body}");
        assert_eq!(body["settled_transactions"], 1, "{body}");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "after");
        assert!(!journal.exists());
        assert_eq!(std::fs::read_to_string(&library).unwrap(), SYMBOLS_KICAD10);
    }

    /// A read-only file would stop the transaction after its journal, so it
    /// is refused before anything is written.
    #[tokio::test]
    async fn a_read_only_file_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.pretty");
        std::fs::create_dir(&library).unwrap();
        let second = FOOTPRINT_OLD.replace("\"R_X\"", "\"R_Y\"");
        std::fs::write(library.join("R_X.kicad_mod"), FOOTPRINT_OLD).unwrap();
        std::fs::write(library.join("R_Y.kicad_mod"), &second).unwrap();
        let locked = library.join("R_Y.kicad_mod");
        let mut permissions = std::fs::metadata(&locked).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&locked, permissions.clone()).unwrap();
        let upgraded = dir.path().join("upgraded.pretty");
        std::fs::create_dir(&upgraded).unwrap();
        std::fs::write(upgraded.join("R_X.kicad_mod"), FOOTPRINT_KICAD10).unwrap();
        std::fs::write(
            upgraded.join("R_Y.kicad_mod"),
            FOOTPRINT_KICAD10.replace("\"R_X\"", "\"R_Y\""),
        )
        .unwrap();
        let cli = copying_cli(dir.path(), &upgraded, false);

        let body = call(&cli, json!({"library_path": library, "dry_run": false})).await;

        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(reason.contains("read-only"), "{reason}");
        assert!(reason.contains("R_Y.kicad_mod"), "{reason}");
        assert_eq!(
            std::fs::read_to_string(library.join("R_X.kicad_mod")).unwrap(),
            FOOTPRINT_OLD
        );
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&locked, permissions).unwrap();
    }

    #[tokio::test]
    async fn a_path_that_is_no_library_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cli = noop_cli(dir.path()).display().to_string();
        let file = dir.path().join("board.kicad_pcb");
        std::fs::write(&file, "(kicad_pcb)").unwrap();
        let body = call(&cli, json!({"library_path": file})).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "library_path", "{body}");

        let missing = dir.path().join("missing.kicad_sym");
        let body = call(&cli, json!({"library_path": missing})).await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");

        let empty = dir.path().join("empty.pretty");
        std::fs::create_dir(&empty).unwrap();
        let body = call(&cli, json!({"library_path": empty})).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }

    /// With the real kicad-cli 10.0.8, KiCad 9's stock Device symbols and
    /// Resistor_SMD footprints upgrade with their content kept, and its
    /// Interface library is blocked: KiCad drops LTC1519's unit block for body
    /// style 3, which the symbol does not have, and the circle in it. Run with
    /// `--ignored` where KiCad 9 is installed beside KiCad 10.
    #[tokio::test]
    #[ignore = "needs KiCad 9's stock library and KiCad 10's kicad-cli"]
    async fn kicad_9_stock_libraries_through_the_real_kicad_cli() {
        let kicad9 = PathBuf::from(r"C:\Program Files\KiCad\9.0\share\kicad");
        let cli = r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe";
        let dir = tempfile::tempdir().unwrap();
        let device = dir.path().join("Device.kicad_sym");
        std::fs::copy(kicad9.join("symbols").join("Device.kicad_sym"), &device).unwrap();
        let resistors = dir.path().join("Resistor_SMD.pretty");
        copy_dir(
            &kicad9.join("footprints").join("Resistor_SMD.pretty"),
            &resistors,
        );
        let interface = dir.path().join("Interface.kicad_sym");
        std::fs::copy(
            kicad9.join("symbols").join("Interface.kicad_sym"),
            &interface,
        )
        .unwrap();

        for library in [&device, &resistors] {
            let body = call(cli, json!({"library_path": library, "limit": 0})).await;
            assert_eq!(body["status"], "ready", "{library:?}: {body}");
            assert!(
                body["counts"].get("differs").is_none(),
                "{library:?}: {body}"
            );
        }

        let body = call(cli, json!({"library_path": interface})).await;
        assert_eq!(body["status"], "blocked", "{body}");
        assert_eq!(
            body["files"][0]["differences"],
            json!([r#"symbol 'LTC1519': graphics (3, 3, "circle"): Some(1) → None"#]),
            "{body}"
        );
    }
}
