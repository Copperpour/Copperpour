//! `edit_symbol_pin`: change one pin's electrical type, name or graphic style
//! in an existing `.kicad_sym` symbol, leaving everything else as written.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::library::{skip_sexp_token, ALLOWED_PIN_ELECTRICAL_TYPES};
use crate::tools::sch_annotate::opt_string;
use crate::tools::symbol_fields::{header, symbol_block, unusable, write_checked};
use crate::tools::{invalid_arg, ToolContext, ToolDef};
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

/// The first library version in which a pin named `~` is not an empty name
/// (KiCad's `sch_file_versions.h`).
const TILDE_IS_TEXT: u32 = 20250318;

pub(super) fn tool() -> ToolDef {
    tool!(
        "edit_symbol_pin",
        "Set the electrical type, graphic style or name of one pin of an existing library \
         symbol. A pin number found in more than one unit or body style needs unit or \
         body_style to choose the pin. Position, length, text effects and the rest of the \
         library are left as written. Reports each changed part's previous and current value.",
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
                "name": { "type": "string", "description": "New pin name. KiCad keeps no space in a pin name, so a space is written as '_'" }
            },
            "required": ["library_path", "symbol_name", "pin_number"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_edit_symbol_pin(args, ctx).await }
    )
}

/// Which pin a call names, and what it asks to set; a `None` value leaves
/// that part of the pin alone.
struct Request<'a> {
    symbol: &'a str,
    number: &'a str,
    unit: Option<u32>,
    body_style: Option<u32>,
    pin_type: Option<String>,
    style: Option<String>,
    name: Option<String>,
}

/// One `(pin TYPE STYLE (at …) (length …) (name "N" …) (number "NUM" …))` block,
/// with the spans of the parts a call can change.
#[derive(Debug, Clone, PartialEq)]
struct Pin {
    unit: u32,
    body_style: u32,
    number: String,
    pin_type: String,
    style: String,
    /// As KiCad reads it, which for a library before [`TILDE_IS_TEXT`] is
    /// empty where the file says `~`.
    name: String,
    type_span: (usize, usize),
    style_span: (usize, usize),
    name_span: (usize, usize),
}

impl Pin {
    fn place(&self) -> String {
        format!("unit {} body style {}", self.unit, self.body_style)
    }
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

fn parse_request(args: &serde_json::Value) -> Result<Request<'_>, CallToolResult> {
    let request = Request {
        symbol: super::require_str(args, "symbol_name")?,
        number: super::require_str(args, "pin_number")?,
        unit: super::opt_u32(args, "unit")?,
        body_style: super::opt_u32(args, "body_style")?,
        pin_type: enum_arg(args, "type", &ALLOWED_PIN_ELECTRICAL_TYPES)?,
        style: enum_arg(args, "style", PIN_STYLES)?,
        // As eeschema's SCH_PIN::SetName stores it, so KiCad reads back what is written.
        name: opt_string(args, "name")?.map(|name| name.replace(' ', "_")),
    };
    if request.pin_type.is_none() && request.style.is_none() && request.name.is_none() {
        return Err(invalid_arg(
            "pin_change",
            "supply at least one of type, style or name",
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
    for (child_start, _) in find_direct_child_blocks(&source[start..end], "pin") {
        let child_start = start + child_start;
        let slot = match header(source, child_start).map(|(head, _)| head) {
            Some("name") => &mut name,
            Some("number") => &mut number,
            _ => continue,
        };
        let (_, after_head) = skip_sexp_token(source, child_start + 1).ok_or(BAD)?;
        let span = skip_sexp_token(source, after_head).ok_or(BAD)?;
        let text = copperpour_sexp::parse_sexp(&source[span.0..span.1]).map_err(|_| BAD)?;
        *slot = Some((text.as_str().ok_or(BAD)?.to_string(), span));
    }
    let ((mut name, name_span), (number, _)) = name.zip(number).ok_or(BAD)?;
    if tilde_is_empty && name == "~" {
        name.clear();
    }
    Ok(Pin {
        unit: place.0,
        body_style: place.1,
        number,
        pin_type: source[type_span.0..type_span.1].to_string(),
        style: source[style_span.0..style_span.1].to_string(),
        name,
        type_span,
        style_span,
        name_span,
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
    let Request { symbol, number, .. } = *request;
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
    let pins = symbol_pins(source, start, end).map_err(|reason| unusable(path, &reason))?;
    let pin = choose_pin(path, &pins, request)?.clone();

    let mut edits = Vec::new();
    let mut changes = serde_json::Map::new();
    for (key, new, old, span, text) in [
        (
            "type",
            &request.pin_type,
            &pin.pin_type,
            pin.type_span,
            None,
        ),
        ("style", &request.style, &pin.style, pin.style_span, None),
        (
            "name",
            &request.name,
            &pin.name,
            pin.name_span,
            request.name.as_deref().map(quote_string),
        ),
    ] {
        let Some(new) = new.as_ref().filter(|new| *new != old) else {
            continue;
        };
        edits.push(SexpEdit::replace(
            span.0,
            span.1,
            text.unwrap_or_else(|| new.clone()),
        ));
        changes.insert(key.into(), json!({"previous": old, "current": new}));
    }
    Ok((
        pin,
        apply_edits(source.to_string(), edits),
        serde_json::Value::Object(changes),
    ))
}

/// Why `written` is not `before` with only the chosen pin changed as asked,
/// or `None` when it is. Each pin's number, unit, body style, type, style and
/// name are compared, in file order.
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
        let (pin_type, style, name) = if was == target {
            (
                request.pin_type.as_ref().unwrap_or(&was.pin_type),
                request.style.as_ref().unwrap_or(&was.style),
                request.name.as_ref().unwrap_or(&was.name),
            )
        } else {
            (&was.pin_type, &was.style, &was.name)
        };
        if now.number != was.number
            || now.unit != was.unit
            || now.body_style != was.body_style
            || now.pin_type != *pin_type
            || now.style != *style
            || now.name != *name
        {
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
}
