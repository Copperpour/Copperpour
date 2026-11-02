//! `rename_schematic_net_label`: rename placed net labels in place.
//!
//! Only each label's name string is rewritten, so it keeps its UUID, position,
//! rotation, effects and fields. KiCad joins nets by name as well as by wire,
//! so a new name can join two nets or break one apart. Either is refused unless
//! the caller allows it, and the check reads the whole hierarchy.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::library::skip_sexp_token;
use crate::tools::sch_annotate::opt_bool;
use crate::tools::sch_connectivity::{net_graph_for, NetRoot};
use crate::tools::sch_hierarchy::{commit_sheets, load_saved_hierarchy, SavedHierarchy};
use crate::tools::sch_wiring::{same_point, LABEL_TAGS};
use crate::tools::{
    get_path, invalid_arg, opt_str, require_f64, require_str, ToolContext, ToolDef,
};
use copperpour_sexp::schematic::{
    extract_lib_pins_for_unit, extract_power_symbol_labels, extract_symbol_instances,
    extract_wires, find_lib_symbol, parse_at, pin_endpoint, Label, LabelKind,
};
use copperpour_sexp::writer::{apply_edits, direct_children_with_tag, find_direct_child_blocks};
use copperpour_sexp::{parse_sexp, read_consistent, FileTransition, SexpEdit, SexpNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

const OPERATION: &str = "rename_schematic_net_label";

/// How many nets a refusal names before it counts the rest.
const SHOWN: usize = 8;

pub(super) fn tool() -> ToolDef {
    tool!(
        "rename_schematic_net_label",
        "Rename placed net labels in place: one label by uuid, or by net and position, or \
         every label of a net with scope. Only the name changes; each label keeps its uuid, \
         position, rotation, effects and fields. Refuses a rename that would join two nets \
         unless allow_merge, or break a connection unless allow_split, reading the whole \
         hierarchy. Edits the saved files: refused while KiCad holds a target or the \
         project's root schematic open. Reports the labels renamed, read back from the file.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                "uuid": { "type": "string", "description": "One label's uuid, as list_schematic_labels reports it. Given alone" },
                "net": { "type": "string", "description": "The current name, with x and y for one label, or with scope for every label of the name" },
                "x": { "type": "number", "description": "With net and y: the label's position in mm, as delete_schematic_net_label takes it" },
                "y": { "type": "number", "description": "With net and x: the label's position in mm" },
                "scope": {
                    "type": "string",
                    "enum": ["sheet", "hierarchy"],
                    "description": "With net alone: 'sheet' renames every label of the name in this file; 'hierarchy' also in every sheet file below it, with the sheet pins of the name there"
                },
                "new_name": { "type": "string", "description": "The new name. '/' is written as KiCad writes it, '{slash}', and line breaks are dropped. Bus names and text variables are refused" },
                "allow_merge": { "type": "boolean", "default": false, "description": "Rename even when the new name joins two nets" },
                "allow_split": { "type": "boolean", "default": false, "description": "Rename even when the rename breaks a connection KiCad made by name" }
            },
            "required": ["schematic", "new_name"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_rename_schematic_net_label(args, ctx).await }
    )
}

// ─── Names ────────────────────────────────────────────────────────────────────

/// A name as eeschema's label dialog stores it (`EscapeString` with
/// `CTX_NETNAME`, `common/string_utils.cpp:196-203`): `/` as `{slash}`, line
/// breaks dropped. Names are compared in this form, so a label written with a
/// raw `/` is the same name as one KiCad wrote.
fn stored_name(name: &str) -> String {
    let mut stored = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' => stored.push_str("{slash}"),
            '\n' | '\r' => {}
            other => stored.push(other),
        }
    }
    stored
}

/// The deepest brace nesting a name may have. `UnescapeString` recurses once a
/// level, an unclosed brace counting as one, so a deeper name is not followed.
const MAX_BRACE_DEPTH: usize = 16;

fn brace_depth(text: &str) -> usize {
    let mut depth = 0usize;
    let mut deepest = 0;
    for c in text.chars() {
        match c {
            '{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// KiCad's `UnescapeString` (`common/string_utils.cpp`): each `{token}` escape
/// back to its character. Formatting (`~{…}`, `_{…}`, `^{…}`) and variables
/// (`${…}`) are kept as written. A name nested deeper than
/// [`MAX_BRACE_DEPTH`] is returned as it is.
fn unescaped(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    if chars.len() <= 2 || brace_depth(source) > MAX_BRACE_DEPTH {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len());
    let mut ch = '\0';
    let mut i = 0;
    while i < chars.len() {
        let prev = ch;
        ch = chars[i];
        if ch != '{' {
            out.push(ch);
            i += 1;
            continue;
        }
        let mut token = String::new();
        let mut depth = 1;
        let mut terminated = false;
        i += 1;
        while i < chars.len() {
            ch = chars[i];
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
            }
            if depth <= 0 {
                terminated = true;
                break;
            }
            token.push(ch);
            i += 1;
        }
        let replacement = match token.as_str() {
            _ if !terminated => {
                out.push('{');
                out.push_str(&unescaped(&token));
                i += 1;
                continue;
            }
            _ if matches!(prev, '$' | '~' | '^' | '_') => None,
            "dblquote" => Some("\""),
            "quote" => Some("'"),
            "lt" => Some("<"),
            "gt" => Some(">"),
            "backslash" => Some("\\"),
            "slash" => Some("/"),
            "bar" => Some("|"),
            "comma" => Some(","),
            "colon" => Some(":"),
            "space" => Some(" "),
            "dollar" => Some("$"),
            "tab" => Some("\t"),
            "return" => Some("\n"),
            "brace" => Some("{"),
            _ => None,
        };
        match replacement {
            Some(text) => out.push_str(text),
            None => {
                out.push('{');
                out.push_str(&unescaped(&token));
                out.push('}');
            }
        }
        i += 1;
    }
    out
}

fn is_format_marker(c: char) -> bool {
    matches!(c, '_' | '^' | '~')
}

/// Whether `s[pos]` follows an odd run of backslashes.
fn is_escaped(s: &[char], pos: usize) -> bool {
    s[..pos].iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1
}

/// A vector bus such as `D[0..7]`: its members are `prefix`, an index from
/// `begin` to `end`, and `suffix`.
#[derive(Debug, PartialEq)]
struct BusVector {
    prefix: String,
    suffix: String,
    begin: i64,
    end: i64,
}

/// KiCad's `NET_SETTINGS::ParseBusVector`
/// (`common/project/net_settings.cpp:1186-1391`), on an unescaped name.
fn bus_vector(bus: &str) -> Option<BusVector> {
    let s: Vec<char> = bus.chars().collect();
    let len = s.len();
    let mut i = 0;
    let mut prefix: Vec<char> = Vec::new();
    let mut nesting = 0i32;
    let mut format_wraps_name = false;
    let mut in_quotes = false;
    while i < len {
        let c = s[i];
        if c == '"' && !is_escaped(&s, i) {
            in_quotes = !in_quotes;
        } else if in_quotes {
            if c == '\\' && i + 1 < len {
                i += 1;
            }
            prefix.push(s[i]);
        } else if c == '{' {
            if i == 0 || !is_format_marker(s[i - 1]) {
                return None;
            }
            nesting += 1;
            prefix.push('{');
        } else if c == '}' {
            nesting -= 1;
            prefix.push('}');
        } else if c == '\\' && i + 1 < len && s[i + 1] == ' ' {
            i += 1;
            prefix.push(' ');
        } else if c == ' ' || c == ']' {
            return None;
        } else if c == '[' {
            if nesting > 0 {
                if let Some(open) = prefix.iter().rposition(|&p| p == '{') {
                    if open > 0 && is_format_marker(prefix[open - 1]) {
                        if open == prefix.len() - 1 {
                            // `D_{[1..2]}`: the formatting decorates the range.
                            prefix.truncate(open - 1);
                        } else {
                            // `~{BE[0..3]}`: the formatting wraps the name.
                            format_wraps_name = true;
                        }
                    }
                }
            }
            break;
        } else {
            prefix.push(c);
        }
        i += 1;
    }

    i += 1;
    if i >= len {
        return None;
    }
    let mut digits = String::new();
    let mut begin = 0;
    while i < len {
        if s[i] == '.' && i + 1 < len && s[i + 1] == '.' {
            begin = digits.parse().unwrap_or(0);
            i += 2;
            break;
        }
        if !s[i].is_ascii_digit() {
            return None;
        }
        digits.push(s[i]);
        i += 1;
    }
    if i >= len {
        return None;
    }
    digits.clear();
    let mut end = 0;
    while i < len {
        if s[i] == ']' {
            end = digits.parse().unwrap_or(0);
            i += 1;
            break;
        }
        if !s[i].is_ascii_digit() {
            return None;
        }
        digits.push(s[i]);
        i += 1;
    }
    let mut suffix = String::new();
    while i < len {
        match s[i] {
            '}' => {
                nesting -= 1;
                if format_wraps_name {
                    suffix.push('}');
                }
            }
            c @ ('+' | '-' | 'P' | 'N') => suffix.push(c),
            _ => return None,
        }
        i += 1;
    }
    if nesting != 0 || begin == end {
        return None;
    }
    Some(BusVector {
        prefix: prefix.into_iter().collect(),
        suffix,
        begin: begin.min(end),
        end: begin.max(end),
    })
}

/// KiCad's `NET_SETTINGS::ParseBusGroup`
/// (`common/project/net_settings.cpp:1394-1583`), on an unescaped name: the
/// group's name and its members, each escaped as KiCad escapes them.
fn bus_group(group: &str) -> Option<(String, Vec<String>)> {
    let member = |text: &str| stored_name(&text.replace(' ', "\\ "));
    let s: Vec<char> = group.chars().collect();
    let len = s.len();
    let mut i = 0;
    let mut prefix = String::new();
    let mut nesting = 0i32;
    let mut in_quotes = false;
    while i < len {
        let c = s[i];
        if c == '"' && !is_escaped(&s, i) {
            in_quotes = !in_quotes;
        } else if in_quotes {
            if c == '\\' && i + 1 < len {
                i += 1;
            }
            prefix.push(s[i]);
        } else if c == '{' {
            if i == 0 || !is_format_marker(s[i - 1]) {
                break;
            }
            nesting += 1;
            prefix.push('{');
        } else if c == '}' {
            nesting -= 1;
            prefix.push('}');
        } else if c == '\\' && i + 1 < len && s[i + 1] == ' ' {
            i += 1;
            prefix.push(' ');
        } else if c == ' ' || c == '[' || c == ']' {
            return None;
        } else {
            prefix.push(c);
        }
        i += 1;
    }
    if nesting != 0 {
        return None;
    }

    i += 1;
    if i >= len {
        return None;
    }
    in_quotes = false;
    let mut text = String::new();
    let mut members = Vec::new();
    while i < len {
        let c = s[i];
        if c == '"' && !is_escaped(&s, i) {
            in_quotes = !in_quotes;
        } else if in_quotes {
            if c == '\\' && i + 1 < len {
                i += 1;
            }
            text.push(s[i]);
        } else if c == '{' {
            if !is_format_marker(s[i - 1]) {
                return None;
            }
            nesting += 1;
            text.push('{');
        } else if c == '}' {
            if nesting == 0 {
                if !text.is_empty() {
                    members.push(member(&text));
                }
                return Some((prefix, members));
            }
            nesting -= 1;
            text.push('}');
        } else if c == '\\' && i + 1 < len && s[i + 1] == ' ' {
            i += 1;
            text.push(' ');
        } else if c == ' ' || c == ',' {
            if !text.is_empty() {
                members.push(member(&text));
            }
            text.clear();
        } else {
            text.push(c);
        }
        i += 1;
    }
    None
}

/// KiCad's `SCH_CONNECTION::IsBusLabel`, which reads the unescaped name.
fn is_bus_name(name: &str) -> bool {
    let text = unescaped(name);
    bus_vector(&text).is_some() || bus_group(&text).is_some()
}

/// Whether the bus label `bus` carries a member named `target` (a stored
/// name), its members named as `SCH_CONNECTION::ConfigureFromLabel`
/// (`eeschema/sch_connection.cpp:140-205`) names them: a vector's as
/// `prefix`, index, `suffix`; a named group's as `NAME.` and the member; a
/// member that names a bus alias by the alias's members. KiCad builds a
/// vector's members and a group's prefix from the unescaped name; here both
/// are compared in stored form, which can only find more members, never fewer.
fn bus_carries(bus: &str, target: &str, aliases: &BTreeMap<String, Vec<String>>) -> bool {
    member_carries(bus, "", target, aliases, 0)
}

fn member_carries(
    label: &str,
    prefix: &str,
    target: &str,
    aliases: &BTreeMap<String, Vec<String>>,
    depth: usize,
) -> bool {
    if depth > 8 {
        return false;
    }
    let text = unescaped(label);
    if let Some(vector) = bus_vector(&text) {
        let head = stored_name(&format!("{prefix}{}", vector.prefix));
        let tail = stored_name(&vector.suffix);
        let Some(index) = target
            .strip_prefix(head.as_str())
            .and_then(|rest| rest.strip_suffix(tail.as_str()))
        else {
            return false;
        };
        let canonical = !index.is_empty()
            && index.bytes().all(|b| b.is_ascii_digit())
            && (index == "0" || !index.starts_with('0'));
        return canonical
            && index
                .parse::<i64>()
                .is_ok_and(|n| (vector.begin..=vector.end).contains(&n));
    }
    if let Some((name, members)) = bus_group(&text) {
        // A group's members take the group's own prefix, as SetPrefix sets it.
        let prefix = if name.is_empty() {
            String::new()
        } else {
            format!("{name}.")
        };
        return members.iter().any(|member| match aliases.get(member) {
            Some(alias) => alias
                .iter()
                .any(|m| member_carries(&stored_name(m), &prefix, target, aliases, depth + 1)),
            None => member_carries(member, &prefix, target, aliases, depth + 1),
        });
    }
    stored_name(&format!("{prefix}{label}")) == target
}

/// The bus aliases KiCad 10 loads: `schematic.bus_aliases` in the project's
/// `.kicad_pro` (`common/project/project_file.cpp:119-160`), and the
/// `(bus_alias "NAME" (members …))` blocks a sheet saved before KiCad 10 keeps
/// (`eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr_parser.cpp:5106-5150`).
fn bus_aliases(
    project: Option<&Path>,
    sheets: &[&SexpNode],
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut aliases = BTreeMap::new();
    let mut add = |name: &str, members: Vec<&str>| {
        let members: Vec<String> = members
            .into_iter()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .collect();
        aliases.insert(name.trim().to_string(), members);
    };
    for sheet in sheets {
        for alias in sheet.find_all("bus_alias") {
            if let Some(name) = alias.get(1).and_then(SexpNode::as_str) {
                let members = alias
                    .find("members")
                    .and_then(SexpNode::children)
                    .unwrap_or_default()
                    .iter()
                    .skip(1)
                    .filter_map(SexpNode::as_str)
                    .collect();
                add(name, members);
            }
        }
    }
    if let Some(project) = project {
        let text = std::fs::read_to_string(project).map_err(|e| e.to_string())?;
        let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(entries) = json["schematic"]["bus_aliases"].as_object() {
            for (name, members) in entries {
                let members = members
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                add(name, members);
            }
        }
    }
    Ok(aliases)
}

// ─── What a sheet file holds ──────────────────────────────────────────────────

/// One label as it stands in its file.
struct PlacedLabel {
    tag: &'static str,
    /// As written, decoded from its quoted string.
    name: String,
    uuid: Option<String>,
    x: f64,
    y: f64,
    /// Byte range of the quoted name in the file.
    name_span: (usize, usize),
    segment: NetRoot,
}

struct PlacedPin {
    name: String,
    /// Whether its shape is `output`, which KiCad prefers when it chooses
    /// between sheet pins.
    output: bool,
    x: f64,
    y: f64,
    name_span: (usize, usize),
    segment: NetRoot,
}

struct SheetBlock {
    uuid: String,
    name: String,
    pins: Vec<PlacedPin>,
}

/// A power symbol's net-naming pin, global unless its symbol is `(power local)`;
/// or a hidden `power_in` pin of another symbol, which is always global.
struct PowerPin {
    name: String,
    global: bool,
    hidden: bool,
    segment: NetRoot,
}

/// The labels, power pins and sheet blocks of one file, each on the
/// wire-connected segment it sits on.
struct SheetContents {
    labels: Vec<PlacedLabel>,
    powers: Vec<PowerPin>,
    sheets: Vec<SheetBlock>,
}

fn read_contents(content: &str, tree: &SexpNode) -> Result<SheetContents, String> {
    let mut labels = Vec::new();
    let mut sheets = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_sch") {
        let block = &content[start..end];
        let node = parse_sexp(block).map_err(|e| e.to_string())?;
        let Some(head) = node.head() else { continue };
        if let Some(tag) = LABEL_TAGS.into_iter().find(|tag| *tag == head) {
            let (Some(name), Some((x, y, _))) =
                (node.get(1).and_then(SexpNode::as_str), parse_at(&node))
            else {
                return Err(format!("a {tag} at byte {start} has no name or position"));
            };
            let name_span = skip_sexp_token(content, start + 1 + tag.len())
                .ok_or_else(|| format!("the {tag} '{name}' has an unreadable name"))?;
            labels.push(PlacedLabel {
                tag,
                name: name.to_string(),
                uuid: node.find_str("uuid").map(str::to_string),
                x,
                y,
                name_span,
                segment: (0, 0),
            });
        } else if head == "sheet" {
            let mut pins = Vec::new();
            for (pin_start, pin_end) in direct_children_with_tag(block, "sheet", "pin") {
                let pin = parse_sexp(&block[pin_start..pin_end]).map_err(|e| e.to_string())?;
                let (Some(name), Some((x, y, _))) =
                    (pin.get(1).and_then(SexpNode::as_str), parse_at(&pin))
                else {
                    return Err(format!(
                        "a sheet pin at byte {} has no name or position",
                        start + pin_start
                    ));
                };
                let name_span = skip_sexp_token(content, start + pin_start + "(pin".len())
                    .ok_or_else(|| format!("the sheet pin '{name}' has an unreadable name"))?;
                pins.push(PlacedPin {
                    name: name.to_string(),
                    output: pin.get(2).and_then(SexpNode::as_str) == Some("output"),
                    x,
                    y,
                    name_span,
                    segment: (0, 0),
                });
            }
            let name = node
                .find_all("property")
                .into_iter()
                .find(|p| p.get(1).and_then(SexpNode::as_str) == Some("Sheetname"))
                .and_then(|p| p.get(2).and_then(SexpNode::as_str))
                .unwrap_or_default()
                .to_string();
            sheets.push(SheetBlock {
                uuid: node.find_str("uuid").unwrap_or_default().to_string(),
                name,
                pins,
            });
        }
    }

    // A power symbol whose definition says `(power local)` names a net on its
    // own sheet only; `(power)` and `(power global)` name it everywhere.
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|n| n.find_all("symbol"))
        .unwrap_or_default();
    let instances = extract_symbol_instances(tree);
    let local_power: HashSet<&str> = instances
        .iter()
        .filter(|inst| {
            find_lib_symbol(&lib_symbols, inst)
                .and_then(|sym| sym.find("power"))
                .and_then(|power| power.get(1).and_then(SexpNode::as_str))
                == Some("local")
        })
        .filter_map(|inst| inst.uuid.as_deref())
        .collect();
    let power_labels = extract_power_symbol_labels(tree);
    // A hidden `power_in` pin of a symbol that is not a power symbol names a
    // global net by the pin's name (`SCH_PIN::IsGlobalPower`,
    // `eeschema/sch_pin.cpp:357-373`; `SCH_PIN::GetDefaultNetName`, `:1484-1504`).
    let hidden_pins: Vec<(String, f64, f64)> = instances
        .iter()
        .filter_map(|inst| Some((inst, find_lib_symbol(&lib_symbols, inst)?)))
        .filter(|(_, sym)| sym.find("power").is_none())
        .flat_map(|(inst, sym)| {
            let t = inst.pin_transform();
            extract_lib_pins_for_unit(sym, inst.unit)
                .into_iter()
                .filter(|pin| pin.hidden && pin.electrical_type == "power_in")
                .map(move |pin| {
                    let (x, y) = pin_endpoint(&pin, t);
                    (stored_name(&pin.name), x, y)
                })
        })
        .collect();

    // Wires and junctions alone join segments here: every item goes into the
    // graph under a name of its own, so no two are joined by name.
    let items: Vec<Label> = labels
        .iter()
        .map(|l| (l.x, l.y))
        .chain(power_labels.iter().map(|p| (p.x, p.y)))
        .chain(hidden_pins.iter().map(|&(_, x, y)| (x, y)))
        .enumerate()
        .map(|(i, (x, y))| Label {
            kind: LabelKind::NetLabel,
            net: format!("\u{1}{i}"),
            x,
            y,
            rotation: 0.0,
            uuid: None,
        })
        .collect();
    let mut graph = net_graph_for(tree, &extract_wires(tree), &items);
    for label in &mut labels {
        label.segment = graph.root_at(label.x, label.y);
    }
    for sheet in &mut sheets {
        for pin in &mut sheet.pins {
            pin.segment = graph.root_at(pin.x, pin.y);
        }
    }
    let mut powers: Vec<PowerPin> = power_labels
        .iter()
        .map(|p| PowerPin {
            name: stored_name(&p.net),
            global: !p.uuid.as_deref().is_some_and(|u| local_power.contains(u)),
            hidden: false,
            segment: graph.root_at(p.x, p.y),
        })
        .collect();
    for (name, x, y) in hidden_pins {
        powers.push(PowerPin {
            name,
            global: true,
            hidden: true,
            segment: graph.root_at(x, y),
        });
    }
    Ok(SheetContents {
        labels,
        powers,
        sheets,
    })
}

// ─── Joins by name across the hierarchy ───────────────────────────────────────

/// One sheet instance: its file, where its parent places it, and its sheet
/// path as KiCad names it (`/`, `/Child/`).
struct Instance {
    file: usize,
    /// The parent instance and the index of the sheet block placing this one.
    parent: Option<(usize, usize)>,
    path: String,
}

/// The names one side of the rename gives: every label's and every sheet pin's.
struct Naming<'a> {
    contents: &'a [SheetContents],
    /// (file, label index) renamed to the new name.
    labels: &'a HashSet<(usize, usize)>,
    /// (file, sheet index, pin index) renamed to the new name.
    pins: &'a HashSet<(usize, usize, usize)>,
    new_name: &'a str,
    after: bool,
}

impl Naming<'_> {
    fn label(&self, file: usize, index: usize) -> String {
        if self.after && self.labels.contains(&(file, index)) {
            self.new_name.to_string()
        } else {
            stored_name(&self.contents[file].labels[index].name)
        }
    }

    fn pin(&self, file: usize, sheet: usize, index: usize) -> String {
        if self.after && self.pins.contains(&(file, sheet, index)) {
            self.new_name.to_string()
        } else {
            stored_name(&self.contents[file].sheets[sheet].pins[index].name)
        }
    }
}

/// Every (instance, segment) that carries a label, a power pin or a sheet pin.
struct Nodes {
    index: HashMap<(usize, NetRoot), usize>,
    keys: Vec<(usize, NetRoot)>,
}

impl Nodes {
    fn new(contents: &[SheetContents], instances: &[Instance]) -> Self {
        let mut nodes = Nodes {
            index: HashMap::new(),
            keys: Vec::new(),
        };
        for (i, instance) in instances.iter().enumerate() {
            let sheet = &contents[instance.file];
            let segments = sheet
                .labels
                .iter()
                .map(|l| l.segment)
                .chain(sheet.powers.iter().map(|p| p.segment))
                .chain(
                    sheet
                        .sheets
                        .iter()
                        .flat_map(|s| s.pins.iter().map(|p| p.segment)),
                );
            for segment in segments {
                nodes.index.entry((i, segment)).or_insert_with(|| {
                    nodes.keys.push((i, segment));
                    nodes.keys.len() - 1
                });
            }
        }
        nodes
    }

    fn of(&self, instance: usize, segment: NetRoot) -> usize {
        self.index[&(instance, segment)]
    }
}

fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        parent[b] = a;
    }
}

/// KiCad's driver priorities (`CONNECTION_SUBGRAPH::PRIORITY`,
/// `eeschema/connection_graph.h`), for the drivers a name comes from.
const SHEET_PIN: u8 = 2;
const HIER_LABEL: u8 = 3;
const LOCAL_LABEL: u8 = 4;
const LOCAL_POWER_PIN: u8 = 5;
const GLOBAL_POWER_PIN: u8 = 6;
const GLOBAL_LABEL: u8 = 7;

/// One named driver on a wire.
struct Driver {
    name: String,
    priority: u8,
    /// A sheet pin whose shape is `output`.
    output: bool,
    /// A hidden power pin of a symbol that is not a power symbol.
    hidden: bool,
}

/// The driver KiCad chooses to name a net (`CONNECTION_SUBGRAPH::ResolveDrivers`,
/// `eeschema/connection_graph.cpp:134-262`): the highest priority; between
/// power pins, a power symbol's; between sheet pins, an output one; then a
/// name without `-Pad`; then the lower name.
fn chosen<'d>(drivers: impl IntoIterator<Item = &'d Driver>) -> &'d Driver {
    drivers
        .into_iter()
        .max_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.hidden.cmp(&a.hidden))
                .then_with(|| a.output.cmp(&b.output))
                .then_with(|| b.name.contains("-Pad").cmp(&a.name.contains("-Pad")))
                .then_with(|| b.name.cmp(&a.name))
        })
        .expect("a net is listed only with a driver")
}

/// Each wire of a file with the drivers on it, under `naming`: its labels and
/// power pins, or, on a wire with neither, its sheet pins.
fn wire_drivers(
    sheet: &SheetContents,
    file: usize,
    naming: &Naming,
) -> BTreeMap<NetRoot, Vec<Driver>> {
    let mut wires: BTreeMap<NetRoot, Vec<Driver>> = BTreeMap::new();
    for (k, label) in sheet.labels.iter().enumerate() {
        let priority = match label.tag {
            "global_label" => GLOBAL_LABEL,
            "hierarchical_label" => HIER_LABEL,
            _ => LOCAL_LABEL,
        };
        wires.entry(label.segment).or_default().push(Driver {
            name: naming.label(file, k),
            priority,
            output: false,
            hidden: false,
        });
    }
    for power in &sheet.powers {
        wires.entry(power.segment).or_default().push(Driver {
            name: power.name.clone(),
            priority: if power.global {
                GLOBAL_POWER_PIN
            } else {
                LOCAL_POWER_PIN
            },
            output: false,
            hidden: power.hidden,
        });
    }
    let mut pin_wires: BTreeMap<NetRoot, Vec<Driver>> = BTreeMap::new();
    for (s, block) in sheet.sheets.iter().enumerate() {
        for (k, pin) in block.pins.iter().enumerate() {
            if !wires.contains_key(&pin.segment) {
                pin_wires.entry(pin.segment).or_default().push(Driver {
                    name: naming.pin(file, s, k),
                    priority: SHEET_PIN,
                    output: pin.output,
                    hidden: false,
                });
            }
        }
    }
    wires.extend(pin_wires);
    wires
}

/// One net of a sheet instance after the same-sheet pass: its wires' nodes and
/// every driver on them.
struct SheetNet<'w> {
    nodes: Vec<usize>,
    drivers: Vec<&'w Driver>,
}

/// Which nodes KiCad makes one net under `naming` (`eeschema/connection_graph.cpp`):
/// - on one sheet, wires that share a name, as the name each chose or another
///   (`CONNECTION_GRAPH::processSubGraphs`, `:2148-2299`). A wire only sheet
///   pins name takes part only when no other wire on the sheet is chosen by its
///   name: KiCad renames it apart, or leaves it weak beside a global of the
///   name (`:2036-2107`). Of two such wires with one name KiCad keeps the one
///   it reaches first; the check keeps neither;
/// - across sheets, among the nets of that pass whose chosen driver, chosen
///   again over the whole net (`:2302-2317`), is global: nets that share a
///   global name (`CONNECTION_GRAPH::buildConnectionGraph`, `:2487-2508`), and
///   a net with every such net carrying, in any kind, a global name it did not
///   choose (`:2547-2586`). A local name it did not choose joins only on its
///   own sheet, which the same-sheet pass has done;
/// - a sheet pin with the hierarchical labels of its name in the sheet it
///   places (`propagateToNeighbors`).
fn net_classes(
    contents: &[SheetContents],
    instances: &[Instance],
    nodes: &Nodes,
    naming: &Naming,
) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..nodes.keys.len()).collect();
    let wires: Vec<BTreeMap<NetRoot, Vec<Driver>>> = contents
        .iter()
        .enumerate()
        .map(|(file, sheet)| wire_drivers(sheet, file, naming))
        .collect();

    let mut nets: Vec<SheetNet> = Vec::new();
    for (i, instance) in instances.iter().enumerate() {
        let file = instance.file;
        let mut chosen_by: HashMap<&str, usize> = HashMap::new();
        for drivers in wires[file].values() {
            *chosen_by.entry(chosen(drivers).name.as_str()).or_default() += 1;
        }
        let taking_part: Vec<(usize, &Vec<Driver>)> = wires[file]
            .iter()
            .filter(|(_, drivers)| {
                let pick = chosen(drivers.iter());
                pick.priority > SHEET_PIN || chosen_by[pick.name.as_str()] == 1
            })
            .map(|(segment, drivers)| (nodes.of(i, *segment), drivers))
            .collect();
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for &(node, drivers) in &taking_part {
            for driver in drivers {
                let anchor = *by_name.entry(driver.name.as_str()).or_insert(node);
                union(&mut parent, anchor, node);
            }
        }
        let mut grouped: BTreeMap<usize, SheetNet> = BTreeMap::new();
        for &(node, drivers) in &taking_part {
            let net = grouped
                .entry(find(&mut parent, node))
                .or_insert_with(|| SheetNet {
                    nodes: Vec::new(),
                    drivers: Vec::new(),
                });
            net.nodes.push(node);
            net.drivers.extend(drivers.iter());
        }
        nets.extend(grouped.into_values());
    }

    let global: Vec<&SheetNet> = nets
        .iter()
        .filter(|net| chosen(net.drivers.iter().copied()).priority >= GLOBAL_POWER_PIN)
        .collect();
    let mut by_global_name: HashMap<&str, usize> = HashMap::new();
    let mut by_any_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for net in &global {
        for driver in &net.drivers {
            if driver.priority >= GLOBAL_POWER_PIN {
                let anchor = *by_global_name
                    .entry(driver.name.as_str())
                    .or_insert(net.nodes[0]);
                union(&mut parent, anchor, net.nodes[0]);
            }
            by_any_name
                .entry(driver.name.as_str())
                .or_default()
                .push(net.nodes[0]);
        }
    }
    for net in &global {
        let pick = chosen(net.drivers.iter().copied());
        for driver in &net.drivers {
            if driver.name != pick.name && driver.priority >= GLOBAL_POWER_PIN {
                for &other in &by_any_name[driver.name.as_str()] {
                    union(&mut parent, net.nodes[0], other);
                }
            }
        }
    }

    for (i, instance) in instances.iter().enumerate() {
        let Some((up, block)) = instance.parent else {
            continue;
        };
        let file = instance.file;
        let up_file = instances[up].file;
        for (k, pin) in contents[up_file].sheets[block].pins.iter().enumerate() {
            let pin_name = naming.pin(up_file, block, k);
            let pin_node = nodes.of(up, pin.segment);
            for (m, label) in contents[file].labels.iter().enumerate() {
                if label.tag == "hierarchical_label" && naming.label(file, m) == pin_name {
                    union(&mut parent, pin_node, nodes.of(i, label.segment));
                }
            }
        }
    }
    (0..parent.len()).map(|n| find(&mut parent, n)).collect()
}

/// A net described by what names it, under `naming`: each name with the sheet
/// it is on, a sheet pin marked as one.
fn describe(
    members: &[usize],
    contents: &[SheetContents],
    instances: &[Instance],
    nodes: &Nodes,
    naming: &Naming,
) -> String {
    let mut names = BTreeSet::new();
    for &node in members {
        let (i, segment) = nodes.keys[node];
        let instance = &instances[i];
        let sheet = &contents[instance.file];
        for (k, label) in sheet.labels.iter().enumerate() {
            if label.segment == segment {
                names.insert(format!(
                    "'{}' on {}",
                    naming.label(instance.file, k),
                    instance.path
                ));
            }
        }
        for power in sheet.powers.iter().filter(|p| p.segment == segment) {
            names.insert(format!("'{}' on {}", power.name, instance.path));
        }
        for (s, block) in sheet.sheets.iter().enumerate() {
            for (k, pin) in block.pins.iter().enumerate() {
                if pin.segment == segment {
                    names.insert(format!(
                        "sheet pin '{}' on {}",
                        naming.pin(instance.file, s, k),
                        instance.path
                    ));
                }
            }
        }
    }
    let shown: Vec<String> = names.iter().take(4).cloned().collect();
    match names.len().saturating_sub(4) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// The nets a rename joins and the nets it splits: each as the descriptions of
/// the nets joined, or of the parts a net falls into.
fn joins_and_splits(
    contents: &[SheetContents],
    instances: &[Instance],
    before: &Naming,
    after: &Naming,
) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    let nodes = Nodes::new(contents, instances);
    let was = net_classes(contents, instances, &nodes, before);
    let now = net_classes(contents, instances, &nodes, after);
    let group = |by: &[usize], within: &[usize]| {
        let mut parts: BTreeMap<usize, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
        for node in 0..nodes.keys.len() {
            parts
                .entry(by[node])
                .or_default()
                .entry(within[node])
                .or_default()
                .push(node);
        }
        parts
            .into_values()
            .filter(|parts| parts.len() > 1)
            .map(|parts| parts.into_values().collect::<Vec<_>>())
            .collect::<Vec<_>>()
    };
    let merges = group(&now, &was)
        .into_iter()
        .map(|nets| {
            nets.iter()
                .map(|net| describe(net, contents, instances, &nodes, before))
                .collect()
        })
        .collect();
    let splits = group(&was, &now)
        .into_iter()
        .map(|parts| {
            parts
                .iter()
                .map(|part| describe(part, contents, instances, &nodes, after))
                .collect()
        })
        .collect();
    (merges, splits)
}

// ─── The tool ─────────────────────────────────────────────────────────────────

/// Which labels a call names.
enum Selection {
    Uuid(String),
    At { net: String, x: f64, y: f64 },
    Scope { net: String, hierarchy: bool },
}

fn selection(args: &Value) -> Result<Selection, CallToolResult> {
    let given = |key: &str| !args[key].is_null();
    if let Some(uuid) = opt_str(args, "uuid") {
        if ["net", "x", "y", "scope"].iter().any(|key| given(key)) {
            return Err(invalid_arg(
                "uuid",
                "is given alone; net with x and y, or net with scope, choose labels instead",
            ));
        }
        return Ok(Selection::Uuid(uuid.to_string()));
    }
    let net = require_str(args, "net")
        .map_err(|_| invalid_arg("net", "give uuid, or net with x and y, or net with scope"))?;
    let net = checked_name("net", net)?;
    if let Some(scope) = opt_str(args, "scope") {
        if given("x") || given("y") {
            return Err(invalid_arg(
                "scope",
                "goes with net alone; x and y choose one label",
            ));
        }
        return match scope {
            "sheet" | "hierarchy" => Ok(Selection::Scope {
                net,
                hierarchy: scope == "hierarchy",
            }),
            other => Err(invalid_arg(
                "scope",
                &format!("must be 'sheet' or 'hierarchy', got '{other}'"),
            )),
        };
    }
    if given("scope") {
        return Err(invalid_arg("scope", "must be 'sheet' or 'hierarchy'"));
    }
    if !given("x") && !given("y") {
        return Err(invalid_arg(
            "net",
            "goes with x and y for one label, or with scope for every label of the name",
        ));
    }
    Ok(Selection::At {
        net,
        x: require_f64(args, "x")?,
        y: require_f64(args, "y")?,
    })
}

/// `name` in stored form, refused when it is empty, a bus name or holds a text
/// variable: a bus's members, and a variable's value, are not in the file.
fn checked_name(field: &str, name: &str) -> Result<String, CallToolResult> {
    let stored = stored_name(name);
    if stored.is_empty() {
        return Err(invalid_arg(field, "is empty once line breaks are dropped"));
    }
    if brace_depth(&stored) > MAX_BRACE_DEPTH {
        return Err(invalid_arg(
            field,
            &format!("nests braces more than {MAX_BRACE_DEPTH} deep"),
        ));
    }
    if stored.contains("${") {
        return Err(invalid_arg(
            field,
            &format!("'{stored}' holds a text variable, which KiCad resolves per sheet; it is not renamed"),
        ));
    }
    if is_bus_name(&stored) {
        return Err(invalid_arg(
            field,
            &format!("'{stored}' is a bus name; this tool renames net labels only"),
        ));
    }
    Ok(stored)
}

/// The renamed labels and sheet pins as read back, as the reply lists them.
struct Written {
    labels: Vec<Value>,
    sheet_pins: Vec<Value>,
}

/// The hierarchy a call reads: each file, what it holds, and each instance.
struct Design {
    root: PathBuf,
    project: Option<PathBuf>,
    hierarchy: SavedHierarchy,
    contents: Vec<SheetContents>,
    instances: Vec<Instance>,
}

impl Design {
    /// The hierarchy that owns `schematic`, read from its root, and the index
    /// of `schematic` among its files.
    fn load(schematic: &Path) -> Result<(Design, usize), CallToolResult> {
        let ownership = crate::tools::resolve_schematic_ownership(schematic)
            .map_err(|error| error.into_tool_result())?;
        let (root, project) = match ownership {
            Some(o) => (o.root_schematic, Some(o.project_file)),
            None => (schematic.to_path_buf(), None),
        };
        let hierarchy = load_saved_hierarchy(&root).map_err(|error| error.into_tool_result())?;
        let canonical = schematic.canonicalize().map_err(|error| {
            CallToolResult::error(format!("Cannot read '{}': {error}", schematic.display()))
        })?;
        let given = hierarchy
            .files
            .iter()
            .position(|f| f.canonical == canonical)
            .ok_or_else(|| {
                CallToolResult::error(format!(
                    "'{}' is not a sheet of the hierarchy under '{}'",
                    schematic.display(),
                    root.display()
                ))
            })?;
        let mut contents = Vec::with_capacity(hierarchy.files.len());
        for file in &hierarchy.files {
            contents.push(read_contents(&file.content, &file.tree).map_err(|reason| {
                CallToolResult::error(format!(
                    "Cannot read the labels of '{}': {reason}",
                    file.path.display()
                ))
            })?);
        }
        let instances = instances_of(&hierarchy, &contents);
        Ok((
            Design {
                root,
                project,
                hierarchy,
                contents,
                instances,
            },
            given,
        ))
    }

    fn path(&self, file: usize) -> String {
        self.hierarchy.files[file].path.display().to_string()
    }

    /// The files a call reaches: `given`, and under 'hierarchy' every file an
    /// instance of it places, at any depth.
    fn reached(&self, given: usize, hierarchy: bool) -> Vec<usize> {
        let mut reached = vec![given];
        if !hierarchy {
            return reached;
        }
        let instances = &self.hierarchy.instances;
        let tops: Vec<&str> = instances
            .iter()
            .filter(|i| i.file == given)
            .map(|i| i.path.as_str())
            .collect();
        for instance in instances {
            let below = tops.iter().any(|top| {
                instance
                    .path
                    .strip_prefix(top)
                    .is_some_and(|rest| rest.starts_with('/'))
            });
            if below && !reached.contains(&instance.file) {
                reached.push(instance.file);
            }
        }
        reached
    }

    /// The labels `selection` names, as (file, label index), and their name.
    fn select(
        &self,
        selection: &Selection,
        given: usize,
        reached: &[usize],
    ) -> Result<(String, HashSet<(usize, usize)>), CallToolResult> {
        let labels = &self.contents[given].labels;
        match selection {
            Selection::Uuid(uuid) => {
                let k = labels
                    .iter()
                    .position(|l| l.uuid.as_deref() == Some(uuid))
                    .ok_or_else(|| {
                        CallToolResult::error(format!(
                            "No label with uuid '{uuid}' in this schematic"
                        ))
                    })?;
                Ok((
                    checked_name("uuid", &labels[k].name)?,
                    HashSet::from([(given, k)]),
                ))
            }
            Selection::At { net, x, y } => {
                let named: Vec<usize> = (0..labels.len())
                    .filter(|&k| stored_name(&labels[k].name) == *net)
                    .collect();
                if named.is_empty() {
                    return Err(CallToolResult::error(format!(
                        "No label named '{net}' in this schematic"
                    )));
                }
                let here: Vec<usize> = named
                    .iter()
                    .copied()
                    .filter(|&k| same_point(labels[k].x, *x) && same_point(labels[k].y, *y))
                    .collect();
                match here.as_slice() {
                    [k] => Ok((net.clone(), HashSet::from([(given, *k)]))),
                    [] => {
                        let found: Vec<String> = named
                            .iter()
                            .map(|&k| format!("{} at ({}, {})", labels[k].tag, labels[k].x, labels[k].y))
                            .collect();
                        Err(CallToolResult::error(format!(
                            "No label '{net}' at ({x}, {y}). Found {} label(s) named '{net}': {}",
                            named.len(),
                            found.join("; ")
                        )))
                    }
                    several => Err(CallToolResult::error_kind(
                        ToolErrorKind::AmbiguousTarget {
                            target: format!("label '{net}' at ({x}, {y})"),
                            candidates: several
                                .iter()
                                .map(|&k| labels[k].uuid.clone().unwrap_or_default())
                                .collect(),
                        },
                        format!(
                            "{} labels named '{net}' share ({x}, {y}); give the uuid of the one to rename",
                            several.len()
                        ),
                    )),
                }
            }
            Selection::Scope { net, .. } => {
                let renamed: HashSet<(usize, usize)> = reached
                    .iter()
                    .flat_map(|&file| {
                        self.contents[file]
                            .labels
                            .iter()
                            .enumerate()
                            .filter(|(_, label)| stored_name(&label.name) == *net)
                            .map(move |(k, _)| (file, k))
                    })
                    .collect();
                if renamed.is_empty() {
                    let place = if reached.len() > 1 {
                        "this schematic or the sheets below it"
                    } else {
                        "this schematic"
                    };
                    return Err(CallToolResult::error(format!(
                        "No label named '{net}' in {place}"
                    )));
                }
                Ok((net.clone(), renamed))
            }
        }
    }

    /// The sheet pins named `name` in `files`, as (file, sheet, pin index).
    fn pins_named(&self, files: &[usize], name: &str) -> HashSet<(usize, usize, usize)> {
        let mut pins = HashSet::new();
        for &file in files {
            for (s, sheet) in self.contents[file].sheets.iter().enumerate() {
                for (k, pin) in sheet.pins.iter().enumerate() {
                    if stored_name(&pin.name) == name {
                        pins.insert((file, s, k));
                    }
                }
            }
        }
        pins
    }

    /// A refusal for each bus on a sheet of `renamed` that carries one of
    /// `names` as a member: a bus joins its members by name, and this tool does
    /// not follow a bus.
    fn bus_refusals(
        &self,
        renamed: &HashSet<(usize, usize)>,
        names: [&str; 2],
    ) -> Result<Vec<String>, CallToolResult> {
        let files: BTreeSet<usize> = renamed.iter().map(|&(file, _)| file).collect();
        let mut aliases = None;
        let mut reasons = Vec::new();
        for file in files {
            for bus in self.contents[file]
                .labels
                .iter()
                .filter(|l| is_bus_name(&l.name))
            {
                if aliases.is_none() {
                    let sheets: Vec<&SexpNode> =
                        self.hierarchy.files.iter().map(|f| &f.tree).collect();
                    aliases = Some(bus_aliases(self.project.as_deref(), &sheets).map_err(
                        |reason| {
                            CallToolResult::error(format!(
                                "Cannot read the project's bus aliases: {reason}"
                            ))
                        },
                    )?);
                }
                let aliases = aliases.as_ref().expect("read above");
                for name in names {
                    if bus_carries(&bus.name, name, aliases) {
                        reasons.push(format!(
                            "the bus '{}' in {} carries '{name}' as a member; this tool does not rename a bus's members",
                            bus.name,
                            self.path(file)
                        ));
                    }
                }
            }
        }
        Ok(reasons)
    }

    /// Write each name in one transaction, then read each renamed label and
    /// sheet pin back: the new name, where it stood. Their entries for the
    /// reply.
    fn write(
        &self,
        renamed: &HashSet<(usize, usize)>,
        pins: &HashSet<(usize, usize, usize)>,
        new_name: &str,
    ) -> anyhow::Result<Result<Written, CallToolResult>> {
        let quoted = quote_string(new_name);
        let mut edits: BTreeMap<usize, Vec<SexpEdit>> = BTreeMap::new();
        let spans = renamed
            .iter()
            .map(|&(file, k)| (file, self.contents[file].labels[k].name_span))
            .chain(
                pins.iter()
                    .map(|&(file, s, k)| (file, self.contents[file].sheets[s].pins[k].name_span)),
            );
        for (file, (start, end)) in spans {
            edits
                .entry(file)
                .or_default()
                .push(SexpEdit::replace(start, end, quoted.clone()));
        }
        let mut written: Vec<(&Path, String)> = Vec::new();
        let mut transitions = Vec::new();
        for (&file, file_edits) in &edits {
            let saved = &self.hierarchy.files[file];
            let replacement = apply_edits(saved.content.clone(), file_edits.clone());
            transitions.push(FileTransition::replace(
                &saved.path,
                saved.content.clone(),
                replacement.clone(),
            ));
            written.push((&saved.path, replacement));
        }

        // The check read every sheet, and the transaction guards only those it
        // writes: the others must still hold what was read.
        for (file, saved) in self.hierarchy.files.iter().enumerate() {
            if edits.contains_key(&file) {
                continue;
            }
            #[cfg(test)]
            crate::tools::write_hook::run(&saved.path);
            if read_consistent(&saved.path)? != saved.content {
                return Ok(Err(CallToolResult::error_kind(
                    ToolErrorKind::Conflict {
                        paths: vec![saved.path.display().to_string()],
                    },
                    format!(
                        "'{}' changed after it was read, so no sheet was modified. Reload and \
                         retry.",
                        saved.path.display()
                    ),
                )));
            }
        }
        let paths: Vec<&Path> = written.iter().map(|(path, _)| *path).collect();
        if let Err(refusal) = commit_sheets(&self.root, &paths, transitions)? {
            return Ok(Err(refusal));
        }

        // Read back: each renamed label and sheet pin holds the new name where
        // it stood.
        let mut trees: HashMap<&Path, SexpNode> = HashMap::new();
        for (path, replacement) in &written {
            let on_disk = read_consistent(path)?;
            if on_disk != *replacement {
                return Ok(Err(crate::tools::mutation_outcome_uncertain(
                    path,
                    OPERATION,
                    "the file on disk is not the text the transaction wrote",
                )));
            }
            trees.insert(path, parse_sexp(&on_disk)?);
        }
        let named_here = |node: &SexpNode, x: f64, y: f64| {
            node.get(1).and_then(SexpNode::as_str) == Some(new_name)
                && parse_at(node)
                    .is_some_and(|(at_x, at_y, _)| same_point(at_x, x) && same_point(at_y, y))
        };
        let mut labels = Vec::new();
        for &(file, k) in renamed {
            let path = self.hierarchy.files[file].path.as_path();
            let label = &self.contents[file].labels[k];
            let landed = trees[path].find_all(label.tag).into_iter().any(|node| {
                node.find_str("uuid") == label.uuid.as_deref() && named_here(node, label.x, label.y)
            });
            if !landed {
                return Ok(Err(crate::tools::mutation_outcome_uncertain(
                    path,
                    OPERATION,
                    format!(
                        "the {} at ({}, {}) does not read back as '{new_name}'",
                        label.tag, label.x, label.y
                    ),
                )));
            }
            labels.push(json!({
                "uuid": label.uuid,
                "type": label.tag,
                "file": self.path(file),
                "x": label.x,
                "y": label.y
            }));
        }
        let mut sheet_pins = Vec::new();
        for &(file, s, k) in pins {
            let path = self.hierarchy.files[file].path.as_path();
            let sheet = &self.contents[file].sheets[s];
            let pin = &sheet.pins[k];
            let landed = trees[path]
                .find_all("sheet")
                .into_iter()
                .filter(|block| block.find_str("uuid") == Some(sheet.uuid.as_str()))
                .flat_map(|block| block.find_all("pin"))
                .any(|node| named_here(node, pin.x, pin.y));
            if !landed {
                return Ok(Err(crate::tools::mutation_outcome_uncertain(
                    path,
                    OPERATION,
                    format!(
                        "the sheet pin of '{}' at ({}, {}) does not read back as '{new_name}'",
                        sheet.name, pin.x, pin.y
                    ),
                )));
            }
            sheet_pins.push(json!({
                "file": self.path(file),
                "sheet": sheet.name,
                "x": pin.x,
                "y": pin.y
            }));
        }
        labels.sort_by_key(Value::to_string);
        sheet_pins.sort_by_key(Value::to_string);
        Ok(Ok(Written { labels, sheet_pins }))
    }

    /// Each segment of a renamed label left with more than one name: KiCad's
    /// `multiple_net_names`.
    fn two_names(&self, renamed: &HashSet<(usize, usize)>, after: &Naming) -> Vec<Value> {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for &(file, k) in renamed {
            let sheet = &self.contents[file];
            let segment = sheet.labels[k].segment;
            if !seen.insert((file, segment)) {
                continue;
            }
            let labels = sheet
                .labels
                .iter()
                .enumerate()
                .filter(|(_, l)| l.segment == segment)
                .map(|(m, _)| after.label(file, m));
            let powers = sheet
                .powers
                .iter()
                .filter(|p| p.segment == segment)
                .map(|p| p.name.clone());
            let names: BTreeSet<String> = labels.chain(powers).collect();
            if names.len() > 1 {
                entries.push(json!({
                    "file": self.path(file),
                    "x": sheet.labels[k].x,
                    "y": sheet.labels[k].y,
                    "names": names
                }));
            }
        }
        entries
    }

    /// Sheet pins that kept `old_name` while every hierarchical label of the
    /// name in the sheet they place moved off it: KiCad's `hier_label_mismatch`.
    fn pins_left(
        &self,
        renamed: &HashSet<(usize, usize)>,
        after: &Naming,
        old_name: &str,
    ) -> Vec<Value> {
        let mut entries = Vec::new();
        let mut listed = HashSet::new();
        for instance in &self.instances {
            let Some((up, block)) = instance.parent else {
                continue;
            };
            let file = instance.file;
            let labels = &self.contents[file].labels;
            let moved = renamed
                .iter()
                .any(|&(f, k)| f == file && labels[k].tag == "hierarchical_label");
            let kept = labels
                .iter()
                .enumerate()
                .any(|(m, l)| l.tag == "hierarchical_label" && after.label(file, m) == old_name);
            if !moved || kept {
                continue;
            }
            let up_file = self.instances[up].file;
            let sheet = &self.contents[up_file].sheets[block];
            for (k, pin) in sheet.pins.iter().enumerate() {
                if after.pin(up_file, block, k) == old_name && listed.insert((up_file, block, k)) {
                    entries.push(json!({
                        "file": self.path(up_file),
                        "sheet": sheet.name,
                        "name": old_name,
                        "x": pin.x,
                        "y": pin.y
                    }));
                }
            }
        }
        entries
    }
}

/// Each saved instance with its parent's sheet block and its sheet path.
fn instances_of(hierarchy: &SavedHierarchy, contents: &[SheetContents]) -> Vec<Instance> {
    let saved = &hierarchy.instances;
    let paths: HashMap<&str, usize> = saved
        .iter()
        .enumerate()
        .map(|(i, instance)| (instance.path.as_str(), i))
        .collect();
    let mut instances: Vec<Instance> = saved
        .iter()
        .map(|instance| Instance {
            file: instance.file,
            parent: instance.path.rsplit_once('/').and_then(|(up, sheet_uuid)| {
                let up = *paths.get(up)?;
                let block = contents[saved[up].file]
                    .sheets
                    .iter()
                    .position(|s| s.uuid == sheet_uuid)?;
                Some((up, block))
            }),
            path: String::new(),
        })
        .collect();
    // Parents first: a shorter instance path is never below a longer one.
    let mut order: Vec<usize> = (0..instances.len()).collect();
    order.sort_by_key(|&i| saved[i].path.len());
    for i in order {
        instances[i].path = match instances[i].parent {
            None => "/".to_string(),
            Some((up, block)) => format!(
                "{}{}/",
                instances[up].path, contents[instances[up].file].sheets[block].name
            ),
        };
    }
    instances
}

async fn handle_rename_schematic_net_label(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let schematic = get_path(args, "schematic")?;
    let selection = match selection(args) {
        Ok(selection) => selection,
        Err(refusal) => return Ok(refusal),
    };
    let new_name = match require_str(args, "new_name").and_then(|n| checked_name("new_name", n)) {
        Ok(name) => name,
        Err(refusal) => return Ok(refusal),
    };
    let (allow_merge, allow_split) =
        match (opt_bool(args, "allow_merge"), opt_bool(args, "allow_split")) {
            (Ok(merge), Ok(split)) => (merge, split),
            (Err(refusal), _) | (_, Err(refusal)) => return Ok(refusal),
        };
    let (design, given) = match Design::load(&schematic) {
        Ok(loaded) => loaded,
        Err(refusal) => return Ok(refusal),
    };
    let hierarchy_scope = matches!(
        selection,
        Selection::Scope {
            hierarchy: true,
            ..
        }
    );
    let reached = design.reached(given, hierarchy_scope);
    let (old_name, renamed) = match design.select(&selection, given, &reached) {
        Ok(chosen) => chosen,
        Err(refusal) => return Ok(refusal),
    };
    if new_name == old_name {
        return Ok(invalid_arg(
            "new_name",
            &format!("'{new_name}' is the label's name already"),
        ));
    }
    // Under 'hierarchy' the sheet pins of the name go with their labels.
    let renamed_pins = if hierarchy_scope {
        design.pins_named(&reached, &old_name)
    } else {
        HashSet::new()
    };

    let reasons = match design.bus_refusals(&renamed, [&old_name, &new_name]) {
        Ok(reasons) => reasons,
        Err(refusal) => return Ok(refusal),
    };
    if !reasons.is_empty() {
        return Ok(blocked(reasons));
    }
    let before = Naming {
        contents: &design.contents,
        labels: &renamed,
        pins: &renamed_pins,
        new_name: &new_name,
        after: false,
    };
    let after = Naming {
        after: true,
        ..before
    };
    let (merges, splits) = joins_and_splits(&design.contents, &design.instances, &before, &after);
    let mut reasons = Vec::new();
    if !allow_merge {
        reasons.extend(merges.iter().map(|nets| {
            format!(
                "'{new_name}' would join {}; allow_merge renames anyway",
                nets.join(" with ")
            )
        }));
    }
    if !allow_split {
        reasons.extend(splits.iter().map(|parts| {
            format!(
                "the rename would break this net into {}; allow_split renames anyway",
                parts.join(" and ")
            )
        }));
    }
    if !reasons.is_empty() {
        return Ok(blocked(reasons));
    }

    let Written { labels, sheet_pins } = match design.write(&renamed, &renamed_pins, &new_name)? {
        Ok(written) => written,
        Err(refusal) => return Ok(refusal),
    };

    Ok(CallToolResult::json(&json!({
        "old_name": old_name,
        "new_name": new_name,
        "count": labels.len(),
        "labels": labels,
        "sheet_pins": sheet_pins,
        "merged": merges,
        "split": splits,
        "two_names": design.two_names(&renamed, &after),
        "sheet_pins_left": design.pins_left(&renamed, &after, &old_name),
        "hierarchy_root": design.root.display().to_string(),
        "source": "file"
    })))
}

fn blocked(reasons: Vec<String>) -> CallToolResult {
    let more = reasons.len().saturating_sub(SHOWN);
    let mut shown: Vec<String> = reasons.into_iter().take(SHOWN).collect();
    if more > 0 {
        shown.push(format!("and {more} more"));
    }
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: OPERATION.to_string(),
            reasons: shown.clone(),
        },
        format!("No label was renamed: {}", shown.join("; ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::handler::McpHandler;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;

    /// KiCad 10.0.5's single-pin-net pair: see its README for the nets.
    const ROOT: &str = "single_pin_nets.kicad_sch";
    const CHILD: &str = "single_pin_nets_child.kicad_sch";
    const TWO_PIN: &str = "6f563532-1960-4bee-8635-18eb1ecb12f6";
    const SPLIT_LEFT: &str = "413d5a81-60f3-417f-8611-61535183b829";
    const LOCAL_MIXED: &str = "f09b57cc-2f69-4f4e-80a7-9de3cb56bb02";
    const GLOBAL_MIXED: &str = "5ff0e940-4486-44f3-ba88-2c0fd8071a42";
    const CHILD_LOCAL: &str = "81427a13-8c2e-4fc2-9eb9-db88c28cbbb1";
    const CHILD_SHEET_NET: &str = "27f62676-1284-4dbb-a2f6-b74c4a504c15";

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .expect("the fixture is committed")
    }

    /// `names` copied into a fresh directory, with `project` as its
    /// `.kicad_pro` when given, so a child sheet resolves to its root.
    fn copied(names: &[&str], project: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            let target = dir.path().join(Path::new(name).file_name().unwrap());
            std::fs::write(target, fixture(name)).unwrap();
        }
        if let Some(project) = project {
            std::fs::write(dir.path().join(project), "{}").unwrap();
        }
        dir
    }

    fn single_pin_nets() -> tempfile::TempDir {
        copied(&[ROOT, CHILD], Some("single_pin_nets.kicad_pro"))
    }

    /// The pair with the root's `power:VCC` made `(power local)`.
    fn local_vcc() -> tempfile::TempDir {
        let global = "\t\t(symbol \"power:VCC\"\n\t\t\t(power global)";
        assert!(fixture(ROOT).contains(global));
        let dir = single_pin_nets();
        let root = fixture(ROOT).replace(global, &global.replace("global", "local"));
        std::fs::write(dir.path().join(ROOT), root).unwrap();
        dir
    }

    /// The pair with `U99`, whose hidden `power_in` pin names the global net
    /// `HIDDEN`: see its README.
    const HIDDEN_ROOT: &str = "hidden_power_pin/single_pin_nets.kicad_sch";

    fn hidden_power_pin() -> tempfile::TempDir {
        copied(&[HIDDEN_ROOT, CHILD], Some("single_pin_nets.kicad_pro"))
    }

    /// `tool` through served `tools/call`, schema gate included.
    async fn call(tool: &str, sheet: &Path, mut args: Value) -> CallToolResult {
        args["schematic"] = json!(sheet.to_str().unwrap());
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 146, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    async fn rename(sheet: &Path, args: Value) -> CallToolResult {
        call("rename_schematic_net_label", sheet, args).await
    }

    fn text(result: &CallToolResult) -> &str {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        text
    }

    fn body(result: &CallToolResult) -> Value {
        assert!(!result.is_error, "{}", text(result));
        serde_json::from_str(text(result)).unwrap()
    }

    fn refused(result: &CallToolResult, kind: &str) -> String {
        assert_eq!(
            extract_error_kind(result).as_deref(),
            Some(kind),
            "{}",
            text(result)
        );
        text(result).to_string()
    }

    fn read(dir: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(name)).unwrap()
    }

    #[tokio::test]
    async fn only_the_name_changes() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let reply = body(&rename(&root, json!({ "uuid": TWO_PIN, "new_name": "RENAMED" })).await);
        assert_eq!(reply["count"], 1);
        assert_eq!(reply["labels"][0]["uuid"], TWO_PIN);
        assert_eq!(reply["old_name"], "TWO_PIN");
        assert_eq!(
            read(&dir, ROOT),
            fixture(ROOT).replacen("(label \"TWO_PIN\"", "(label \"RENAMED\"", 1)
        );
        assert_eq!(read(&dir, CHILD), fixture(CHILD));
    }

    #[tokio::test]
    async fn a_global_label_keeps_its_shape_and_fields() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        // Its wire keeps the local MIXED: two names, nothing broken.
        let reply =
            body(&rename(&root, json!({ "uuid": GLOBAL_MIXED, "new_name": "SHARED" })).await);
        assert_eq!(
            reply["two_names"][0]["names"],
            json!(["MIXED", "SHARED"]),
            "{reply}"
        );
        assert_eq!(
            read(&dir, ROOT),
            fixture(ROOT).replacen("(global_label \"MIXED\"", "(global_label \"SHARED\"", 1)
        );
    }

    #[tokio::test]
    async fn names_are_written_as_kicad_writes_them() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let reply = body(&rename(&root, json!({ "uuid": TWO_PIN, "new_name": "A/B\n" })).await);
        assert_eq!(reply["new_name"], "A{slash}B");
        assert!(read(&dir, ROOT).contains("(label \"A{slash}B\""));
        // The name is found again as the caller writes it.
        let args = json!({ "net": "A/B", "x": 85.09, "y": 39.37, "new_name": "C\"D" });
        body(&rename(&root, args).await);
        assert!(read(&dir, ROOT).contains("(label \"C\\\"D\""));

        let empty = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "\r\n" })).await;
        refused(&empty, "invalid_argument");
    }

    #[tokio::test]
    async fn bus_names_and_text_variables_are_refused() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        for name in ["D[0..3]", "{SDA SCL}", "I2C{SDA SCL}"] {
            let result = rename(&root, json!({ "uuid": TWO_PIN, "new_name": name })).await;
            assert!(refused(&result, "invalid_argument").contains("bus name"));
        }
        // `${NET}` would also read as a bus group; the variable is the reason.
        let variable = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "${NET}" })).await;
        assert!(refused(&variable, "invalid_argument").contains("text variable"));
        assert_eq!(read(&dir, ROOT), fixture(ROOT));
        body(&rename(&root, json!({ "uuid": TWO_PIN, "new_name": "~{RST}" })).await);
    }

    #[test]
    fn bus_names_are_told_as_kicad_tells_them() {
        for bus in [
            "D[0..7]",
            "D[7..0]",
            "D_{[1..2]}",
            "~{BE[0..3]}",
            "{A B}",
            "USB{DP DM}",
            "A{slash}B[0..1]",
        ] {
            assert!(is_bus_name(bus), "{bus}");
        }
        for net in [
            "SIG",
            "~{RST}",
            "D[0]",
            "D[3..3]",
            "A{slash}B",
            "I^{2}C",
            "D[0..1]x",
        ] {
            assert!(!is_bus_name(net), "{net}");
        }
        let aliases = BTreeMap::from([(
            "I2C".to_string(),
            vec!["SDA".to_string(), "SCL".to_string()],
        )]);
        assert!(bus_carries("D[0..3]", "D2", &aliases));
        assert!(!bus_carries("D[0..3]", "D4", &aliases));
        assert!(!bus_carries("D[0..3]", "D02", &aliases));
        assert!(bus_carries("USB{DP DM}", "USB.DP", &aliases));
        assert!(!bus_carries("USB{DP DM}", "DP", &aliases));
        assert!(bus_carries("{SDA SCL}", "SCL", &aliases));
        assert!(bus_carries("BUS{D[0..1] I2C}", "BUS.D1", &aliases));
        assert!(bus_carries("BUS{D[0..1] I2C}", "BUS.SDA", &aliases));
    }

    #[test]
    fn bus_aliases_come_from_sheets_and_the_project() {
        let sheet =
            parse_sexp("(kicad_sch (bus_alias \" I2C \" (members \"SDA\" \" SCL \")))").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.kicad_pro");
        std::fs::write(
            &project,
            r#"{ "schematic": { "bus_aliases": { "SPI": ["MOSI", "", "MISO"] } } }"#,
        )
        .unwrap();
        let aliases = bus_aliases(Some(&project), &[&sheet]).unwrap();
        assert_eq!(aliases["I2C"], ["SDA", "SCL"]);
        assert_eq!(aliases["SPI"], ["MOSI", "MISO"]);
        assert!(bus_carries("{I2C}", "SCL", &aliases));
    }

    #[test]
    fn braces_nested_too_deep_are_refused_not_followed() {
        let deep = "{".repeat(20_000);
        assert_eq!(unescaped(&deep), deep);
        assert!(!is_bus_name(&deep));
        let refused = checked_name("new_name", &deep).unwrap_err();
        assert_eq!(
            extract_error_kind(&refused).as_deref(),
            Some("invalid_argument")
        );
        assert!(checked_name("new_name", "~{~{~{RST}}}").is_ok());
    }

    #[tokio::test]
    async fn a_sheet_changed_after_the_check_read_it_refuses_the_write() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        // The child, which the rename does not write, is saved in between.
        let saved = fixture(CHILD).replacen("(label \"CHILD_LOCAL\"", "(label \"TWO_PIN\"", 1);
        crate::tools::write_hook::set(saved.clone());
        let result = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "RENAMED" })).await;
        assert!(!crate::tools::write_hook::armed());
        refused(&result, "conflict");
        assert_eq!(read(&dir, ROOT), fixture(ROOT));
        assert_eq!(read(&dir, CHILD), saved);
    }

    #[tokio::test]
    async fn the_schema_refuses_what_it_does_not_declare() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        for args in [
            json!({ "uuid": TWO_PIN, "new_name": "X", "net_name": "TWO_PIN" }),
            json!({ "net": "TWO_PIN", "scope": "page", "new_name": "X" }),
            json!({ "uuid": TWO_PIN, "new_name": "X", "allow_merge": "yes" }),
        ] {
            refused(&rename(&root, args).await, "invalid_argument");
        }
        assert_eq!(read(&dir, ROOT), fixture(ROOT));
    }

    #[test]
    fn a_driver_is_chosen_as_resolve_drivers_chooses_it() {
        let driver = |name: &str, priority: u8, output: bool| Driver {
            name: name.to_string(),
            priority,
            output,
            hidden: false,
        };
        let pick = |drivers: &[Driver]| chosen(drivers).name.clone();
        assert_eq!(
            pick(&[
                driver("A", LOCAL_LABEL, false),
                driver("Z", GLOBAL_LABEL, false)
            ]),
            "Z"
        );
        let hidden = Driver {
            hidden: true,
            ..driver("A", GLOBAL_POWER_PIN, false)
        };
        assert_eq!(pick(&[hidden, driver("Z", GLOBAL_POWER_PIN, false)]), "Z");
        assert_eq!(
            pick(&[driver("A", SHEET_PIN, false), driver("B", SHEET_PIN, true)]),
            "B"
        );
        assert_eq!(
            pick(&[
                driver("A-Pad1", LOCAL_LABEL, false),
                driver("B", LOCAL_LABEL, false)
            ]),
            "B"
        );
        assert_eq!(
            pick(&[
                driver("B", LOCAL_LABEL, false),
                driver("A", LOCAL_LABEL, false)
            ]),
            "A"
        );
    }

    #[test]
    fn unescaping_follows_kicad() {
        assert_eq!(unescaped("A{slash}B"), "A/B");
        assert_eq!(unescaped("~{RST}"), "~{RST}");
        assert_eq!(unescaped("${VAR}"), "${VAR}");
        assert_eq!(unescaped("{dblquote}x{unknown}"), "\"x{unknown}");
        assert_eq!(unescaped("A{open"), "A{open");
    }

    #[tokio::test]
    async fn a_name_a_bus_carries_is_refused() {
        let dir = copied(
            &[
                "rigid_block_move_kicad10.kicad_sch",
                "rigid_block_move_child_kicad10.kicad_sch",
            ],
            None,
        );
        let sheet = dir.path().join("rigid_block_move_kicad10.kicad_sch");
        let off = rename(
            &sheet,
            json!({ "net": "D0", "scope": "sheet", "new_name": "X" }),
        )
        .await;
        assert!(refused(&off, "plan_blocked").contains("'D[0..3]'"));
        let onto = rename(
            &sheet,
            json!({ "net": "SIG", "scope": "sheet", "new_name": "D1" }),
        )
        .await;
        assert!(refused(&onto, "plan_blocked").contains("carries 'D1'"));
        assert_eq!(
            read(&dir, "rigid_block_move_kicad10.kicad_sch"),
            fixture("rigid_block_move_kicad10.kicad_sch")
        );
    }

    #[tokio::test]
    async fn breaking_a_join_by_name_is_a_split() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let result = rename(&root, json!({ "uuid": SPLIT_LEFT, "new_name": "SIDE" })).await;
        let message = refused(&result, "plan_blocked");
        assert!(
            message.contains("'SPLIT' on /") && message.contains("allow_split"),
            "{message}"
        );
        assert_eq!(read(&dir, ROOT), fixture(ROOT));

        let args = json!({ "uuid": SPLIT_LEFT, "new_name": "SIDE", "allow_split": true });
        let reply = body(&rename(&root, args).await);
        assert_eq!(reply["split"].as_array().unwrap().len(), 1, "{reply}");
        assert_eq!(reply["split"][0].as_array().unwrap().len(), 2, "{reply}");
    }

    #[tokio::test]
    async fn every_label_of_a_name_on_the_sheet() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let reply = body(
            &rename(
                &root,
                json!({ "net": "SPLIT", "scope": "sheet", "new_name": "SIDE" }),
            )
            .await,
        );
        assert_eq!(reply["count"], 2);
        assert_eq!(reply["split"], json!([]));
        assert_eq!(reply["merged"], json!([]));
    }

    #[tokio::test]
    async fn a_name_in_use_is_a_merge() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let result = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "LONE" })).await;
        let message = refused(&result, "plan_blocked");
        assert!(
            message.contains("'TWO_PIN' on /") && message.contains("allow_merge"),
            "{message}"
        );
        assert_eq!(read(&dir, ROOT), fixture(ROOT));

        let args = json!({ "uuid": TWO_PIN, "new_name": "LONE", "allow_merge": true });
        let reply = body(&rename(&root, args).await);
        assert_eq!(reply["merged"][0].as_array().unwrap().len(), 2, "{reply}");
    }

    #[tokio::test]
    async fn a_local_label_joins_a_global_label_of_its_name_on_its_sheet() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        // The local MIXED shares the global one's wire: renaming it leaves that
        // wire two names and breaks nothing, and leaves MIXED to the global.
        let reply = body(&rename(&root, json!({ "uuid": LOCAL_MIXED, "new_name": "OWN" })).await);
        assert_eq!(
            reply["two_names"][0]["names"],
            json!(["MIXED", "OWN"]),
            "{reply}"
        );
        let result = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "MIXED" })).await;
        assert!(refused(&result, "plan_blocked").contains("'MIXED' on /"));
    }

    #[tokio::test]
    async fn a_sheet_pin_alone_on_its_wire_joins_no_label_of_its_name() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let unname = json!({ "net": "SHEET_NET", "x": 76.2, "y": 139.7 });
        body(&call("delete_schematic_net_label", &root, unname).await);
        let reply = body(&rename(&root, json!({ "uuid": TWO_PIN, "new_name": "SHEET_NET" })).await);
        assert_eq!(reply["merged"], json!([]), "{reply}");
    }

    #[tokio::test]
    async fn a_wire_with_a_second_global_name_joins_it_on_other_sheets() {
        // The root's MIXED wire also carries a second global name; the child's
        // CHILD_LOCAL wire carries the global ACROSS. KiCad joins through the
        // second name only when it does not choose it for the wire: MIXED is
        // chosen over ZZ, AA over MIXED.
        for (second, joins) in [("ZZ", true), ("AA", false)] {
            let dir = single_pin_nets();
            let root = dir.path().join(ROOT);
            let child = dir.path().join(CHILD);
            let add =
                json!({ "net": second, "x": 66.04, "y": 160.02, "label_type": "global_label" });
            body(&call("add_schematic_net_label", &root, add).await);
            let across =
                json!({ "net": "ACROSS", "x": 96.52, "y": 59.69, "label_type": "global_label" });
            body(&call("add_schematic_net_label", &child, across).await);
            let result = rename(&child, json!({ "uuid": CHILD_LOCAL, "new_name": second })).await;
            if joins {
                let message = refused(&result, "plan_blocked");
                assert!(message.contains(&format!("'{second}' on /")), "{message}");
            } else {
                assert_eq!(body(&result)["merged"], json!([]), "{second}");
            }
        }
    }

    #[tokio::test]
    async fn wires_that_share_a_second_name_are_one_net() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        // The TWO_PIN wire gains ZMID and a LONE wire ZOTHER, neither chosen.
        for (net, x, y) in [("ZMID", 96.52, 39.37), ("ZOTHER", 104.14, 59.69)] {
            body(
                &call(
                    "add_schematic_net_label",
                    &root,
                    json!({ "net": net, "x": x, "y": y }),
                )
                .await,
            );
        }
        let args = json!({ "net": "ZOTHER", "x": 104.14, "y": 59.69, "new_name": "ZMID" });
        let message = refused(&rename(&root, args).await, "plan_blocked");
        assert!(message.contains("'TWO_PIN' on /"), "{message}");
    }

    #[tokio::test]
    async fn a_net_chooses_its_driver_again_once_its_wires_are_joined() {
        // On the root, the TWO_PIN wire gains the global ZZ and a LONE wire the
        // local ZZ, joining the two; with the global AA on the LONE wire too,
        // KiCad chooses AA for the joined net, which leaves ZZ a global name it
        // did not choose, and that reaches the child's wire.
        for (with_aa, joins) in [(true, true), (false, false)] {
            let dir = single_pin_nets();
            let root = dir.path().join(ROOT);
            let child = dir.path().join(CHILD);
            let mut adds = vec![
                json!({ "net": "ZZ", "x": 96.52, "y": 39.37, "label_type": "global_label" }),
                json!({ "net": "ZZ", "x": 104.14, "y": 59.69 }),
            ];
            if with_aa {
                adds.push(
                    json!({ "net": "AA", "x": 100.33, "y": 59.69, "label_type": "global_label" }),
                );
            }
            for add in adds {
                body(&call("add_schematic_net_label", &root, add).await);
            }
            let across =
                json!({ "net": "ACROSS", "x": 96.52, "y": 59.69, "label_type": "global_label" });
            body(&call("add_schematic_net_label", &child, across).await);
            let result = rename(&child, json!({ "uuid": CHILD_LOCAL, "new_name": "ZZ" })).await;
            if joins {
                refused(&result, "plan_blocked");
            } else {
                assert_eq!(body(&result)["merged"], json!([]), "without AA");
            }
        }
    }

    #[tokio::test]
    async fn a_sheet_pin_alone_on_its_wire_joins_a_wire_that_carries_its_name_unchosen() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        // The sheet pin's wire loses its label; a LONE wire gains SHEET_NET,
        // which KiCad does not choose over LONE.
        let unname = json!({ "net": "SHEET_NET", "x": 76.2, "y": 139.7 });
        body(&call("delete_schematic_net_label", &root, unname).await);
        let add = json!({ "net": "SHEET_NET", "x": 101.6, "y": 59.69 });
        body(&call("add_schematic_net_label", &root, add).await);
        let args = json!({ "net": "SHEET_NET", "x": 101.6, "y": 59.69, "new_name": "OTHER" });
        let message = refused(&rename(&root, args).await, "plan_blocked");
        assert!(message.contains("sheet pin 'SHEET_NET' on /"), "{message}");
    }

    #[tokio::test]
    async fn a_global_label_joins_its_name_on_other_sheets() {
        let dir = single_pin_nets();
        let child = dir.path().join(CHILD);
        let add =
            json!({ "net": "ACROSS", "x": 120.65, "y": 100.33, "label_type": "global_label" });
        body(&call("add_schematic_net_label", &child, add).await);
        let root = dir.path().join(ROOT);
        let result = rename(
            &root,
            json!({ "net": "MIXED", "scope": "sheet", "new_name": "ACROSS" }),
        )
        .await;
        assert!(refused(&result, "plan_blocked").contains("'ACROSS' on /Child/"));
    }

    #[tokio::test]
    async fn a_local_power_symbol_joins_on_its_own_sheet_only() {
        // The root's VCC symbol as the fixture has it, and as a local one.
        for (dir, joins) in [(single_pin_nets(), true), (local_vcc(), false)] {
            let child = dir.path().join(CHILD);
            let add =
                json!({ "net": "SPARE", "x": 120.65, "y": 100.33, "label_type": "global_label" });
            body(&call("add_schematic_net_label", &child, add).await);
            let result = rename(
                &child,
                json!({ "net": "SPARE", "scope": "sheet", "new_name": "VCC" }),
            )
            .await;
            if joins {
                let message = refused(&result, "plan_blocked");
                assert!(
                    message.contains("'VCC' on / ") && message.contains("'SPARE' on /Child/"),
                    "{message}"
                );
            } else {
                assert_eq!(body(&result)["merged"], json!([]));
            }
        }
    }

    #[tokio::test]
    async fn a_hidden_power_pin_names_a_global_net() {
        let dir = hidden_power_pin();
        let root = dir.path().join(ROOT);
        let result = rename(&root, json!({ "uuid": TWO_PIN, "new_name": "HIDDEN" })).await;
        refused(&result, "plan_blocked");
        assert_eq!(read(&dir, ROOT), fixture(HIDDEN_ROOT));

        let child = dir.path().join(CHILD);
        let add = json!({ "net": "SPARE", "x": 120.65, "y": 100.33, "label_type": "global_label" });
        body(&call("add_schematic_net_label", &child, add).await);
        let result = rename(
            &child,
            json!({ "net": "SPARE", "scope": "sheet", "new_name": "HIDDEN" }),
        )
        .await;
        let message = refused(&result, "plan_blocked");
        assert!(
            message.contains("'HIDDEN' on / ") && message.contains("'SPARE' on /Child/"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_local_label_stays_on_its_sheet() {
        let dir = single_pin_nets();
        let child = dir.path().join(CHILD);
        for name in ["TWO_PIN", "MIXED"] {
            let reply =
                body(&rename(&child, json!({ "uuid": CHILD_LOCAL, "new_name": name })).await);
            assert_eq!(reply["merged"], json!([]), "{name}: {reply}");
        }
    }

    #[tokio::test]
    async fn a_hierarchical_label_alone_leaves_its_sheet_pin() {
        let dir = single_pin_nets();
        let child = dir.path().join(CHILD);
        let result = rename(
            &child,
            json!({ "uuid": CHILD_SHEET_NET, "new_name": "LINK" }),
        )
        .await;
        assert!(refused(&result, "plan_blocked").contains("sheet pin 'SHEET_NET' on /"));
        assert_eq!(read(&dir, CHILD), fixture(CHILD));

        let args = json!({ "uuid": CHILD_SHEET_NET, "new_name": "LINK", "allow_split": true });
        let reply = body(&rename(&child, args).await);
        assert_eq!(reply["sheet_pins_left"][0]["sheet"], "Child", "{reply}");
        assert_eq!(reply["sheet_pins_left"][0]["x"], 149.86, "{reply}");
        assert_eq!(read(&dir, ROOT), fixture(ROOT));
    }

    #[tokio::test]
    async fn hierarchy_scope_renames_the_sheet_pins_with_their_labels() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let args = json!({ "net": "SHEET_NET", "scope": "hierarchy", "new_name": "LINK" });
        let reply = body(&rename(&root, args).await);
        assert_eq!(reply["count"], 2, "{reply}");
        assert_eq!(reply["sheet_pins"].as_array().unwrap().len(), 1, "{reply}");
        assert_eq!(reply["split"], json!([]));
        assert_eq!(reply["sheet_pins_left"], json!([]));
        assert!(read(&dir, ROOT).contains("(pin \"LINK\" input"));
        assert!(read(&dir, CHILD).contains("(hierarchical_label \"LINK\""));
    }

    #[tokio::test]
    async fn two_names_on_one_wire_are_listed_not_refused() {
        let dir = copied(&["two_name_nets.kicad_sch"], None);
        let sheet = dir.path().join("two_name_nets.kicad_sch");
        let args = json!({ "net": "AAA", "x": 55.88, "y": 119.38, "new_name": "BBB" });
        let reply = body(&rename(&sheet, args).await);
        assert_eq!(
            reply["two_names"][0]["names"],
            json!(["BBB", "ZZZ"]),
            "{reply}"
        );
        assert_eq!(reply["split"], json!([]));
    }

    #[tokio::test]
    async fn a_sheet_placed_twice_is_written_once() {
        let dir = copied(
            &[
                "title_block_hierarchy/complex_hierarchy.kicad_sch",
                "title_block_hierarchy/ampli_ht.kicad_sch",
            ],
            Some("complex_hierarchy.kicad_pro"),
        );
        let child = dir.path().join("ampli_ht.kicad_sch");
        let args = json!({ "net": "PIEZO_IN", "scope": "sheet", "new_name": "PIEZO_DRIVE" });
        let reply = body(&rename(&child, args).await);
        assert_eq!(reply["count"], 1, "{reply}");
        assert_eq!(
            read(&dir, "ampli_ht.kicad_sch"),
            fixture("title_block_hierarchy/ampli_ht.kicad_sch").replacen(
                "(label \"PIEZO_IN\"",
                "(label \"PIEZO_DRIVE\"",
                1
            )
        );
        assert_eq!(
            read(&dir, "complex_hierarchy.kicad_sch"),
            fixture("title_block_hierarchy/complex_hierarchy.kicad_sch")
        );
    }

    #[tokio::test]
    async fn one_label_is_chosen_by_its_position_or_refused() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        let missing = rename(
            &root,
            json!({ "net": "LONE", "x": 1.0, "y": 2.0, "new_name": "X" }),
        )
        .await;
        assert!(text(&missing).contains("Found 2 label(s) named 'LONE'"));
        let add = json!({ "net": "LONE", "x": 95.25, "y": 59.69 });
        body(&call("add_schematic_net_label", &root, add).await);
        let both = rename(
            &root,
            json!({ "net": "LONE", "x": 95.25, "y": 59.69, "new_name": "X" }),
        )
        .await;
        refused(&both, "ambiguous_target");
    }

    /// The pins of each net in KiCad's netlist of `root`, as `REF.pin`.
    fn kicad_nets(cli: &str, root: &Path) -> Vec<BTreeSet<String>> {
        let out = root.with_extension("net");
        let status = std::process::Command::new(cli)
            .args(["sch", "export", "netlist", "--output"])
            .arg(&out)
            .arg(root)
            .status()
            .expect("kicad-cli runs");
        assert!(status.success(), "kicad-cli exported no netlist");
        let netlist = parse_sexp(&std::fs::read_to_string(&out).unwrap()).unwrap();
        netlist
            .find("nets")
            .expect("a nets section")
            .find_all("net")
            .into_iter()
            .map(|net| {
                net.find_all("node")
                    .into_iter()
                    .map(|node| {
                        format!(
                            "{}.{}",
                            node.find_str("ref").unwrap(),
                            node.find_str("pin").unwrap()
                        )
                    })
                    .collect()
            })
            .collect()
    }

    /// Whether some net of `to` takes pins from more than one net of `from`.
    fn draws_from_several(from: &[BTreeSet<String>], to: &[BTreeSet<String>]) -> bool {
        to.iter()
            .any(|net| from.iter().filter(|other| !other.is_disjoint(net)).count() > 1)
    }

    /// KiCad's netlist decides every case: each is a rename whose nets reach a
    /// pin on both sides, so a join or a split shows in the pins. The tool's
    /// `merged` and `split` must say what KiCad's netlist shows.
    ///
    ///     KICAD_CLI=/path/to/kicad-cli cargo test -p copperpour-core --lib \
    ///         sch_label_rename::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "needs a real kicad-cli"]
    async fn live_kicad_netlists_agree_with_the_merge_and_split_checks() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        // Labels a case adds or removes first, through the tools.
        let across = (
            CHILD,
            "add_schematic_net_label",
            json!({ "net": "ACROSS", "x": 96.52, "y": 59.69, "label_type": "global_label" }),
        );
        let across_again = across.clone();
        let zz = (
            ROOT,
            "add_schematic_net_label",
            json!({ "net": "ZZ", "x": 66.04, "y": 160.02, "label_type": "global_label" }),
        );
        let unname = (
            ROOT,
            "delete_schematic_net_label",
            json!({ "net": "SHEET_NET", "x": 76.2, "y": 139.7 }),
        );
        let aa = (
            ROOT,
            "add_schematic_net_label",
            json!({ "net": "AA", "x": 66.04, "y": 160.02, "label_type": "global_label" }),
        );
        let sheet_net_on_lone = (
            ROOT,
            "add_schematic_net_label",
            json!({ "net": "SHEET_NET", "x": 101.6, "y": 59.69 }),
        );
        let add = |net: &str, x: f64, y: f64, kind: &str| {
            (
                ROOT,
                "add_schematic_net_label",
                json!({ "net": net, "x": x, "y": y, "label_type": kind }),
            )
        };
        let zmid = add("ZMID", 96.52, 39.37, "net_label");
        let zother = add("ZOTHER", 104.14, 59.69, "net_label");
        let zz_global = add("ZZ", 96.52, 39.37, "global_label");
        let zz_local = add("ZZ", 104.14, 59.69, "net_label");
        let aa_on_lone = add("AA", 100.33, 59.69, "global_label");
        let own = (
            ROOT,
            "rename_schematic_net_label",
            json!({ "uuid": LOCAL_MIXED, "new_name": "OWN" }),
        );
        let spare = (
            CHILD,
            "add_schematic_net_label",
            json!({ "net": "SPARE", "x": 100.33, "y": 59.69, "label_type": "global_label" }),
        );
        // The pair a case starts from.
        type Start = fn() -> tempfile::TempDir;
        let cases: [(Start, Vec<_>, _, _, _, _); 22] = [
            (
                single_pin_nets,
                vec![],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "TWO_PIN" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "MIXED" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![],
                ROOT,
                json!({ "net": "SPLIT", "scope": "sheet", "new_name": "SIDE" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![],
                ROOT,
                json!({ "uuid": LOCAL_MIXED, "new_name": "OWN" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![],
                ROOT,
                json!({ "net": "SHEET_NET", "scope": "hierarchy", "new_name": "LINK" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![],
                ROOT,
                json!({ "uuid": SPLIT_LEFT, "new_name": "SIDE" }),
                false,
                true,
            ),
            (
                single_pin_nets,
                vec![],
                CHILD,
                json!({ "uuid": CHILD_SHEET_NET, "new_name": "LINK" }),
                false,
                true,
            ),
            (
                single_pin_nets,
                vec![],
                ROOT,
                json!({ "uuid": TWO_PIN, "new_name": "LONE" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![own],
                ROOT,
                json!({ "uuid": TWO_PIN, "new_name": "MIXED" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![across.clone()],
                ROOT,
                json!({ "net": "MIXED", "scope": "sheet", "new_name": "ACROSS" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![across.clone(), zz],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "ZZ" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![across, aa],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "AA" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![unname.clone(), sheet_net_on_lone],
                ROOT,
                json!({ "net": "SHEET_NET", "x": 101.6, "y": 59.69, "new_name": "OTHER" }),
                false,
                true,
            ),
            (
                single_pin_nets,
                vec![zmid, zother],
                ROOT,
                json!({ "net": "ZOTHER", "x": 104.14, "y": 59.69, "new_name": "ZMID" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![
                    zz_global.clone(),
                    zz_local.clone(),
                    aa_on_lone,
                    across_again.clone(),
                ],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "ZZ" }),
                true,
                false,
            ),
            (
                single_pin_nets,
                vec![zz_global, zz_local, across_again],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "ZZ" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![unname],
                ROOT,
                json!({ "uuid": TWO_PIN, "new_name": "SHEET_NET" }),
                false,
                false,
            ),
            (
                single_pin_nets,
                vec![spare.clone()],
                CHILD,
                json!({ "net": "SPARE", "scope": "sheet", "new_name": "VCC" }),
                true,
                false,
            ),
            (
                local_vcc,
                vec![spare.clone()],
                CHILD,
                json!({ "net": "SPARE", "scope": "sheet", "new_name": "VCC" }),
                false,
                false,
            ),
            (
                hidden_power_pin,
                vec![],
                ROOT,
                json!({ "uuid": TWO_PIN, "new_name": "HIDDEN" }),
                true,
                false,
            ),
            (
                hidden_power_pin,
                vec![spare],
                CHILD,
                json!({ "net": "SPARE", "scope": "sheet", "new_name": "HIDDEN" }),
                true,
                false,
            ),
            (
                hidden_power_pin,
                vec![],
                CHILD,
                json!({ "uuid": CHILD_LOCAL, "new_name": "HIDDEN" }),
                false,
                false,
            ),
        ];
        for (start, setup, sheet, mut args, merges, splits) in cases {
            let dir = start();
            let root = dir.path().join(ROOT);
            for (file, tool, step) in setup {
                body(&call(tool, &dir.path().join(file), step).await);
            }
            let before = kicad_nets(&cli, &root);
            args["allow_merge"] = json!(true);
            args["allow_split"] = json!(true);
            let reply = body(&rename(&dir.path().join(sheet), args.clone()).await);
            let after = kicad_nets(&cli, &root);
            assert_eq!(draws_from_several(&before, &after), merges, "KiCad, {args}");
            assert_eq!(draws_from_several(&after, &before), splits, "KiCad, {args}");
            assert_eq!(reply["merged"] != json!([]), merges, "{args}: {reply}");
            assert_eq!(reply["split"] != json!([]), splits, "{args}: {reply}");
        }
    }

    #[tokio::test]
    async fn the_arguments_choose_one_way_to_select() {
        let dir = single_pin_nets();
        let root = dir.path().join(ROOT);
        for args in [
            json!({ "uuid": TWO_PIN, "net": "TWO_PIN", "new_name": "X" }),
            json!({ "net": "TWO_PIN", "new_name": "X" }),
            json!({ "net": "TWO_PIN", "scope": "sheet", "x": 85.09, "y": 39.37, "new_name": "X" }),
            json!({ "net": "TWO_PIN", "scope": "page", "new_name": "X" }),
            json!({ "uuid": TWO_PIN, "new_name": "TWO_PIN" }),
            json!({ "uuid": TWO_PIN, "new_name": "X", "allow_merge": "yes" }),
        ] {
            refused(&rename(&root, args.clone()).await, "invalid_argument");
        }
        assert_eq!(read(&dir, ROOT), fixture(ROOT));
    }
}
