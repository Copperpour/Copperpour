//! `set_footprint_field_style`: set the glyph size and stroke thickness of
//! the reference or value field on many placed footprints at once (#198).
//!
//! A board synced before footprint fields carried their library's stroke
//! thickness holds every Reference at KiCad's derived default, which a
//! fabricator's minimum then flags once per footprint. Nothing could repair
//! that short of KiCad's own "Edit Text and Graphics Properties":
//! `update_footprints_from_library` keeps placed fields by design and
//! `edit_footprint_field` takes one footprint per call on a live board only.
//!
//! This tool is live-first with a file fallback. On a live board the fields
//! go through the same whole-footprint update `edit_footprint_field` uses, in
//! one undo step. With no live KiCad holding the board, the `(size …)` and
//! `(thickness …)` of each field's `(font …)` are rewritten in the saved
//! file, guarded against a concurrent save, and re-read to prove the result.
//! Either way the response lists the old and new values of every field.

use super::{live_error_result, mm, positive_length_nm, MAX_TEXT_SIZE_MM};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::pcb_board::{attempt_ipc_write_in, write_board_edits, BoardWrite};
use crate::tools::{get_path, invalid_arg, ToolContext};
use copperpour_ipc::client::{FootprintTargetError, TextTargetError};
use copperpour_ipc::{
    IpcFootprintField, IpcFootprintFieldBatchOutcome, IpcFootprintFieldSlot, IpcTextEdit,
};
use copperpour_sexp::parser::parse_sexp;
use copperpour_sexp::writer::{find_direct_child_blocks, read_consistent, SexpEdit};
use serde_json::{json, Value};
use std::path::Path;

pub(super) const OPERATION: &str = "set_footprint_field_style";

pub(super) fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "board":      { "type": "string" },
            "references": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Reference designators of the footprints, e.g. ['R1', 'C4']. Omit for every footprint on the board." },
            "fields":     { "type": "array", "items": { "type": "string", "enum": ["reference", "value"] }, "minItems": 1, "default": ["reference"], "description": "Which fields of each footprint to restyle" },
            "size_x":     { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph width in mm" },
            "size_y":     { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph height in mm" },
            "thickness":  { "type": "number", "exclusiveMinimum": 0, "description": "Stroke thickness in mm" }
        },
        "required": ["board"]
    })
}

// ─── Arguments ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct Request {
    /// `None` covers every footprint on the board.
    references: Option<Vec<String>>,
    slots: Vec<IpcFootprintFieldSlot>,
    size_x_nm: Option<i64>,
    size_y_nm: Option<i64>,
    thickness_nm: Option<i64>,
}

impl Request {
    fn edit(&self) -> IpcTextEdit {
        IpcTextEdit {
            size_x_nm: self.size_x_nm,
            size_y_nm: self.size_y_nm,
            stroke_width_nm: self.thickness_nm,
            ..Default::default()
        }
    }
}

/// A non-empty array of distinct strings, each mapped by `parse`. A repeated
/// entry is refused rather than collapsed: on the live path it would send the
/// same footprint twice in one update.
fn distinct_list<T: PartialEq>(
    args: &Value,
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
    expected: &str,
) -> Result<Option<Vec<T>>, CallToolResult> {
    let items = match &args[key] {
        Value::Null => return Ok(None),
        Value::Array(items) => items,
        _ => return Err(invalid_arg(key, &format!("must be an array of {expected}"))),
    };
    if items.is_empty() {
        return Err(invalid_arg(
            key,
            "must not be empty; omit it for the default",
        ));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let parsed = item
            .as_str()
            .and_then(&parse)
            .ok_or_else(|| invalid_arg(key, &format!("every entry must be {expected}")))?;
        if out.contains(&parsed) {
            return Err(invalid_arg(key, &format!("names {item} more than once")));
        }
        out.push(parsed);
    }
    Ok(Some(out))
}

fn parse_request(args: &Value) -> Result<Request, CallToolResult> {
    let references = distinct_list(
        args,
        "references",
        |reference| (!reference.trim().is_empty()).then(|| reference.to_string()),
        "a non-empty reference designator",
    )?;
    let slots = distinct_list(
        args,
        "fields",
        |field| match field {
            "reference" => Some(IpcFootprintFieldSlot::Reference),
            "value" => Some(IpcFootprintFieldSlot::Value),
            _ => None,
        },
        "'reference' or 'value'",
    )?
    .unwrap_or_else(|| vec![IpcFootprintFieldSlot::Reference]);
    let request = Request {
        references,
        slots,
        size_x_nm: positive_length_nm(args, "size_x", Some(MAX_TEXT_SIZE_MM))?,
        size_y_nm: positive_length_nm(args, "size_y", Some(MAX_TEXT_SIZE_MM))?,
        thickness_nm: positive_length_nm(args, "thickness", None)?,
    };
    if request.edit().is_empty() {
        return Err(invalid_arg(
            "edit",
            "name at least one property to change: size_x, size_y, thickness",
        ));
    }
    Ok(request)
}

// ─── Result ───────────────────────────────────────────────────────────────────

/// The three values this tool sets, as one field holds them. `thickness_nm`
/// is `None` only on the file path, for a font that stores no thickness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Style {
    size_x_nm: i64,
    size_y_nm: i64,
    thickness_nm: Option<i64>,
}

impl Style {
    fn json(self) -> Value {
        json!({
            "size_x_mm": mm(self.size_x_nm),
            "size_y_mm": mm(self.size_y_nm),
            "thickness_mm": self.thickness_nm.map(mm),
        })
    }
}

fn live_style(field: &IpcFootprintField) -> Value {
    match &field.state {
        Some(state) => Style {
            size_x_nm: state.size_x_nm,
            size_y_nm: state.size_y_nm,
            thickness_nm: Some(state.stroke_width_nm),
        }
        .json(),
        None => Value::Null,
    }
}

fn entry(footprint: &str, field: &str, before: Value, after: Value) -> Value {
    json!({
        "footprint": footprint,
        "field": field,
        "changed": before != after,
        "before": before,
        "after": after,
    })
}

/// The success body both paths share, so a caller reads one shape whichever
/// ran and `source` names it. `wrote` is whether anything was sent or
/// written, which is what makes the status `applied`. `fallback_reason` and
/// `warning` are null here; the file path fills them in.
fn settled_body(source: &str, wrote: bool, fields: Vec<Value>, skipped: Vec<Value>) -> Value {
    let changed = fields
        .iter()
        .filter(|field| field["changed"] == true)
        .count();
    json!({
        "status": if wrote { "applied" } else { "unchanged" },
        "applied": wrote,
        "source": source,
        "count": fields.len(),
        "changed_count": changed,
        "fields": fields,
        "skipped": skipped,
        "undo": if wrote && source == "ipc" { json!("one KiCad undo step") } else { Value::Null },
        "fallback_reason": Value::Null,
        "warning": Value::Null,
    })
}

fn uncertain(board: &Path, reason: String, evidence: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: OPERATION.to_string(),
            path: board.display().to_string(),
            reason: reason.clone(),
        },
        format!(
            "{OPERATION} may have changed the board, but a fresh readback did not prove the \
             requested result. Inspect before retrying. {reason}{evidence}"
        ),
    )
}

fn live_result(outcome: IpcFootprintFieldBatchOutcome, board: &Path) -> CallToolResult {
    match outcome {
        IpcFootprintFieldBatchOutcome::Settled {
            fields,
            skipped,
            sent,
        } => {
            let fields = fields
                .iter()
                .map(|change| {
                    entry(
                        &change.before.footprint_reference,
                        &change.before.slot,
                        live_style(&change.before),
                        live_style(&change.after),
                    )
                })
                .collect();
            let skipped = skipped
                .iter()
                .map(|skip| {
                    json!({
                        "footprint": skip.footprint_reference,
                        "field": skip.slot,
                        "reason": skip.reason,
                    })
                })
                .collect();
            CallToolResult::json(&settled_body("ipc", sent, fields, skipped))
        }
        IpcFootprintFieldBatchOutcome::Uncertain { mismatches, reason } => uncertain(
            board,
            if mismatches.is_empty() {
                reason
            } else {
                format!("{reason}; {}", mismatches.join("; "))
            },
            " If it did change, it is one KiCad undo step.",
        ),
    }
}

// ─── Saved-file path ──────────────────────────────────────────────────────────

/// Whether `block` is a `(tag …)` list, matching the whole tag.
fn tagged(block: &str, tag: &str) -> bool {
    block
        .strip_prefix('(')
        .and_then(|rest| rest.strip_prefix(tag))
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == ')'))
}

/// The byte range, within `block`, of its first direct `(tag …)` child.
/// `block` must itself be a `(parent …)` list.
fn child(block: &str, parent: &str, tag: &str) -> Option<(usize, usize)> {
    find_direct_child_blocks(block, parent)
        .into_iter()
        .find(|&(start, end)| tagged(&block[start..end], tag))
}

/// Whole nanometres as the millimetre number KiCad writes: no exponent, no
/// trailing zeros (`1`, `0.15`, `1.27`).
fn mm_text(nm: i64) -> String {
    let (whole, fraction) = (nm.unsigned_abs() / 1_000_000, nm.unsigned_abs() % 1_000_000);
    let sign = if nm < 0 { "-" } else { "" };
    if fraction == 0 {
        format!("{sign}{whole}")
    } else {
        let digits = format!("{fraction:06}");
        format!("{sign}{whole}.{}", digits.trim_end_matches('0'))
    }
}

fn to_nm(mm: f64) -> i64 {
    (mm * 1_000_000.0).round() as i64
}

/// One field of one footprint in the saved board: what its font holds, what
/// it should hold, and the edit that gets it there (none when they agree).
#[derive(Debug)]
struct FileField {
    footprint: String,
    slot: IpcFootprintFieldSlot,
    before: Style,
    after: Style,
    edit: Option<SexpEdit>,
}

#[derive(Debug, Default)]
struct FilePlan {
    fields: Vec<FileField>,
    skipped: Vec<Value>,
}

/// Read the style of the `(property …)` block starting at `offset` in
/// `content` and build the edit that applies `request` to it. `Err` says
/// which part of the block is missing.
fn plan_property(
    content: &str,
    offset: usize,
    property: &str,
    request: &Request,
) -> Result<(Style, Style, Option<SexpEdit>), String> {
    let (effects_start, effects_end) =
        child(property, "property", "effects").ok_or("the field has no (effects …)")?;
    let effects = &property[effects_start..effects_end];
    let (font_start, font_end) =
        child(effects, "effects", "font").ok_or("the field's effects have no (font …)")?;
    let font = &effects[font_start..font_end];
    let font_offset = offset + effects_start + font_start;
    let (size_start, size_end) =
        child(font, "font", "size").ok_or("the field's font has no (size …)")?;
    let number = |block: &str, index: usize| {
        parse_sexp(block)
            .ok()
            .and_then(|node| node.get_f64(index))
            .filter(|value| value.is_finite())
            .map(to_nm)
    };
    let size = &font[size_start..size_end];
    // Height first: KiCad writes `(size <height> <width>)`
    // (`common/eda_text.cpp:1050-1051`) and reads it back that way
    // (`pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp:783-785`).
    let (Some(size_y_nm), Some(size_x_nm)) = (number(size, 1), number(size, 2)) else {
        return Err(format!("the field's font size {size} is not two numbers"));
    };
    let thickness_span = child(font, "font", "thickness");
    let thickness_nm = match thickness_span {
        None => None,
        Some((start, end)) => Some(number(&font[start..end], 1).ok_or_else(|| {
            format!(
                "the field's font thickness {} is not a number",
                &font[start..end]
            )
        })?),
    };
    let before = Style {
        size_x_nm,
        size_y_nm,
        thickness_nm,
    };
    let after = Style {
        size_x_nm: request.size_x_nm.unwrap_or(size_x_nm),
        size_y_nm: request.size_y_nm.unwrap_or(size_y_nm),
        thickness_nm: request.thickness_nm.or(thickness_nm),
    };
    if before == after {
        return Ok((before, after, None));
    }

    // Height first, as KiCad writes it (see above).
    let size_text = format!(
        "(size {} {})",
        mm_text(after.size_y_nm),
        mm_text(after.size_x_nm)
    );
    let thickness_text = after
        .thickness_nm
        .map(|nm| format!("(thickness {})", mm_text(nm)));
    let (start, end, replacement) = match (thickness_span, thickness_text) {
        // One replacement from the size through the thickness, keeping
        // whatever separates them in the file.
        (Some((thickness_start, thickness_end)), Some(thickness_text))
            if thickness_start >= size_end =>
        {
            (
                size_start,
                thickness_end,
                format!(
                    "{size_text}{}{thickness_text}",
                    &font[size_end..thickness_start]
                ),
            )
        }
        (Some(_), _) => {
            return Err(
                "the field's font lists its thickness before its size, which KiCad \
                        does not write"
                    .to_string(),
            )
        }
        // KiCad writes the thickness right after the size, on its own line
        // at the same indentation. A font written on one line gets a space.
        (None, Some(thickness_text)) => {
            let absolute_size = font_offset + size_start;
            let line_start = content[..absolute_size].rfind('\n').map_or(0, |at| at + 1);
            let indent = &content[line_start..absolute_size];
            let separator = if line_start > 0 && indent.chars().all(|c| c == ' ' || c == '\t') {
                // The line ending the file already uses: a board saved on
                // Windows has CRLF.
                let newline = if content[..line_start].ends_with("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                format!("{newline}{indent}")
            } else {
                " ".to_string()
            };
            (
                size_start,
                size_end,
                format!("{size_text}{separator}{thickness_text}"),
            )
        }
        (None, None) => (size_start, size_end, size_text),
    };
    Ok((
        before,
        after,
        Some(SexpEdit::replace(
            font_offset + start,
            font_offset + end,
            replacement,
        )),
    ))
}

/// A `(footprint …)` of the saved board: its Reference value and each of its
/// `(property …)` blocks as (name, absolute offset, text).
struct FileFootprint<'a> {
    reference: Option<String>,
    properties: Vec<(String, usize, &'a str)>,
}

/// Find every field `request` covers in the saved board `content`.
///
/// The rules match the live path: a named reference must identify exactly one
/// footprint whose requested fields can all be edited, or nothing is planned;
/// on a whole-board run a field that cannot be edited is listed in `skipped`.
/// The errors are the live path's own types, so both paths refuse alike.
fn plan_file(content: &str, request: &Request) -> anyhow::Result<FilePlan> {
    let mut footprints: Vec<FileFootprint> = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_pcb") {
        let footprint = &content[start..end];
        if !tagged(footprint, "footprint") {
            continue;
        }
        let mut reference = None;
        let mut properties = Vec::new();
        for (property_start, property_end) in find_direct_child_blocks(footprint, "footprint") {
            let property = &footprint[property_start..property_end];
            if !tagged(property, "property") {
                continue;
            }
            let Ok(node) = parse_sexp(property) else {
                continue;
            };
            let Some(name) = node.get(1).and_then(|name| name.as_str()) else {
                continue;
            };
            if name == "Reference" {
                reference = node
                    .get(2)
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
            properties.push((name.to_string(), start + property_start, property));
        }
        footprints.push(FileFootprint {
            reference,
            properties,
        });
    }

    let targets: Vec<&FileFootprint> = match &request.references {
        None => footprints.iter().collect(),
        Some(references) => {
            let mut targets = Vec::with_capacity(references.len());
            for reference in references {
                let matches: Vec<_> = footprints
                    .iter()
                    .filter(|held| held.reference.as_deref() == Some(reference.as_str()))
                    .collect();
                match matches.as_slice() {
                    [one] => targets.push(*one),
                    [] => {
                        return Err(FootprintTargetError::Missing {
                            reference: reference.clone(),
                        }
                        .into())
                    }
                    more => {
                        return Err(FootprintTargetError::Ambiguous {
                            reference: reference.clone(),
                            matches: more.len(),
                        }
                        .into())
                    }
                }
            }
            targets
        }
    };

    let mut plan = FilePlan::default();
    for FileFootprint {
        reference,
        properties,
    } in targets
    {
        let reference = reference.clone().unwrap_or_default();
        for &slot in &request.slots {
            let name = match slot {
                IpcFootprintFieldSlot::Value => "Value",
                _ => "Reference",
            };
            let planned = match properties.iter().find(|(held, _, _)| held == name) {
                None => Err(TextTargetError::MissingField {
                    reference: reference.clone(),
                    field: slot.as_str().to_string(),
                }),
                Some((_, offset, property)) => plan_property(content, *offset, property, request)
                    .map_err(|reason| TextTargetError::Unreadable {
                        target: format!("the {} field of '{reference}'", slot.as_str()),
                        reason,
                    }),
            };
            match planned {
                Ok((before, after, edit)) => plan.fields.push(FileField {
                    footprint: reference.clone(),
                    slot,
                    before,
                    after,
                    edit,
                }),
                Err(problem) if request.references.is_some() => return Err(problem.into()),
                Err(problem) => plan.skipped.push(json!({
                    "footprint": reference,
                    "field": slot.as_str(),
                    "reason": problem.to_string(),
                })),
            }
        }
    }
    Ok(plan)
}

/// Edit the saved board. The write goes through only while the file still
/// holds the bytes the plan was built from, and the result is a second parse
/// of the file, not the plan echoed back.
fn edit_file(board: &Path, request: &Request) -> anyhow::Result<Result<Value, CallToolResult>> {
    let content = read_consistent(board)?;
    let mut plan = match plan_file(&content, request) {
        Ok(plan) => plan,
        Err(error) => return Ok(Err(live_error_result(error, OPERATION))),
    };
    let edits: Vec<SexpEdit> = plan
        .fields
        .iter_mut()
        .filter_map(|field| field.edit.take())
        .collect();
    let wrote = !edits.is_empty();
    if wrote {
        if let Some(conflict) = write_board_edits(board, OPERATION, content, edits)? {
            return Ok(Err(conflict));
        }
    }

    // After a write every planned field must read back as already holding
    // the request: same fields, in the same order, with nothing left to edit.
    let observed = if wrote {
        let reread = read_consistent(board).map_err(anyhow::Error::from);
        match reread.and_then(|content| plan_file(&content, request)) {
            Ok(observed) => {
                let proven = observed.fields.len() == plan.fields.len()
                    && observed
                        .fields
                        .iter()
                        .zip(&plan.fields)
                        .all(|(seen, planned)| {
                            seen.footprint == planned.footprint
                                && seen.slot == planned.slot
                                && seen.before == planned.after
                                && seen.before == seen.after
                        });
                if !proven {
                    return Ok(Err(uncertain(
                        board,
                        "the board was written, but reading it back did not show the requested \
                         size and thickness on every field"
                            .to_string(),
                        "",
                    )));
                }
                observed.fields.iter().map(|field| field.before).collect()
            }
            Err(error) => {
                return Ok(Err(uncertain(
                    board,
                    format!("the board was written, but reading it back failed: {error:#}"),
                    "",
                )))
            }
        }
    } else {
        plan.fields
            .iter()
            .map(|field| field.before)
            .collect::<Vec<_>>()
    };

    let fields = plan
        .fields
        .iter()
        .zip(observed)
        .map(|(field, after)| {
            entry(
                &field.footprint,
                field.slot.as_str(),
                field.before.json(),
                after.json(),
            )
        })
        .collect();
    Ok(Ok(settled_body("file", wrote, fields, plan.skipped)))
}

// ─── Handler ──────────────────────────────────────────────────────────────────

pub(super) async fn handle_set_footprint_field_style(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let request = match parse_request(args) {
        Ok(request) => request,
        Err(result) => return Ok(result),
    };

    let live = request.clone();
    let fallback_reason = match attempt_ipc_write_in(
        ctx,
        &board,
        "footprint field style",
        // The outcome travels inside `Ok` so a typed refusal (an unknown
        // reference, a missing field) reaches the caller as itself; the
        // transport classifier would keep only its message.
        move |client, document| {
            Ok(client.edit_footprint_fields_in(
                document,
                live.references.as_deref(),
                &live.slots,
                &live.edit(),
            ))
        },
    )
    .await?
    {
        BoardWrite::Ipc(Ok(outcome)) => return Ok(live_result(outcome, &board)),
        BoardWrite::Ipc(Err(error)) => return Ok(live_error_result(error, OPERATION)),
        BoardWrite::Refused(result) => return Ok(result),
        BoardWrite::File(reason) => reason,
    };

    let mut body = match edit_file(&board, &request)? {
        Ok(body) => body,
        Err(result) => return Ok(result),
    };
    body["fallback_reason"] = fallback_reason.evidence();
    body["warning"] = json!(fallback_reason.warning());
    Ok(CallToolResult::json(&body))
}

#[cfg(test)]
#[path = "pcb_text_field_style_tests.rs"]
mod tests;
