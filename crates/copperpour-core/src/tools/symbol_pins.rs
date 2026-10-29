//! `edit_symbol_pin`: change one pin's electrical type, graphic style, name,
//! number, position, angle or length in an existing `.kicad_sym` symbol,
//! leaving everything else as written.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::library::{skip_sexp_token, ALLOWED_PIN_ELECTRICAL_TYPES};
use crate::tools::sch_annotate::opt_string;
use crate::tools::symbol_fields::{header, symbol_block, unusable, write_checked};
use crate::tools::{invalid_arg, ToolContext, ToolDef};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::writer::{apply_edits, find_direct_child_blocks, read_consistent, SexpEdit};
use copperpour_sexp::SexpError;
use serde_json::json;
use std::path::Path;

/// KiCad's pin graphic styles, as `create_symbol` takes them.
const PIN_STYLES: &[&str] = &[
    "line",
    "inverted",
    "clock",
    "inverted_clock",
    "input_low",
    "clock_low",
    "output_low",
    "edge_clock_high",
    "non_logic",
];

/// The first library version in which a pin named or numbered `~` is not
/// empty (KiCad's `sch_file_versions.h`).
const TILDE_IS_TEXT: u32 = 20250318;

/// The pin directions KiCad reads in `(at x y angle)`; any other angle stops it
/// loading the library.
const PIN_ANGLES: [u32; 4] = [0, 90, 180, 270];

/// The parts of a pin a call can set, in the order [`Pin::parts`] gives them.
const PARTS: [&str; 8] = [
    "number", "type", "style", "name", "x", "y", "angle", "length",
];

pub(super) fn tool() -> ToolDef {
    tool!(
        "edit_symbol_pin",
        "Set the electrical type, graphic style, name, number, position, angle or length of \
         one pin of an existing library symbol. A pin number found in more than one unit or \
         body style needs unit or body_style to choose the pin. Everything else, and the rest \
         of the library, is left as written. Reports each changed part's previous and current \
         value.",
        json!({
            "type": "object",
            "properties": {
                "library_path": { "type": "string", "description": "Path to an existing .kicad_sym library file" },
                "symbol_name": { "type": "string", "description": "Name of the symbol to change" },
                "pin_number": { "type": "string", "description": "Number of the pin to change, as written in the symbol (for example '1' or 'A3')" },
                "unit": { "type": "integer", "minimum": 0, "description": "Unit holding the pin; 0 for a pin common to all units" },
                "body_style": { "type": "integer", "minimum": 0, "description": "Body style holding the pin: 1 for the first, 2 and up for alternates such as De Morgan, 0 for a pin common to all" },
                "type": {
                    "type": "string",
                    "enum": ALLOWED_PIN_ELECTRICAL_TYPES,
                    "description": "New pin electrical type"
                },
                "style": {
                    "type": "string",
                    "enum": PIN_STYLES,
                    "description": "New pin graphic style"
                },
                "name": { "type": "string", "description": "New pin name. KiCad keeps no space in a pin name, so a space is written as '_'" },
                "new_number": { "type": "string", "description": "New pin number, a space written as '_'. Refused when another pin of the symbol has it, in any unit, unless the two are in different body styles: the pins KiCad's symbol checker reports as duplicates" },
                "x": { "type": "number", "description": "New X of the pin's connection point in mm, in the symbol's coordinates as create_symbol writes them" },
                "y": { "type": "number", "description": "New Y of the pin's connection point in mm, as for x" },
                "angle": { "type": "integer", "enum": PIN_ANGLES, "description": "New pin direction in degrees, from the connection point toward the body" },
                "length": { "type": "number", "minimum": 0, "description": "New pin length in mm" }
            },
            "required": ["library_path", "symbol_name", "pin_number"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_edit_symbol_pin(args, ctx).await }
    )
}

/// KiCad's schematic unit, 0.0001 mm (`SCH_IU_PER_MM`, `include/base_units.h:72`).
const UNITS_PER_MM: f64 = 1e4;

/// The largest magnitude KiCad reads in its units, about 151848 mm; it clamps
/// beyond (`parseInternalUnits`, `eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr_parser.cpp:691-694`).
/// KiCad's factor is the literal 0.7071, not 1/√2.
const UNITS_LIMIT: f64 = i32::MAX as f64 * 7071.0 / 1e4;

/// Which pin a call names, and what it asks to set; a `None` value leaves
/// that part of the pin alone.
struct Request<'a> {
    symbol: &'a str,
    /// As KiCad reads a pin number, a space as '_'.
    number: String,
    unit: Option<u32>,
    body_style: Option<u32>,
    pin_type: Option<String>,
    style: Option<String>,
    name: Option<String>,
    new_number: Option<String>,
    /// `x`, `y` and `length` in KiCad's units.
    x: Option<i64>,
    y: Option<i64>,
    angle: Option<u32>,
    length: Option<i64>,
}

/// One `(pin TYPE STYLE (at X Y ANGLE) (length L) (name "N" …) (number "NUM" …))`
/// block: each part a call can set, as written, with its span. KiCad defaults
/// a missing `(at …)` or `(length …)`; such a part reads as empty and has no span.
#[derive(Debug, Clone, PartialEq)]
struct Pin {
    unit: u32,
    body_style: u32,
    /// As KiCad reads it: a space as '_', and, for a library before
    /// [`TILDE_IS_TEXT`], empty where the file says `~`; so is `name`.
    number: String,
    pin_type: String,
    style: String,
    name: String,
    x: String,
    y: String,
    angle: String,
    length: String,
    /// The spans of the parts, in [`PARTS`] order.
    spans: [Option<(usize, usize)>; 8],
}

impl Pin {
    fn place(&self) -> String {
        format!("unit {} body style {}", self.unit, self.body_style)
    }

    /// The parts a call can set, in [`PARTS`] order.
    fn parts(&self) -> [&str; 8] {
        [
            &self.number,
            &self.pin_type,
            &self.style,
            &self.name,
            &self.x,
            &self.y,
            &self.angle,
            &self.length,
        ]
    }

    /// Whether KiCad's symbol checker counts two pins with one number as
    /// duplicates: unless they are in different body styles, 0 sharing all
    /// (`eeschema/symbol_checker.cpp`, `CheckDuplicatePins`).
    fn shares_a_body_style(&self, other: &Pin) -> bool {
        self.body_style == 0 || other.body_style == 0 || self.body_style == other.body_style
    }
}

/// A length or coordinate written in mm, in KiCad's units as it reads them.
fn units_of(text: &str) -> Option<i64> {
    text.parse::<f64>()
        .ok()
        .map(|mm| (mm * UNITS_PER_MM).round() as i64)
}

/// KiCad's units as mm, written as `create_symbol` writes them.
fn mm_text(units: i64) -> String {
    fmt_f64(units as f64 / UNITS_PER_MM)
}

/// The whole number at the front of `text`, as KiCad reads a pin's angle
/// (`parseInt`, through `strtol`).
fn leading_integer(text: &str) -> Option<i64> {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let digits = unsigned.len()
        - unsigned
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    text[..text.len() - unsigned.len() + digits].parse().ok()
}

/// `pin`'s parts once `request` is applied, as [`prepare_edit`] writes them: a
/// part KiCad would already read as the asked value keeps its text.
fn parts_as_asked(pin: &Pin, request: &Request) -> [String; 8] {
    let measure = |new: Option<i64>, old: &str| match new {
        Some(new) if units_of(old) != Some(new) => mm_text(new),
        _ => old.to_string(),
    };
    let text = |new: &Option<String>, old: &str| new.clone().unwrap_or_else(|| old.to_string());
    [
        text(&request.new_number, &pin.number),
        text(&request.pin_type, &pin.pin_type),
        text(&request.style, &pin.style),
        text(&request.name, &pin.name),
        measure(request.x, &pin.x),
        measure(request.y, &pin.y),
        match request.angle {
            Some(angle) if leading_integer(&pin.angle) != Some(i64::from(angle)) => {
                angle.to_string()
            }
            _ => pin.angle.clone(),
        },
        measure(request.length, &pin.length),
    ]
}

/// One element of a pin number in KiCad's stacked notation: a number, or a
/// run `prefix first..=last` such as `A1-A3`.
#[derive(Debug, PartialEq)]
enum Stacked {
    One(String),
    Run(String, i64, i64),
}

/// A trailing run of digits and what comes before it, as KiCad's
/// `ParseAlphaNumericPin` splits a pin number.
fn alphanumeric(number: &str) -> (&str, Option<i64>) {
    let digits = number.len() - number.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (prefix, value) = number.split_at(number.len() - digits);
    (prefix, value.parse().ok())
}

/// A pin number as KiCad's `ExpandStackedPinNotation` reads it
/// (`common/string_utils.cpp:1672-1757`), its runs kept as runs so that a
/// long range costs nothing; anything else is the number itself.
fn stacked(number: &str) -> Vec<Stacked> {
    let whole = || vec![Stacked::One(number.to_string())];
    let Some(inner) = number
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return whole();
    };
    let mut parts = Vec::new();
    for part in inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((from, to)) = part.split_once('-') else {
            parts.push(Stacked::One(part.to_string()));
            continue;
        };
        match (alphanumeric(from.trim()), alphanumeric(to.trim())) {
            ((prefix, Some(first)), (end_prefix, Some(last)))
                if prefix == end_prefix && first <= last =>
            {
                parts.push(Stacked::Run(prefix.to_string(), first, last));
            }
            _ => return whole(),
        }
    }
    if parts.is_empty() {
        whole()
    } else {
        parts
    }
}

/// A number two pin numbers both stand for, written as KiCad writes one taken
/// from a run, or `None`.
fn shared_number(a: &[Stacked], b: &[Stacked]) -> Option<String> {
    let in_run = |run: &Stacked, number: &str| match (run, alphanumeric(number)) {
        (Stacked::Run(prefix, first, last), (own, Some(value))) => {
            own == prefix
                && (*first..=*last).contains(&value)
                && format!("{prefix}{value}") == number
        }
        _ => false,
    };
    a.iter()
        .flat_map(|x| b.iter().map(move |y| (x, y)))
        .find_map(|pair| match pair {
            (Stacked::One(m), Stacked::One(n)) => (m == n).then(|| m.clone()),
            (Stacked::One(n), run) | (run, Stacked::One(n)) => in_run(run, n).then(|| n.clone()),
            (Stacked::Run(p, f, l), Stacked::Run(q, g, m)) => {
                (p == q && f.max(g) <= l.min(m)).then(|| format!("{p}{}", f.max(g)))
            }
        })
}

fn enum_arg(
    args: &serde_json::Value,
    key: &str,
    allowed: &[&str],
) -> Result<Option<String>, CallToolResult> {
    let Some(value) = opt_string(args, key)? else {
        return Ok(None);
    };
    if allowed.contains(&value.as_str()) {
        Ok(Some(value))
    } else {
        Err(invalid_arg(
            key,
            &format!("'{value}' is not one of {}", allowed.join(", ")),
        ))
    }
}

/// A length or coordinate in mm, at least `minimum`, in KiCad's units as it
/// would read it; `None` when absent.
fn measure_arg(
    args: &serde_json::Value,
    key: &str,
    minimum: f64,
) -> Result<Option<i64>, CallToolResult> {
    let mm = match args.get(key) {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(value) => value
            .as_f64()
            .ok_or_else(|| invalid_arg(key, "must be a number of mm"))?,
    };
    if mm < minimum {
        Err(invalid_arg(key, &format!("must be at least {minimum}")))
    } else if !mm.is_finite() || (mm * UNITS_PER_MM).abs() > UNITS_LIMIT {
        Err(invalid_arg(key, "is beyond the 151848 mm KiCad reads"))
    } else {
        Ok(Some((mm * UNITS_PER_MM).round() as i64))
    }
}

fn parse_request(args: &serde_json::Value) -> Result<Request<'_>, CallToolResult> {
    let angle = super::opt_u32(args, "angle")?;
    if angle.is_some_and(|angle| !PIN_ANGLES.contains(&angle)) {
        return Err(invalid_arg(
            "angle",
            "must be 0, 90, 180 or 270, the directions KiCad reads",
        ));
    }
    let request = Request {
        symbol: super::require_str(args, "symbol_name")?,
        number: super::require_str(args, "pin_number")?.replace(' ', "_"),
        unit: super::opt_u32(args, "unit")?,
        body_style: super::opt_u32(args, "body_style")?,
        pin_type: enum_arg(args, "type", &ALLOWED_PIN_ELECTRICAL_TYPES)?,
        style: enum_arg(args, "style", PIN_STYLES)?,
        // As eeschema's SCH_PIN::SetName and SetNumber store them, so KiCad
        // reads back what is written.
        name: opt_string(args, "name")?.map(|name| name.replace(' ', "_")),
        new_number: opt_string(args, "new_number")?.map(|number| number.replace(' ', "_")),
        x: measure_arg(args, "x", f64::NEG_INFINITY)?,
        y: measure_arg(args, "y", f64::NEG_INFINITY)?,
        angle,
        length: measure_arg(args, "length", 0.0)?,
    };
    if request.pin_type.is_none()
        && request.style.is_none()
        && request.name.is_none()
        && request.new_number.is_none()
        && request.x.is_none()
        && request.y.is_none()
        && request.angle.is_none()
        && request.length.is_none()
    {
        return Err(invalid_arg(
            "pin_change",
            "supply at least one of type, style, name, new_number, x, y, angle or length",
        ));
    }
    Ok(request)
}

async fn handle_edit_symbol_pin(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = super::get_path(args, "library_path")?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sym") {
        return Ok(unusable(&path, "it does not end in .kicad_sym"));
    }
    let request = match parse_request(args) {
        Ok(request) => request,
        Err(error) => return Ok(error),
    };
    let source = match read_consistent(&path) {
        Ok(source) => source,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
                format!("Symbol library not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let (target, replacement, changes) = match prepare_edit(&path, &source, &request) {
        Ok(edit) => edit,
        Err(error) => return Ok(error),
    };

    if replacement != source {
        let check = |text: &str| readback_mismatch(text, &source, &request, &target);
        if let Some(refusal) = write_checked(&path, &source, replacement, "edit_symbol_pin", check)?
        {
            return Ok(refusal);
        }
    }

    Ok(CallToolResult::json(&json!({
        "success": true,
        "library_path": path.display().to_string(),
        "symbol": request.symbol,
        "pin_number": request.number,
        "unit": target.unit,
        "body_style": target.body_style,
        "changes": changes
    })))
}

/// The library's `(version N)`.
fn library_version(source: &str) -> Option<u32> {
    find_direct_child_blocks(source, "kicad_symbol_lib")
        .into_iter()
        .find_map(|(start, _)| match header(source, start) {
            Some(("version", version)) => version.parse().ok(),
            _ => None,
        })
}

/// Every pin of the symbol block at `start..end`, in file order: those of its
/// `NAME_<unit>_<body style>` units, and any written directly under the
/// symbol, which KiCad reads as unit 1, body style 1.
fn symbol_pins(source: &str, start: usize, end: usize) -> Result<Vec<Pin>, String> {
    let tilde_is_empty = library_version(source).is_some_and(|version| version < TILDE_IS_TEXT);
    let read =
        |pin_start, pin_end, place| read_pin(source, pin_start, pin_end, place, tilde_is_empty);
    let mut pins = Vec::new();
    for (child_start, child_end) in find_direct_child_blocks(&source[start..end], "symbol") {
        let (child_start, child_end) = (start + child_start, start + child_end);
        match header(source, child_start) {
            Some(("pin", _)) => pins.push(read(child_start, child_end, (1, 1))?),
            Some(("symbol", name)) => {
                let place = unit_and_body_style(name).ok_or_else(|| {
                    format!("it holds a unit named {name}, which KiCad would not read")
                })?;
                for (pin_start, pin_end) in
                    find_direct_child_blocks(&source[child_start..child_end], "symbol")
                {
                    let (pin_start, pin_end) = (child_start + pin_start, child_start + pin_end);
                    if header(source, pin_start).is_some_and(|(head, _)| head == "pin") {
                        pins.push(read(pin_start, pin_end, place)?);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(pins)
}

/// The unit and body style a `"NAME_<unit>_<body style>"` unit name carries.
fn unit_and_body_style(quoted: &str) -> Option<(u32, u32)> {
    let name = copperpour_sexp::parse_sexp(quoted).ok()?;
    let mut parts = name.as_str()?.rsplitn(3, '_');
    let body_style = parts.next()?.parse().ok()?;
    let unit = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((unit, body_style))
}

fn read_pin(
    source: &str,
    start: usize,
    end: usize,
    place: (u32, u32),
    tilde_is_empty: bool,
) -> Result<Pin, String> {
    const BAD: &str = "it holds a pin that does not read as KiCad writes one";
    let (_, head_end) = skip_sexp_token(source, start + 1).ok_or(BAD)?;
    let type_span = skip_sexp_token(source, head_end).ok_or(BAD)?;
    let style_span = skip_sexp_token(source, type_span.1).ok_or(BAD)?;
    let mut name = None;
    let mut number = None;
    let mut at = None;
    let mut length = None;
    for (child_start, _) in find_direct_child_blocks(&source[start..end], "pin") {
        let child_start = start + child_start;
        let (_, after_head) = skip_sexp_token(source, child_start + 1).ok_or(BAD)?;
        let first = skip_sexp_token(source, after_head).ok_or(BAD)?;
        // As SCH_PIN::SetName and SetNumber store what the parser hands them.
        let quoted = |span: (usize, usize)| -> Result<(String, (usize, usize)), String> {
            let text = copperpour_sexp::parse_sexp(&source[span.0..span.1]).map_err(|_| BAD)?;
            let mut text = text.as_str().ok_or(BAD)?.replace(' ', "_");
            if tilde_is_empty && text == "~" {
                text.clear();
            }
            Ok((text, span))
        };
        match header(source, child_start).map(|(head, _)| head) {
            Some("name") => name = Some(quoted(first)?),
            Some("number") => number = Some(quoted(first)?),
            Some("at") => {
                let second = skip_sexp_token(source, first.1).ok_or(BAD)?;
                let third = skip_sexp_token(source, second.1).ok_or(BAD)?;
                at = Some([first, second, third]);
            }
            Some("length") => length = Some(first),
            _ => {}
        }
    }
    let ((name, name_span), (number, number_span)) = name.zip(number).ok_or(BAD)?;
    let at = at.map_or([None; 3], |at| at.map(Some));
    let spans = [
        Some(number_span),
        Some(type_span),
        Some(style_span),
        Some(name_span),
        at[0],
        at[1],
        at[2],
        length,
    ];
    let text = |index: usize| {
        spans[index].map_or(String::new(), |(from, to)| source[from..to].to_string())
    };
    if (4..8).any(|index| spans[index].is_some() && text(index).parse::<f64>().is_err()) {
        return Err(BAD.to_string());
    }
    Ok(Pin {
        unit: place.0,
        body_style: place.1,
        number,
        pin_type: text(1),
        style: text(2),
        name,
        x: text(4),
        y: text(5),
        angle: text(6),
        length: text(7),
        spans,
    })
}

/// The symbol a derived symbol's `(extends "PARENT")` names.
fn extends(source: &str, start: usize, end: usize) -> Option<String> {
    find_direct_child_blocks(&source[start..end], "symbol")
        .into_iter()
        .find_map(
            |(child_start, _)| match header(source, start + child_start) {
                Some(("extends", parent)) => copperpour_sexp::parse_sexp(parent)
                    .ok()
                    .and_then(|parent| parent.as_str().map(str::to_owned)),
                _ => None,
            },
        )
}

/// The pins whose `part` (unit or body style) is `wanted`, else those common
/// to all (0), as KiCad draws a 0 item in every unit or body style.
fn in_place(pins: Vec<&Pin>, wanted: Option<u32>, part: fn(&Pin) -> u32) -> Vec<&Pin> {
    let Some(wanted) = wanted else {
        return pins;
    };
    let exact: Vec<&Pin> = pins
        .iter()
        .copied()
        .filter(|pin| part(pin) == wanted)
        .collect();
    if exact.is_empty() {
        pins.into_iter().filter(|pin| part(pin) == 0).collect()
    } else {
        exact
    }
}

/// The one pin the request names, refusing a number it does not narrow to one.
fn choose_pin<'p>(
    path: &Path,
    pins: &'p [Pin],
    request: &Request,
) -> Result<&'p Pin, CallToolResult> {
    let (symbol, number) = (request.symbol, request.number.as_str());
    let numbered: Vec<&Pin> = pins.iter().filter(|pin| pin.number == number).collect();
    if numbered.is_empty() {
        return Err(invalid_arg(
            "pin_number",
            &format!("symbol '{symbol}' has no pin numbered '{number}'"),
        ));
    }
    let places = |pins: &[&Pin]| pins.iter().map(|pin| pin.place()).collect::<Vec<_>>();
    let in_unit = in_place(numbered.clone(), request.unit, |pin| pin.unit);
    let chosen = in_place(in_unit.clone(), request.body_style, |pin| pin.body_style);
    match chosen.as_slice() {
        [pin] => Ok(pin),
        [] => Err(invalid_arg(
            if in_unit.is_empty() {
                "unit"
            } else {
                "body_style"
            },
            &format!(
                "pin '{number}' of symbol '{symbol}' is in {}",
                places(&numbered).join(", ")
            ),
        )),
        [first, rest @ ..] if rest.iter().all(|pin| pin.place() == first.place()) => Err(unusable(
            path,
            &format!(
                "symbol '{symbol}' has {} pins numbered '{number}' in {}, which \
                     edit_symbol_pin cannot tell apart",
                chosen.len(),
                first.place()
            ),
        )),
        _ => Err(CallToolResult::error_kind(
            ToolErrorKind::AmbiguousTarget {
                target: number.to_string(),
                candidates: places(&chosen),
            },
            format!(
                "Pin '{number}' of symbol '{symbol}' is in {}; give unit and body_style to \
                 choose one.",
                places(&chosen).join(", ")
            ),
        )),
    }
}

/// The chosen pin, the library with it changed, and what changed.
fn prepare_edit(
    path: &Path,
    source: &str,
    request: &Request,
) -> Result<(Pin, String, serde_json::Value), CallToolResult> {
    let root = source.len() - source.trim_start().len();
    if header(source, root).map(|(head, _)| head) != Some("kicad_symbol_lib") {
        return Err(unusable(path, "it is not a (kicad_symbol_lib ...) file"));
    }
    let (start, end) = symbol_block(source, request.symbol)?;
    if let Some(parent) = extends(source, start, end) {
        return Err(invalid_arg(
            "symbol_name",
            &format!(
                "symbol '{}' extends '{parent}', whose pins it uses; edit '{parent}'",
                request.symbol
            ),
        ));
    }
    let tilde_is_empty = library_version(source).is_some_and(|version| version < TILDE_IS_TEXT);
    for (key, value) in [("name", &request.name), ("new_number", &request.new_number)] {
        if tilde_is_empty && value.as_deref() == Some("~") {
            return Err(invalid_arg(
                key,
                "this library is older than version 20250318, so KiCad reads '~' in it as \
                 empty; give an empty string for none",
            ));
        }
    }
    let pins = symbol_pins(source, start, end).map_err(|reason| unusable(path, &reason))?;
    let pin = choose_pin(path, &pins, request)?.clone();

    if let Some(new) = request
        .new_number
        .as_ref()
        .filter(|new| **new != pin.number)
    {
        let wanted = stacked(new);
        let taken = pins
            .iter()
            .filter(|other| **other != pin && pin.shares_a_body_style(other))
            .find_map(|other| {
                shared_number(&wanted, &stacked(&other.number)).map(|shared| (other, shared))
            });
        if let Some((other, shared)) = taken {
            return Err(invalid_arg(
                "new_number",
                &format!(
                    "pin '{}' in {} of symbol '{}' already stands for number '{shared}', so \
                     KiCad's symbol checker would report the two as duplicates",
                    other.number,
                    other.place(),
                    request.symbol
                ),
            ));
        }
    }

    let mut edits = Vec::new();
    let mut changes = serde_json::Map::new();
    let asked = parts_as_asked(&pin, request);
    for (index, (old, new)) in pin.parts().into_iter().zip(&asked).enumerate() {
        if old == new {
            continue;
        }
        let key = if PARTS[index] == "number" {
            "new_number"
        } else {
            PARTS[index]
        };
        let Some((start, end)) = pin.spans[index] else {
            return Err(invalid_arg(
                key,
                &format!(
                    "pin '{}' in {} writes no {}, which KiCad then defaults; add it in KiCad \
                     before changing it",
                    pin.number,
                    pin.place(),
                    if key == "length" {
                        "(length …)"
                    } else {
                        "(at …)"
                    }
                ),
            ));
        };
        let quoted = matches!(PARTS[index], "number" | "name");
        edits.push(SexpEdit::replace(
            start,
            end,
            if quoted {
                quote_string(new)
            } else {
                new.clone()
            },
        ));
        // Measures and the angle are reported as numbers, as KiCad reads them.
        let value = |text: &str| match PARTS[index] {
            "x" | "y" | "length" => {
                units_of(text).map_or(json!(text), |units| json!(units as f64 / UNITS_PER_MM))
            }
            "angle" => leading_integer(text).map_or(json!(text), |angle| json!(angle)),
            _ => json!(text),
        };
        changes.insert(
            PARTS[index].into(),
            json!({"previous": value(old), "current": value(new)}),
        );
    }
    Ok((
        pin,
        apply_edits(source.to_string(), edits),
        serde_json::Value::Object(changes),
    ))
}

/// Why `written` is not `before` with only the chosen pin changed as asked,
/// or `None` when it is. Each pin's unit, body style and every part a call can
/// set are compared, in file order.
fn readback_mismatch(
    written: &str,
    before: &str,
    request: &Request,
    target: &Pin,
) -> Option<String> {
    let symbol = request.symbol;
    let pins_of = |text: &str| {
        let (start, end) = symbol_block(text, symbol).ok()?;
        symbol_pins(text, start, end).ok()
    };
    let Some(after) = pins_of(written) else {
        return Some(format!("symbol '{symbol}' no longer has readable pins"));
    };
    let Some(original) = pins_of(before) else {
        return Some(format!("symbol '{symbol}' had no readable pins"));
    };
    if after.len() != original.len() {
        return Some("the pin count changed".to_string());
    }
    for (was, now) in original.iter().zip(&after) {
        let expected = if was == target {
            parts_as_asked(was, request)
        } else {
            was.parts().map(str::to_string)
        };
        if now.unit != was.unit || now.body_style != was.body_style || now.parts() != expected {
            return Some(format!(
                "pin '{}' of {} does not read as asked",
                was.number,
                was.place()
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    /// KiCad's stock `74xx` library, read from the installed KiCad rather than
    /// committed, because KiCad's libraries are CC BY-SA, as `symbol_fields`'
    /// tests read theirs. Its 74LS00 has four gates, each drawn in two body
    /// styles whose pins carry opposite graphic styles, and a power unit common
    /// to both (`74LS00_5_0`); its 7400 extends 74LS00. `None`, and the test
    /// skips, without an installed KiCad or when the library has lost that shape.
    fn stock_74xx() -> Option<String> {
        let Some(dir) = super::super::symbol_fields::tests::installed_symbol_dir() else {
            eprintln!("SKIP: no installed KiCad symbol library");
            return None;
        };
        let path = dir.join("74xx.kicad_sym");
        let Ok(library) = std::fs::read_to_string(&path) else {
            eprintln!("SKIP: {} is not installed", path.display());
            return None;
        };
        let shape = [
            "(symbol \"74LS00_1_2\"",
            "(symbol \"74LS00_5_0\"",
            "(extends \"74LS00\")",
        ];
        if !shape.iter().all(|part| library.contains(part)) {
            eprintln!("SKIP: {} no longer draws 74LS00 so", path.display());
            return None;
        }
        Some(library)
    }

    async fn call(arguments: serde_json::Value) -> serde_json::Value {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 113, "method": "tools/call",
                "params": {"name": "edit_symbol_pin", "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut body: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        body["is_error"] = response["isError"].clone();
        body
    }

    fn library_with(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("74xx.kicad_sym");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    /// The index, old text and new text of each line of `a` that `b` changes;
    /// both must have as many lines.
    fn changed_lines<'t>(a: &'t str, b: &'t str) -> Vec<(usize, &'t str, &'t str)> {
        assert_eq!(a.lines().count(), b.lines().count());
        a.lines()
            .zip(b.lines())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(index, (a, b))| (index, a, b))
            .collect()
    }

    /// The indices of the `(pin …` and `(name …` lines of pin `number` in the
    /// unit block `unit`, found by text alone, apart from the tool's reading.
    fn pin_lines(library: &str, unit: &str, number: &str) -> (usize, usize) {
        let lines: Vec<&str> = library.lines().collect();
        let opens = format!("(symbol \"{unit}\"");
        let start = lines.iter().position(|line| line.trim() == opens).unwrap();
        let numbered = format!("(number \"{number}\"");
        let at = start
            + lines[start..]
                .iter()
                .position(|line| line.trim() == numbered)
                .unwrap();
        let pin = (start..at)
            .rev()
            .find(|&index| lines[index].trim_start().starts_with("(pin "))
            .unwrap();
        let name = (pin..at)
            .find(|&index| lines[index].trim_start().starts_with("(name "))
            .unwrap();
        (pin, name)
    }

    /// The index of the first line opening `(head …` in pin `number` of the
    /// unit block `unit`, by text alone.
    fn pin_line(library: &str, unit: &str, number: &str, head: &str) -> usize {
        let (pin, _) = pin_lines(library, unit, number);
        let opens = format!("({head} ");
        library
            .lines()
            .enumerate()
            .skip(pin)
            .find(|(_, line)| line.trim_start().starts_with(&opens))
            .unwrap()
            .0
    }

    /// `text` with line `index` replaced by `line`, line endings kept.
    fn with_line(text: &str, index: usize, line: &str) -> String {
        text.split_inclusive('\n')
            .enumerate()
            .map(|(at, old)| {
                if at == index {
                    format!("{line}{}", &old[old.trim_end_matches(['\r', '\n']).len()..])
                } else {
                    old.to_string()
                }
            })
            .collect()
    }

    /// With KiCad 10.0.6's library, `kicad-cli sym upgrade --force` of this
    /// call's output differs from the same resave of the untouched library in
    /// this one line: KiCad reads the edit as written.
    #[tokio::test]
    async fn a_rename_changes_that_pin_and_no_other_byte() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 1, "body_style": 1, "name": "A"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!((&body["unit"], &body["body_style"]), (&json!(1), &json!(1)));
        assert_eq!(
            body["changes"],
            json!({"name": {"previous": "", "current": "A"}})
        );
        let (_, name) = pin_lines(&stock, "74LS00_1_1", "1");
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            [(name, "\t\t\t\t(name \"\"", "\t\t\t\t(name \"A\"")]
        );
    }

    /// As above, KiCad 10.0.6's resave differs from its resave of the untouched
    /// library in these two lines, with the space written as `_`.
    #[tokio::test]
    async fn type_style_and_name_change_on_one_pin() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "3",
            "unit": 1, "body_style": 2, "type": "open_collector",
            "style": "inverted", "name": "~{Y} \"OC\""
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"],
            json!({
                "type": {"previous": "output", "current": "open_collector"},
                "style": {"previous": "line", "current": "inverted"},
                "name": {"previous": "", "current": "~{Y}_\"OC\""}
            })
        );
        let (pin, name) = pin_lines(&stock, "74LS00_1_2", "3");
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            [
                (
                    pin,
                    "\t\t\t(pin output line",
                    "\t\t\t(pin open_collector inverted"
                ),
                (
                    name,
                    "\t\t\t\t(name \"\"",
                    "\t\t\t\t(name \"~{Y}_\\\"OC\\\"\""
                )
            ]
        );
    }

    #[tokio::test]
    async fn a_number_in_one_place_needs_no_unit() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "14", "name": "VDD"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!((&body["unit"], &body["body_style"]), (&json!(5), &json!(0)));
        let (_, name) = pin_lines(&stock, "74LS00_5_0", "14");
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            [(name, "\t\t\t\t(name \"VCC\"", "\t\t\t\t(name \"VDD\"")]
        );
    }

    /// Pin 7 is in 74LS00_5_0, common to both body styles, so either names it.
    #[tokio::test]
    async fn a_pin_common_to_all_body_styles_is_in_each() {
        let Some(stock) = stock_74xx() else { return };
        let (_, name) = pin_lines(&stock, "74LS00_5_0", "7");
        for body_style in [0, 1, 2] {
            let (_dir, path) = library_with(&stock);
            let body = call(json!({
                "library_path": path, "symbol_name": "74LS00", "pin_number": "7",
                "unit": 5, "body_style": body_style, "name": "VSS"
            }))
            .await;
            assert_ne!(body["is_error"], true, "{body_style}: {body}");
            assert_eq!(body["body_style"], 0, "{body_style}: {body}");
            assert_eq!(
                changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
                [(name, "\t\t\t\t(name \"GND\"", "\t\t\t\t(name \"VSS\"")]
            );
        }
    }

    #[tokio::test]
    async fn a_pin_already_as_asked_writes_nothing() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "7",
            "type": "power_in"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changes"], json!({}));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stock);
    }

    /// Before version 20250318 KiCad reads a pin named `~` as unnamed.
    #[tokio::test]
    async fn an_older_library_s_tilde_is_an_empty_name() {
        let Some(stock) = stock_74xx() else { return };
        let version = stock
            .lines()
            .position(|line| line.starts_with("\t(version "))
            .unwrap();
        let (_, name) = pin_lines(&stock, "74LS00_1_1", "1");
        let older = with_line(
            &with_line(&stock, version, "\t(version 20231120)"),
            name,
            "\t\t\t\t(name \"~\"",
        );
        let (_dir, path) = library_with(&older);
        let pin_1 = json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 1, "body_style": 1
        });
        let mut unnamed = pin_1.clone();
        unnamed["name"] = json!("");
        let body = call(unnamed).await;
        assert_eq!(body["changes"], json!({}), "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), older);

        let mut named = pin_1;
        named["name"] = json!("A");
        let body = call(named).await;
        assert_eq!(
            body["changes"],
            json!({"name": {"previous": "", "current": "A"}}),
            "{body}"
        );
    }

    /// KiCad reads a pin written directly under a symbol as unit 1, body style 1.
    #[tokio::test]
    async fn a_pin_directly_under_the_symbol_is_unit_1() {
        const BARE: &str = "(kicad_symbol_lib\n\t(version 20251024)\n\t(symbol \"BARE\"\n\t\t(pin passive line\n\t\t\t(at 0 0 0)\n\t\t\t(length 2.54)\n\t\t\t(name \"P\")\n\t\t\t(number \"1\")\n\t\t)\n\t)\n)\n";
        let (_dir, path) = library_with(BARE);
        let body = call(json!({
            "library_path": path, "symbol_name": "BARE", "pin_number": "1",
            "unit": 1, "body_style": 1, "name": "Q"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            BARE.replace("(name \"P\")", "(name \"Q\")")
        );
    }

    #[tokio::test]
    async fn refusals_name_the_argument_and_leave_the_file_alone() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        for (arguments, field, says) in [
            (
                json!({"pin_number": "1", "unit": 2, "name": "A"}),
                "unit",
                "is in unit 1 body style 1, unit 1 body style 2",
            ),
            (
                json!({"pin_number": "1", "unit": 1, "body_style": 3, "name": "A"}),
                "body_style",
                "is in unit 1 body style 1, unit 1 body style 2",
            ),
            (
                json!({"pin_number": "99", "name": "A"}),
                "pin_number",
                "no pin numbered '99'",
            ),
            (
                json!({"symbol_name": "NOPE", "pin_number": "1", "name": "A"}),
                "symbol_name",
                "no symbol named 'NOPE'",
            ),
            (
                json!({"symbol_name": "7400", "pin_number": "1", "name": "A"}),
                "symbol_name",
                "extends '74LS00'",
            ),
            (
                json!({"pin_number": "7", "type": "sink"}),
                "type",
                "is not one of",
            ),
            (
                json!({"pin_number": "7", "style": "round"}),
                "style",
                "is not one of",
            ),
            (
                json!({"pin_number": "7", "angle": 45}),
                "angle",
                "is not one of",
            ),
            (
                json!({"pin_number": "7", "length": -1}),
                "length",
                "minimum",
            ),
            (json!({"pin_number": "7", "x": "far"}), "x", "not of type"),
            (
                json!({"pin_number": "7"}),
                "pin_change",
                "supply at least one",
            ),
        ] {
            let mut arguments = arguments;
            arguments["library_path"] = json!(path);
            if arguments.get("symbol_name").is_none() {
                arguments["symbol_name"] = json!("74LS00");
            }
            let body = call(arguments.clone()).await;
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{arguments}: {body}"
            );
            assert_eq!(body["error"]["field"], field, "{arguments}: {body}");
            assert!(body.to_string().contains(says), "{arguments}: {body}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), stock);
        }
    }

    /// KiCad 10.0.6 reads the renumbered pin. Its next save also moves the pin
    /// into number order (`SCH_PIN::compare`), as the tool does not.
    #[tokio::test]
    async fn a_renumber_changes_that_number_only() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 1, "body_style": 1, "new_number": "15"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"],
            json!({"number": {"previous": "1", "current": "15"}})
        );
        let number = pin_line(&stock, "74LS00_1_1", "1", "number");
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            [(number, "\t\t\t\t(number \"1\"", "\t\t\t\t(number \"15\"")]
        );

        // A space is written as '_', as KiCad's SCH_PIN::SetNumber stores it.
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "15",
            "new_number": "B 15"
        }))
        .await;
        assert_eq!(
            body["changes"],
            json!({"number": {"previous": "15", "current": "B_15"}}),
            "{body}"
        );
    }

    /// KiCad's symbol checker reports two pins with one number as duplicates
    /// across units, and across body styles where one is common to all; a
    /// stacked number counts as each number it stands for.
    #[tokio::test]
    async fn a_renumber_onto_another_pin_s_number_is_refused() {
        let Some(stock) = stock_74xx() else { return };
        let fourteen = pin_line(&stock, "74LS00_5_0", "14", "number");
        let stacked = with_line(&stock, fourteen, "\t\t\t\t(number \"[14-16]\"");
        for (library, new_number, holder) in [
            (stock.as_str(), "2", "pin '2' in unit 1 body style 1"),
            (stock.as_str(), "4", "pin '4' in unit 2 body style 1"),
            (stock.as_str(), "14", "pin '14' in unit 5 body style 0"),
            (
                stacked.as_str(),
                "15",
                "pin '[14-16]' in unit 5 body style 0",
            ),
        ] {
            let (_dir, path) = library_with(library);
            let body = call(json!({
                "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
                "unit": 1, "body_style": 1, "new_number": new_number
            }))
            .await;
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{new_number}: {body}"
            );
            assert_eq!(body["error"]["field"], "new_number", "{new_number}: {body}");
            assert!(body.to_string().contains(holder), "{new_number}: {body}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), library);
        }
    }

    /// The two drawings of a De Morgan pin may share a number: KiCad does not
    /// count pins in different body styles as duplicates.
    #[tokio::test]
    async fn a_number_in_the_other_body_style_is_not_a_duplicate() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        for body_style in [1, 2] {
            let body = call(json!({
                "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
                "unit": 1, "body_style": body_style, "new_number": "15"
            }))
            .await;
            assert_ne!(body["is_error"], true, "{body_style}: {body}");
        }
        let numbers = [("74LS00_1_1", "1"), ("74LS00_1_2", "1")]
            .map(|(unit, number)| pin_line(&stock, unit, number, "number"));
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            numbers.map(|line| (line, "\t\t\t\t(number \"1\"", "\t\t\t\t(number \"15\""))
        );
    }

    /// With KiCad 10.0.6's library, `kicad-cli sym upgrade --force` of this
    /// output differs from the same resave of the untouched library in exactly
    /// these two lines. A part already at the asked value is not reported.
    #[tokio::test]
    async fn position_angle_and_length_change_on_one_pin() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 1, "body_style": 1,
            "x": -10.16, "y": 2.54, "angle": 90, "length": 6.35
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"],
            json!({
                "x": {"previous": -7.62, "current": -10.16},
                "angle": {"previous": 0, "current": 90},
                "length": {"previous": 3.81, "current": 6.35}
            })
        );
        let at = pin_line(&stock, "74LS00_1_1", "1", "at");
        let length = pin_line(&stock, "74LS00_1_1", "1", "length");
        assert_eq!(
            changed_lines(&stock, &std::fs::read_to_string(&path).unwrap()),
            [
                (
                    at,
                    "\t\t\t\t(at -7.62 2.54 0)",
                    "\t\t\t\t(at -10.16 2.54 90)"
                ),
                (length, "\t\t\t\t(length 3.81)", "\t\t\t\t(length 6.35)")
            ]
        );
    }

    /// Pin 1 is drawn in both body styles of unit 1. Pin 4 renumbered 1 puts
    /// the number in unit 2 as well; pin 2 renumbered 1 puts it twice in each
    /// place of unit 1.
    #[tokio::test]
    async fn an_ambiguous_number_is_refused_until_a_place_names_one_pin() {
        let Some(stock) = stock_74xx() else { return };
        let several_units = stock.replace("(number \"4\"", "(number \"1\"");
        let twice_in_one = stock.replace("(number \"2\"", "(number \"1\"");
        for (library, place, candidates) in [
            (
                stock.as_str(),
                json!({}),
                json!(["unit 1 body style 1", "unit 1 body style 2"]),
            ),
            (
                stock.as_str(),
                json!({"unit": 1}),
                json!(["unit 1 body style 1", "unit 1 body style 2"]),
            ),
            (
                several_units.as_str(),
                json!({}),
                json!([
                    "unit 1 body style 1",
                    "unit 1 body style 2",
                    "unit 2 body style 1",
                    "unit 2 body style 2"
                ]),
            ),
        ] {
            let (_dir, path) = library_with(library);
            let mut arguments = place.clone();
            arguments["library_path"] = json!(path);
            arguments["symbol_name"] = json!("74LS00");
            arguments["pin_number"] = json!("1");
            arguments["name"] = json!("A");
            let body = call(arguments).await;
            assert_eq!(body["error"]["kind"], "ambiguous_target", "{place}: {body}");
            assert_eq!(body["error"]["candidates"], candidates, "{place}: {body}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), library);
        }

        let (_dir, path) = library_with(&several_units);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 2, "body_style": 1, "name": "A"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!((&body["unit"], &body["body_style"]), (&json!(2), &json!(1)));

        let (_dir, path) = library_with(&twice_in_one);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "1",
            "unit": 1, "body_style": 1, "name": "A"
        }))
        .await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert!(
            body.to_string()
                .contains("has 2 pins numbered '1' in unit 1 body style 1"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), twice_in_one);
    }

    #[tokio::test]
    async fn a_symbol_named_twice_is_left_alone() {
        let Some(stock) = stock_74xx() else { return };
        let duplicated = stock.replace("(symbol \"7400\"", "(symbol \"74LS00\"");
        let (_dir, path) = library_with(&duplicated);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "7", "name": "A"
        }))
        .await;
        assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), duplicated);
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_library_is_left_alone() {
        let board = "(kicad_pcb (version 20241229))\n";
        let (_dir, path) = library_with(board);
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "7", "name": "A"
        }))
        .await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), board);
        let dir = tempfile::tempdir().unwrap();
        let body = call(json!({
            "library_path": dir.path().join("missing.kicad_sym"),
            "symbol_name": "74LS00", "pin_number": "7", "name": "A"
        }))
        .await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    }

    #[tokio::test]
    async fn a_library_saved_after_the_read_is_left_as_saved() {
        let Some(stock) = stock_74xx() else { return };
        let (_dir, path) = library_with(&stock);
        let newer = stock.replace("(name \"VCC\"", "(name \"VCC1\"");
        crate::tools::write_hook::set(newer.clone());
        let body = call(json!({
            "library_path": path, "symbol_name": "74LS00", "pin_number": "7", "name": "VSS"
        }))
        .await;
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    // The tests below need no installed KiCad, so CI runs them.

    /// `create_symbol` output resaved by KiCad 10.0.6, committed for
    /// `symbol_fields`' tests. FIELDS_BARE_0_1 holds pin 1 (VCC, at -8.89 0 0)
    /// and pin 2 (GND, at 8.89 0 180), each 2.54 long.
    const OURS: &str = include_str!("../../tests/fixtures/symbol_fields_kicad10.kicad_sym");
    /// KiCad's stock Fiber_Optic library at 7.0.10, committed for
    /// `symbol_fields`' tests: version 20220914, each pin on one line.
    const KICAD7: &str = include_str!("../../tests/fixtures/fiber_optic_kicad7.kicad_sym");

    #[tokio::test]
    async fn renumber_move_and_resize_one_pin() {
        let (_dir, path) = library_with(OURS);
        let body = call(json!({
            "library_path": path, "symbol_name": "FIELDS_BARE", "pin_number": "1",
            "new_number": "3", "x": -10.16, "y": 2.54, "angle": 0, "length": 3.81
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"],
            json!({
                "number": {"previous": "1", "current": "3"},
                "x": {"previous": -8.89, "current": -10.16},
                "y": {"previous": 0.0, "current": 2.54},
                "length": {"previous": 2.54, "current": 3.81}
            })
        );
        let line = |head| pin_line(OURS, "FIELDS_BARE_0_1", "1", head);
        assert_eq!(
            changed_lines(OURS, &std::fs::read_to_string(&path).unwrap()),
            [
                (
                    line("at"),
                    "\t\t\t\t(at -8.89 0 0)",
                    "\t\t\t\t(at -10.16 2.54 0)"
                ),
                (
                    line("length"),
                    "\t\t\t\t(length 2.54)",
                    "\t\t\t\t(length 3.81)"
                ),
                (
                    line("number"),
                    "\t\t\t\t(number \"1\"",
                    "\t\t\t\t(number \"3\""
                )
            ]
        );
    }

    /// KiCad keeps 0.0001 mm, so a value it would read the same is no change,
    /// whatever its text; nor is the pin's own number.
    #[tokio::test]
    async fn what_kicad_reads_the_same_writes_nothing() {
        let at = pin_line(OURS, "FIELDS_BARE_0_1", "1", "at");
        let padded = with_line(OURS, at, "\t\t\t\t(at -8.890 0 0.0)");
        let (_dir, path) = library_with(&padded);
        let body = call(json!({
            "library_path": path, "symbol_name": "FIELDS_BARE", "pin_number": "1",
            "new_number": "1", "x": -8.89001, "y": -0.00001, "angle": 0, "length": 2.54
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changes"], json!({}));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), padded);
    }

    /// Pin 2's number is set in memory, pin 1 renumbered against it. A stacked
    /// number collides through any number it stands for, as KiCad expands it;
    /// a malformed one is taken whole, and a long range costs nothing.
    #[tokio::test]
    async fn a_renumber_collides_through_stacked_numbers() {
        let two = pin_line(OURS, "FIELDS_BARE_0_1", "2", "number");
        for (held, asked, shared) in [
            ("2", "2", Some("2")),
            ("[2-4]", "3", Some("3")),
            ("4", "[3-5]", Some("4")),
            ("[5-7]", "[6-9]", Some("6")),
            ("[A1-A3]", "A2", Some("A2")),
            ("[A1-A3]", "A02", None),
            ("[3-1]", "3", None),
            ("[1-99999999999]", "5", Some("5")),
            ("2", "[100-99999999999]", None),
        ] {
            let library = with_line(OURS, two, &format!("\t\t\t\t(number \"{held}\""));
            let (_dir, path) = library_with(&library);
            let body = call(json!({
                "library_path": path, "symbol_name": "FIELDS_BARE", "pin_number": "1",
                "new_number": asked
            }))
            .await;
            match shared {
                Some(shared) => {
                    assert_eq!(
                        body["error"]["field"], "new_number",
                        "{held} {asked}: {body}"
                    );
                    let says = format!("already stands for number '{shared}'");
                    assert!(body.to_string().contains(&says), "{held} {asked}: {body}");
                    assert_eq!(std::fs::read_to_string(&path).unwrap(), library);
                }
                None => assert_ne!(body["is_error"], true, "{held} {asked}: {body}"),
            }
        }
    }

    /// KiCad's own vectors, `qa/tests/eeschema/test_stacked_pin_conversion.cpp`.
    #[test]
    fn stacked_numbers_read_as_kicad_reads_them() {
        let one = |number: &str| Stacked::One(number.to_string());
        let run = |first, last| Stacked::Run(String::new(), first, last);
        assert_eq!(stacked("[1,2,3]"), [one("1"), one("2"), one("3")]);
        assert_eq!(stacked("[5-7]"), [run(5, 7)]);
        assert_eq!(stacked("[1,3,5-7]"), [one("1"), one("3"), run(5, 7)]);
        assert_eq!(stacked("1"), [one("1")]);
        assert_eq!(stacked("[1,2"), [one("[1,2")]);
    }

    /// KiCad 7 writes a pin's at, length and hide on one line.
    #[tokio::test]
    async fn a_single_line_pin_changes_in_place() {
        const OLD: &str = "(pin no_connect line (at -7.62 5.08 0) (length 2.54) hide";
        const NEW: &str = "(pin no_connect line (at -10.16 5.08 0) (length 5.08) hide";
        let (_dir, path) = library_with(KICAD7);
        let body = call(json!({
            "library_path": path, "symbol_name": "AFBR-1624Z", "pin_number": "2",
            "x": -10.16, "length": 5.08, "new_number": "9"
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let at = KICAD7.find(OLD).unwrap();
        let number = at + KICAD7[at..].find("(number \"2\"").unwrap();
        let mut expected = KICAD7.to_string();
        expected.replace_range(number..number + 11, "(number \"9\"");
        expected.replace_range(at..at + OLD.len(), NEW);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    /// The Fiber_Optic library is version 20220914, before '~' was text.
    #[tokio::test]
    async fn a_tilde_kicad_would_read_as_empty_is_refused() {
        let (_dir, path) = library_with(KICAD7);
        for field in ["name", "new_number"] {
            let mut arguments = json!({
                "library_path": path, "symbol_name": "AFBR-1624Z", "pin_number": "2"
            });
            arguments[field] = json!("~");
            let body = call(arguments).await;
            assert_eq!(body["error"]["field"], field, "{body}");
            assert!(
                body.to_string().contains("reads '~' in it as empty"),
                "{body}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), KICAD7);
    }

    /// KiCad defaults a pin's missing (length …); its other parts still change.
    #[tokio::test]
    async fn a_pin_without_a_length_takes_every_other_edit() {
        let length = pin_line(OURS, "FIELDS_BARE_0_1", "1", "length");
        let library = with_line(OURS, length, "");
        let (_dir, path) = library_with(&library);
        let body = call(json!({
            "library_path": path, "symbol_name": "FIELDS_BARE", "pin_number": "1",
            "length": 5.08
        }))
        .await;
        assert_eq!(body["error"]["field"], "length", "{body}");
        assert!(body.to_string().contains("writes no (length …)"), "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), library);
        let body = call(json!({
            "library_path": path, "symbol_name": "FIELDS_BARE", "pin_number": "1",
            "type": "passive"
        }))
        .await;
        assert_eq!(
            body["changes"],
            json!({"type": {"previous": "power_in", "current": "passive"}}),
            "{body}"
        );
    }

    /// The handler checks what the schema checks, for a call that reaches it
    /// directly.
    #[test]
    fn arguments_are_checked_without_the_schema() {
        let field_of = |error: CallToolResult| {
            let crate::mcp::protocol::ToolContent::Text { text } = &error.content[0] else {
                panic!("a refusal carries text");
            };
            serde_json::from_str::<serde_json::Value>(text).unwrap()["error"]["field"].clone()
        };
        for (extra, field) in [
            (json!({"angle": 45}), "angle"),
            (json!({"length": -1}), "length"),
            (json!({"x": "far"}), "x"),
            (json!({"y": 1e9}), "y"),
            (json!({"new_number": 5}), "new_number"),
            (json!({}), "pin_change"),
        ] {
            let mut arguments = json!({"symbol_name": "S", "pin_number": "1"});
            for (key, value) in extra.as_object().unwrap() {
                arguments[key] = value.clone();
            }
            let Err(error) = parse_request(&arguments) else {
                panic!("{arguments} was taken");
            };
            assert_eq!(field_of(error), field, "{arguments}");
        }
    }
}
