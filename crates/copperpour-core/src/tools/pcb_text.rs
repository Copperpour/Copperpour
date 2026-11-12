//! Live reads and in-place edits of placed text: a footprint's reference and
//! value fields, and board-level text items.
//!
//! These exist for silkscreen clean-up. A DRC `silk_over_copper` finding is
//! fixed by moving, rotating, shrinking, or hiding the offending text, and
//! before these tools the only route for board text was delete-and-recreate,
//! which loses the font face, a non-square glyph size, and anything else
//! `add_board_text` does not model.
//!
//! Everything here is live-IPC only. Positions are board-absolute
//! millimetres: KiCad carries a footprint's fields in board coordinates, not
//! relative to the footprint anchor (see `copperpour_ipc::transform`). Angles are
//! the text's own absolute angle as KiCad holds it, which for a field already
//! includes the footprint's orientation.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{
    get_path, invalid_arg, ipc_recovered_result, ipc_target_error_result, ipc_uncertain_result,
    require_str, with_bound_board_ipc_classified, BoardAccess, BoardBinding, ToolContext, ToolDef,
};
use copperpour_ipc::client::{FootprintTargetError, KiCadIpcClient, TextTargetError};
use copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier;
use copperpour_ipc::{
    IpcBoardTextItem, IpcBox2, IpcFootprintField, IpcFootprintFieldSlot, IpcHorizontalAlignment,
    IpcTextEdit, IpcTextEditOutcome, IpcTextState, IpcVerticalAlignment,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// KiCad's largest text glyph (`TEXT_MAX_SIZE_MM`). A larger request would be
/// clamped on the way in and then fail its own readback.
const MAX_TEXT_SIZE_MM: f64 = 250.0;

const EDITABLE_FIELD_PROPERTIES: &str =
    "x and y, rotation, size_x, size_y, thickness, visible, layer, mirrored (or mirror 'auto'), \
     horizontal_alignment, vertical_alignment, bold, italic, keep_upright, knockout, locked, font";
const EDITABLE_TEXT_PROPERTIES: &str =
    "x and y, rotation, size_x, size_y, thickness, layer, text, mirrored (or mirror 'auto'), \
     horizontal_alignment, vertical_alignment, bold, italic, keep_upright, knockout, locked, font";

/// The style properties `edit_board_text` and `edit_footprint_field` share,
/// added to each tool's own schema properties.
fn with_style_properties(mut schema: Value) -> Value {
    let style = json!({
        "mirrored":             { "type": "boolean", "description": "Draw the glyphs mirrored. Text on a back layer (B.SilkS, B.Fab, B.Cu) must be mirrored to read correctly from the back of the board" },
        "mirror":               { "type": "string", "enum": ["keep", "auto"], "default": "keep", "description": "'auto' sets 'mirrored' from the side of the layer the text ends on: true on a back layer (B.*), false on a front layer (F.*), unchanged on a layer with no side. That layer is 'layer' when given, otherwise the text's current layer. Position and angle are not changed. Cannot be combined with 'mirrored'" },
        "horizontal_alignment": { "type": "string", "enum": ["left", "center", "right"], "description": "Which side of the text its position anchors" },
        "vertical_alignment":   { "type": "string", "enum": ["top", "center", "bottom"] },
        "bold":                 { "type": "boolean", "description": "Sets the bold flag only; the stroke is changed with 'thickness'" },
        "italic":               { "type": "boolean" },
        "keep_upright":         { "type": "boolean", "description": "Let KiCad draw the text within 90 degrees of upright whatever its angle" },
        "knockout":             { "type": "boolean", "description": "Draw the text as a cut-out in a filled box" },
        "locked":               { "type": "boolean" },
        "font":                 { "type": "string", "description": "Font face name, e.g. 'Arial'; the empty string selects KiCad's stroke font" }
    });
    let properties = schema["properties"]
        .as_object_mut()
        .expect("a text edit schema has properties");
    for (key, value) in style.as_object().expect("an object literal") {
        properties.insert(key.clone(), value.clone());
    }
    schema
}

pub(crate) fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "list_footprint_fields",
            "List the fields of one placed footprint (by 'reference') or of every footprint: \
             reference, value, datasheet, description and any user fields, each with its text, \
             board-absolute position in mm, absolute angle in degrees (a field's angle already \
             includes its footprint's rotation), glyph size, stroke thickness, layer, visibility, \
             alignment, keep_upright/mirrored, the field's KIID, and the axis-aligned bounding \
             box KiCad measures for it in board mm. Use it to place silkscreen text clear of \
             pads, then edit with edit_footprint_field. Requires KiCad running with the board \
             open.",
            json!({
                "type": "object",
                "properties": {
                    "board":        { "type": "string" },
                    "reference":    { "type": "string", "description": "Only this footprint, e.g. 'R12'. Omit for every footprint on the board." },
                    "layer":        { "type": "string", "description": "Only fields on this layer, e.g. 'F.SilkS' (optional)" },
                    "visible_only": { "type": "boolean", "default": false, "description": "Leave out hidden fields" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_list_footprint_fields(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
        tool!(
            "edit_footprint_field",
            "Move, rotate, resize, hide/show, restyle (mirrored, alignment, bold, italic, \
             keep_upright, knockout, locked, font), or change the layer of a placed footprint's \
             reference or value field in place. Name at least one property; every property not \
             named is sent back exactly as KiCad holds it. x/y are board-absolute mm (both or \
             neither); rotation is the field's absolute angle in degrees, the same frame \
             list_footprint_fields reports. Changing the layer alone does not mirror the text: \
             pass mirror 'auto' with it to mirror on a back layer and unmirror on a front one, \
             or set 'mirrored' yourself. One KiCad undo step; the result is a fresh \
             readback of what the board holds, and a readback that does not match the request \
             (for example a KiCad that ignores one of the properties) is reported as an error \
             rather than success. Does not save. Requires KiCad running with the board open.",
            with_style_properties(json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string", "description": "Reference designator of the footprint, e.g. 'R12'" },
                    "field":     { "type": "string", "enum": ["reference", "value"], "default": "reference" },
                    "x":         { "type": "number", "description": "Board-absolute X in mm; requires y" },
                    "y":         { "type": "number", "description": "Board-absolute Y in mm; requires x" },
                    "rotation":  { "type": "number", "description": "Absolute text angle in degrees" },
                    "size_x":    { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph width in mm" },
                    "size_y":    { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph height in mm" },
                    "thickness": { "type": "number", "exclusiveMinimum": 0, "description": "Stroke thickness in mm" },
                    "visible":   { "type": "boolean" },
                    "layer":     { "type": "string", "description": "Target layer, e.g. 'F.Fab'" }
                },
                "required": ["board", "reference"]
            })),
            |args, ctx| async move { handle_edit_footprint_field(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
        tool!(
            "list_board_texts",
            "List the board-level text items (not footprint fields, not text boxes) with the \
             UUID edit_board_text needs, the raw text including any ${VARIABLE}, board-absolute \
             position in mm, angle, glyph size (x and y can differ), stroke thickness, font face \
             (empty for KiCad's stroke font), layer, alignment, mirrored/knockout/locked, and the \
             axis-aligned bounding box KiCad measures in board mm. Requires KiCad running with \
             the board open.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string" },
                    "layer": { "type": "string", "description": "Only text on this layer, e.g. 'F.SilkS' (optional)" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_list_board_texts(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
        tool!(
            "edit_board_text",
            "Move, rotate, resize, re-layer, re-word, or restyle (mirrored, alignment, bold, \
             italic, keep_upright, knockout, locked, font) one board-level text item in place, \
             selected by UUID (from list_board_texts). Name at least one property; everything \
             not named — font face, non-square size, alignment, the raw text with any \
             ${VARIABLE} — is sent back exactly as KiCad holds it. Changing the layer alone \
             does not mirror the text: pass mirror 'auto' with it to mirror on a back layer and \
             unmirror on a front one, or set 'mirrored' yourself. When 'text' is omitted \
             the live text is checked against the saved board, and the edit is refused if a text \
             variable the saved file holds is missing from it; pass 'text' with the exact raw \
             string to write it verbatim. One KiCad undo step; the result is a fresh readback, \
             and a mismatch (for example a KiCad that ignores one of the properties) is reported \
             as an error. Does not save. Requires KiCad running with the board open.",
            with_style_properties(json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "uuid":      { "type": "string", "description": "KIID of the board text, from list_board_texts" },
                    "x":         { "type": "number", "description": "Board-absolute X in mm; requires y" },
                    "y":         { "type": "number", "description": "Board-absolute Y in mm; requires x" },
                    "rotation":  { "type": "number", "description": "Text angle in degrees" },
                    "size_x":    { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph width in mm" },
                    "size_y":    { "type": "number", "exclusiveMinimum": 0, "maximum": MAX_TEXT_SIZE_MM, "description": "Glyph height in mm" },
                    "thickness": { "type": "number", "exclusiveMinimum": 0, "description": "Stroke thickness in mm" },
                    "layer":     { "type": "string", "description": "Target layer, e.g. 'B.SilkS'" },
                    "text":      { "type": "string", "description": "Replacement raw text, written verbatim (variables are not expanded)" }
                },
                "required": ["board", "uuid"]
            })),
            |args, ctx| async move { handle_edit_board_text(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
    ]
}

// ─── Argument parsing ─────────────────────────────────────────────────────────

/// An optional number: absent or `null` is `None`; anything else that is not
/// a number is refused rather than treated as absent.
fn opt_number(args: &Value, key: &str) -> Result<Option<f64>, CallToolResult> {
    match &args[key] {
        Value::Null => Ok(None),
        value => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| invalid_arg(key, "must be a finite number")),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, CallToolResult> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(*value)),
        _ => Err(invalid_arg(key, "must be true or false")),
    }
}

fn opt_string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, CallToolResult> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.as_str())),
        _ => Err(invalid_arg(key, "must be a string")),
    }
}

/// Millimetres to KiCad's integer nanometres, rounded rather than truncated
/// (`1.2 * 1e6` is `1199999.999…`), and range-checked before narrowing: KiCad
/// board coordinates are 32-bit nanometres.
fn mm_to_nm(key: &str, mm: f64) -> Result<i64, CallToolResult> {
    let nm = (mm * 1_000_000.0).round();
    if !nm.is_finite() || nm.abs() > f64::from(i32::MAX) {
        return Err(invalid_arg(
            key,
            "is outside KiCad's board coordinate range (about ±2147 mm)",
        ));
    }
    Ok(nm as i64)
}

/// A strictly positive length in mm, as whole nanometres.
fn positive_length_nm(
    args: &Value,
    key: &str,
    max_mm: Option<f64>,
) -> Result<Option<i64>, CallToolResult> {
    let Some(mm) = opt_number(args, key)? else {
        return Ok(None);
    };
    if mm <= 0.0 {
        return Err(invalid_arg(key, "must be greater than zero"));
    }
    if let Some(max_mm) = max_mm {
        if mm > max_mm {
            return Err(invalid_arg(
                key,
                &format!("must not exceed {max_mm} mm, KiCad's largest text size"),
            ));
        }
    }
    let nm = mm_to_nm(key, mm)?;
    if nm < 1 {
        return Err(invalid_arg(key, "rounds to zero nanometres"));
    }
    Ok(Some(nm))
}

/// Read the editable properties of a text item. `visible` is accepted only
/// for footprint fields and `text` only for board text; the schema gate
/// refuses the other already, and this keeps a direct call honest too.
fn parse_text_edit(
    args: &Value,
    allow_visible: bool,
    allow_text: bool,
) -> Result<IpcTextEdit, CallToolResult> {
    let x = opt_number(args, "x")?;
    let y = opt_number(args, "y")?;
    let position_nm = match (x, y) {
        (Some(x), Some(y)) => Some((mm_to_nm("x", x)?, mm_to_nm("y", y)?)),
        (None, None) => None,
        (Some(_), None) => {
            return Err(invalid_arg(
                "y",
                "is required with x: a position needs both coordinates",
            ))
        }
        (None, Some(_)) => {
            return Err(invalid_arg(
                "x",
                "is required with y: a position needs both coordinates",
            ))
        }
    };
    let angle_degrees = opt_number(args, "rotation")?;
    let size_x_nm = positive_length_nm(args, "size_x", Some(MAX_TEXT_SIZE_MM))?;
    let size_y_nm = positive_length_nm(args, "size_y", Some(MAX_TEXT_SIZE_MM))?;
    let stroke_width_nm = positive_length_nm(args, "thickness", None)?;
    let visible = opt_bool(args, "visible")?;
    if visible.is_some() && !allow_visible {
        return Err(invalid_arg(
            "visible",
            "board text cannot be hidden; only footprint fields have a visibility flag",
        ));
    }
    let layer = match opt_string(args, "layer")? {
        None => None,
        Some(layer) => {
            if let Err(error) = copperpour_ipc::builders::try_layer_from_name(layer) {
                return Err(invalid_arg("layer", &format!("{error:#}")));
            }
            Some(layer.to_string())
        }
    };
    let text = match opt_string(args, "text")? {
        None => None,
        Some(_) if !allow_text => {
            return Err(invalid_arg(
                "text",
                "a field's text is not edited here; use edit_component for the value",
            ))
        }
        Some("") => return Err(invalid_arg("text", "must not be empty")),
        Some(text) if text.contains('\0') => {
            return Err(invalid_arg("text", "must not contain NUL"))
        }
        Some(text) => Some(text.to_string()),
    };
    let horizontal_alignment = match opt_string(args, "horizontal_alignment")? {
        None => None,
        Some(name) => Some(IpcHorizontalAlignment::from_name(name).ok_or_else(|| {
            invalid_arg(
                "horizontal_alignment",
                "must be 'left', 'center' or 'right'",
            )
        })?),
    };
    let vertical_alignment = match opt_string(args, "vertical_alignment")? {
        None => None,
        Some(name) => Some(IpcVerticalAlignment::from_name(name).ok_or_else(|| {
            invalid_arg("vertical_alignment", "must be 'top', 'center' or 'bottom'")
        })?),
    };
    let font_name = match opt_string(args, "font")? {
        None => None,
        // A face name is one line of printable text. KiCad stores whatever
        // it is given, so anything else would come back intact and "prove"
        // an edit that names no font at all.
        Some(font) if font.chars().any(char::is_control) => {
            return Err(invalid_arg("font", "must not contain control characters"))
        }
        Some(font) if font != font.trim() => {
            return Err(invalid_arg("font", "must not start or end with whitespace"))
        }
        Some(font) => Some(font.to_string()),
    };
    let mirrored = opt_bool(args, "mirrored")?;
    let mirror_to_match_layer = match opt_string(args, "mirror")? {
        None | Some("keep") => false,
        Some("auto") if mirrored.is_some() => {
            return Err(invalid_arg(
                "mirror",
                "'auto' cannot be combined with 'mirrored': one of them decides the flag",
            ))
        }
        Some("auto") => true,
        Some(_) => return Err(invalid_arg("mirror", "must be 'keep' or 'auto'")),
    };
    let edit = IpcTextEdit {
        position_nm,
        angle_degrees,
        size_x_nm,
        size_y_nm,
        stroke_width_nm,
        layer,
        text,
        visible,
        mirrored,
        mirror_to_match_layer,
        horizontal_alignment,
        vertical_alignment,
        bold: opt_bool(args, "bold")?,
        italic: opt_bool(args, "italic")?,
        keep_upright: opt_bool(args, "keep_upright")?,
        knockout: opt_bool(args, "knockout")?,
        locked: opt_bool(args, "locked")?,
        font_name,
    };
    if edit.is_empty() {
        let properties = if allow_visible {
            EDITABLE_FIELD_PROPERTIES
        } else {
            EDITABLE_TEXT_PROPERTIES
        };
        return Err(invalid_arg(
            "edit",
            &format!("name at least one property to change: {properties}"),
        ));
    }
    Ok(edit)
}

fn parse_field_slot(args: &Value) -> Result<IpcFootprintFieldSlot, CallToolResult> {
    match opt_string(args, "field")? {
        None | Some("reference") => Ok(IpcFootprintFieldSlot::Reference),
        Some("value") => Ok(IpcFootprintFieldSlot::Value),
        Some(_) => Err(invalid_arg("field", "must be 'reference' or 'value'")),
    }
}

fn non_empty_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, CallToolResult> {
    let value = require_str(args, key)?;
    if value.trim().is_empty() {
        return Err(invalid_arg(key, "must not be empty"));
    }
    Ok(value)
}

// ─── Output ───────────────────────────────────────────────────────────────────

fn mm(nm: i64) -> f64 {
    nm as f64 / 1_000_000.0
}

fn box_json(bounds: Option<IpcBox2>) -> Value {
    match bounds {
        Some(bounds) => json!({
            "min_x_mm": mm(bounds.min_x_nm),
            "min_y_mm": mm(bounds.min_y_nm),
            "max_x_mm": mm(bounds.max_x_nm),
            "max_y_mm": mm(bounds.max_y_nm),
        }),
        None => Value::Null,
    }
}

fn state_fields(state: &IpcTextState, out: &mut Map<String, Value>) {
    let entries = [
        ("text", json!(state.text)),
        ("x_mm", json!(mm(state.x_nm))),
        ("y_mm", json!(mm(state.y_nm))),
        ("angle_degrees", json!(state.angle_degrees)),
        ("size_x_mm", json!(mm(state.size_x_nm))),
        ("size_y_mm", json!(mm(state.size_y_nm))),
        ("thickness_mm", json!(mm(state.stroke_width_nm))),
        ("layer", json!(state.layer)),
        ("font_name", json!(state.font_name)),
        ("horizontal_alignment", json!(state.horizontal_alignment)),
        ("vertical_alignment", json!(state.vertical_alignment)),
        ("keep_upright", json!(state.keep_upright)),
        ("mirrored", json!(state.mirrored)),
        ("italic", json!(state.italic)),
        ("bold", json!(state.bold)),
        ("knockout", json!(state.knockout)),
        ("locked", json!(state.locked)),
    ];
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
}

fn field_json(field: &IpcFootprintField) -> Value {
    let mut out = Map::new();
    out.insert("footprint".into(), json!(field.footprint_reference));
    out.insert("footprint_kiid".into(), json!(field.footprint_kiid));
    out.insert("field".into(), json!(field.slot));
    out.insert("name".into(), json!(field.name));
    out.insert("kiid".into(), json!(field.kiid));
    out.insert("visible".into(), json!(field.visible));
    match &field.state {
        Some(state) => state_fields(state, &mut out),
        None => {
            out.insert("unreadable_reason".into(), json!(field.unreadable_reason));
        }
    }
    out.insert("bounding_box".into(), box_json(field.bounding_box));
    Value::Object(out)
}

fn board_text_json(text: &IpcBoardTextItem) -> Value {
    let mut out = Map::new();
    out.insert("uuid".into(), json!(text.kiid));
    match &text.state {
        Some(state) => state_fields(state, &mut out),
        None => {
            out.insert("unreadable_reason".into(), json!(text.unreadable_reason));
        }
    }
    out.insert("bounding_box".into(), box_json(text.bounding_box));
    Value::Object(out)
}

/// The keys whose values differ between two renderings of one item.
fn changed_keys(before: &Value, after: &Value) -> Vec<String> {
    let (Some(before), Some(after)) = (before.as_object(), after.as_object()) else {
        return Vec::new();
    };
    after
        .iter()
        .filter(|(key, value)| key.as_str() != "bounding_box" && before.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .collect()
}

fn bounding_box_evidence(unavailable: &Option<String>) -> Value {
    match unavailable {
        None => json!({ "available": true }),
        Some(reason) => json!({ "available": false, "reason": reason }),
    }
}

const COORDINATES: &str = "board-absolute millimetres; angle_degrees is the text's absolute angle \
                           as KiCad holds it (a field's angle includes its footprint's rotation); \
                           bounding_box is KiCad's axis-aligned box for the rendered text";

// ─── Live plumbing ────────────────────────────────────────────────────────────

/// Run `f` against the exact board KiCad has open. Binding failures become
/// their structured results here; `f` reports its own outcome inside `Ok`, so
/// typed refusals survive (the transport classifier keeps only messages).
async fn on_live_board<T, F>(
    ctx: &ToolContext,
    board: &Path,
    f: F,
) -> anyhow::Result<Result<T, CallToolResult>>
where
    T: Send + 'static,
    F: FnOnce(&KiCadIpcClient, DocumentSpecifier) -> T + Send + 'static,
{
    Ok(
        match with_bound_board_ipc_classified(ctx, board, move |client, document| {
            Ok(f(client, document))
        })
        .await?
        {
            Ok(BoardBinding::Bound(value)) => Ok(value),
            Ok(BoardBinding::Unserved { message, .. }) => Err(editor_unavailable(&message)),
            Err(copperpour_ipc::IpcFailure::Target { error, .. }) => {
                Err(ipc_target_error_result(&error))
            }
            Err(copperpour_ipc::IpcFailure::Recovered(message)) => {
                Err(ipc_recovered_result(&message))
            }
            Err(copperpour_ipc::IpcFailure::Uncertain(message)) => {
                Err(ipc_uncertain_result(&message))
            }
            Err(failure) => Err(editor_unavailable(failure.message())),
        },
    )
}

fn editor_unavailable(reason: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::EditorUnavailable {
            editor: "pcb".to_string(),
            reason: reason.to_string(),
        },
        format!("KiCad must be running with the requested board open: {reason}"),
    )
}

/// A refusal from the saved-file text-variable check.
#[derive(Debug)]
struct TextVariableRefusal {
    uuid: String,
    reason: String,
}

impl std::fmt::Display for TextVariableRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for TextVariableRefusal {}

/// Map an error from a live read or edit onto the structured taxonomy.
fn live_error_result(error: anyhow::Error, operation: &str) -> CallToolResult {
    if let Some(target) = error.downcast_ref::<FootprintTargetError>() {
        return match target {
            FootprintTargetError::Missing { reference } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: reference.clone(),
                    reason: "no placed footprint has this exact reference".to_string(),
                },
                format!("{target}. Nothing was changed."),
            ),
            FootprintTargetError::Ambiguous { reference, matches } => CallToolResult::error_kind(
                ToolErrorKind::AmbiguousTarget {
                    target: reference.clone(),
                    candidates: (0..*matches)
                        .map(|index| format!("match {index}"))
                        .collect(),
                },
                format!("{target}. Nothing was changed."),
            ),
        };
    }
    if let Some(target) = error.downcast_ref::<TextTargetError>() {
        return match target {
            TextTargetError::MissingField { reference, field } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: format!("{reference} {field} field"),
                    reason: "the footprint carries no such field".to_string(),
                },
                format!("{target}. Nothing was changed."),
            ),
            TextTargetError::MissingBoardText { uuid } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: uuid.clone(),
                    reason: "no board-level text has this KIID; list_board_texts shows the ones \
                             that do"
                        .to_string(),
                },
                format!("{target}. Nothing was changed."),
            ),
            TextTargetError::AmbiguousBoardText { uuid, matches } => CallToolResult::error_kind(
                ToolErrorKind::AmbiguousTarget {
                    target: uuid.clone(),
                    candidates: (0..*matches)
                        .map(|index| format!("match {index}"))
                        .collect(),
                },
                format!("{target}. Nothing was changed."),
            ),
            TextTargetError::Unreadable {
                target: what,
                reason,
            } => CallToolResult::error_kind(
                ToolErrorKind::HandlerError {
                    reason: format!("{what}: {reason}"),
                },
                format!("{target}. Nothing was changed, because the result could not be verified."),
            ),
        };
    }
    if let Some(refusal) = error.downcast_ref::<TextVariableRefusal>() {
        return CallToolResult::error_kind(
            ToolErrorKind::StaleTarget {
                target: format!("board text {}", refusal.uuid),
                reason: refusal.reason.clone(),
            },
            format!(
                "Refused before writing: {}. Pass 'text' with the exact raw string to keep (for \
                 example 'Rev ${{REVISION}}'), or save the board first if the variable was \
                 removed on purpose.",
                refusal.reason
            ),
        );
    }
    match copperpour_ipc::IpcFailure::from_error(error) {
        copperpour_ipc::IpcFailure::Target { error, .. } => ipc_target_error_result(&error),
        copperpour_ipc::IpcFailure::Recovered(message) => ipc_recovered_result(&message),
        copperpour_ipc::IpcFailure::Uncertain(message) => ipc_uncertain_result(&message),
        copperpour_ipc::IpcFailure::Unreachable(message) => editor_unavailable(&message),
        // A refusal Copperpour made before KiCad kept anything is worded as
        // the rejection it was before the variant existed.
        copperpour_ipc::IpcFailure::Rejected(message)
        | copperpour_ipc::IpcFailure::Preflight(copperpour_ipc::PreflightRefusal {
            reason: message,
            ..
        }) => CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: message.clone(),
            },
            format!("{operation} failed: {message}"),
        ),
    }
}

/// What `mirror: "auto"` decided, read from the state the edit settled on
/// rather than from the request. A layer with no side is said out loud:
/// otherwise an `unchanged` result would not tell the caller why.
fn mirror_auto_report<T>(
    outcome: &IpcTextEditOutcome<T>,
    state: fn(&T) -> Option<&IpcTextState>,
) -> Option<Value> {
    let settled = match outcome {
        IpcTextEditOutcome::Unchanged { current } => state(current),
        IpcTextEditOutcome::Applied { after, .. } => state(after),
        // An uncertain outcome is an error with its own evidence.
        IpcTextEditOutcome::Uncertain { .. } => None,
    }?;
    Some(
        match copperpour_ipc::mirrored_for_layer_name(&settled.layer) {
            Some(mirrored) => json!({
                "mode": "auto",
                "layer": settled.layer,
                "side": if mirrored { "back" } else { "front" },
            }),
            None => json!({
                "mode": "auto",
                "layer": settled.layer,
                "side": "none",
                "note": "this layer has no front or back side, so 'mirrored' was left as it is",
            }),
        },
    )
}

/// Turn an edit outcome into the tool result. `render` draws one item.
fn edit_outcome_result<T>(
    outcome: IpcTextEditOutcome<T>,
    operation: &str,
    board: &Path,
    render: impl Fn(&T) -> Value,
    mut extra: Map<String, Value>,
) -> CallToolResult {
    match outcome {
        IpcTextEditOutcome::Unchanged { current } => {
            let current = render(&current);
            extra.insert("status".into(), json!("unchanged"));
            extra.insert("applied".into(), json!(false));
            extra.insert("source".into(), json!("ipc"));
            extra.insert("changed".into(), json!([]));
            extra.insert("before".into(), current.clone());
            extra.insert("after".into(), current);
            extra.insert("undo".into(), Value::Null);
            extra.insert("coordinates".into(), json!(COORDINATES));
            CallToolResult::json(&Value::Object(extra))
        }
        IpcTextEditOutcome::Applied { before, after } => {
            let (before, after) = (render(&before), render(&after));
            extra.insert("status".into(), json!("applied"));
            extra.insert("applied".into(), json!(true));
            extra.insert("source".into(), json!("ipc"));
            extra.insert("changed".into(), json!(changed_keys(&before, &after)));
            extra.insert("before".into(), before);
            extra.insert("after".into(), after);
            extra.insert("undo".into(), json!("one KiCad undo step"));
            extra.insert("coordinates".into(), json!(COORDINATES));
            CallToolResult::json(&Value::Object(extra))
        }
        IpcTextEditOutcome::Uncertain {
            expected,
            observed,
            mismatches,
            reason,
            ..
        } => {
            let detail = json!({
                "expected": render(&expected),
                "observed": observed.as_ref().map(&render),
                "mismatches": mismatches,
            });
            CallToolResult::error_kind(
                ToolErrorKind::MutationOutcomeUncertain {
                    operation: operation.to_string(),
                    path: board.display().to_string(),
                    reason: if mismatches.is_empty() {
                        reason.clone()
                    } else {
                        format!("{reason}; {}", mismatches.join("; "))
                    },
                },
                format!(
                    "KiCad may have changed the board, but a fresh readback did not prove the \
                     requested result. If it did change, it is one KiCad undo step. Inspect \
                     before retrying. {reason}. Evidence: {detail}"
                ),
            )
        }
    }
}

// ─── Saved-file text-variable check ──────────────────────────────────────────

/// Every `${…}` variable reference and `@{…}` expression in `text`.
fn variable_tokens(text: &str) -> BTreeSet<&str> {
    let mut tokens = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if (bytes[index] == b'$' || bytes[index] == b'@') && bytes[index + 1] == b'{' {
            if let Some(close) = text[index..].find('}') {
                tokens.insert(&text[index..index + close + 1]);
                index += close + 1;
                continue;
            }
        }
        index += 1;
    }
    tokens
}

/// The raw text the saved board holds for the board-level text `uuid`:
/// `Ok(None)` when the file has no such item (added since the last save) or
/// does not exist yet.
fn saved_board_text(board: &Path, uuid: &str) -> Result<Option<String>, String> {
    let content = match std::fs::read_to_string(board) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    for (start, end) in copperpour_sexp::writer::find_direct_child_blocks(&content, "kicad_pcb") {
        let block = &content[start..end];
        // Exactly `(gr_text`, not `(gr_text_box`.
        let Some(rest) = block.strip_prefix("(gr_text") else {
            continue;
        };
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let Ok(node) = copperpour_sexp::parse_sexp(block) else {
            continue;
        };
        let id = node.find_str("uuid").or_else(|| node.find_str("tstamp"));
        if id == Some(uuid) {
            return Ok(node
                .get(1)
                .and_then(|text| text.as_str())
                .map(str::to_string));
        }
    }
    Ok(None)
}

/// Decide whether writing `live_text` back unchanged could drop a text
/// variable, using the saved board as independent evidence.
///
/// KiCad 10 sends a text's raw string, so `${REVISION}` should arrive intact.
/// This does not take that on trust: if the saved board holds a variable that
/// the live text no longer contains, the live text may be an expansion, and
/// writing it back would replace the variable with its current value — so
/// the edit is refused. Returns the evidence label on success.
fn text_variable_check(board: &Path, uuid: &str, live_text: &str) -> Result<String, String> {
    match saved_board_text(board, uuid) {
        Ok(Some(saved)) if saved == live_text => Ok("matches_saved_file".to_string()),
        Ok(Some(saved)) => {
            let live = variable_tokens(live_text);
            let lost: Vec<&str> = variable_tokens(&saved)
                .into_iter()
                .filter(|token| !live.contains(token))
                .collect();
            if lost.is_empty() {
                Ok("differs_from_saved_file_without_losing_variables".to_string())
            } else {
                Err(format!(
                    "the saved board holds this text as {saved:?}, but KiCad sent {live_text:?}, \
                     which lacks {}; writing it back could replace a text variable with its \
                     current value",
                    lost.join(", ")
                ))
            }
        }
        Ok(None) => Ok("not_in_saved_file".to_string()),
        Err(error) => Ok(format!("saved_file_unreadable: {error}")),
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_list_footprint_fields(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match opt_string(args, "reference") {
        Ok(Some(reference)) if reference.trim().is_empty() => {
            return Ok(invalid_arg("reference", "must not be empty"))
        }
        Ok(reference) => reference.map(str::to_string),
        Err(result) => return Ok(result),
    };
    let layer = match opt_string(args, "layer") {
        Ok(layer) => layer.map(str::to_string),
        Err(result) => return Ok(result),
    };
    let visible_only = match opt_bool(args, "visible_only") {
        Ok(value) => value.unwrap_or(false),
        Err(result) => return Ok(result),
    };

    let live_reference = reference.clone();
    let listing = match on_live_board(ctx, &board, move |client, document| {
        client.list_footprint_fields_in(document, live_reference.as_deref())
    })
    .await?
    {
        Ok(Ok(listing)) => listing,
        Ok(Err(error)) => return Ok(live_error_result(error, "list_footprint_fields")),
        Err(result) => return Ok(result),
    };

    let fields: Vec<Value> = listing
        .fields
        .iter()
        .filter(|field| !visible_only || field.visible)
        .filter(|field| {
            layer.as_deref().is_none_or(|layer| {
                field
                    .state
                    .as_ref()
                    .is_some_and(|state| state.layer == layer)
            })
        })
        .map(field_json)
        .collect();
    Ok(CallToolResult::json(&json!({
        "source": "ipc",
        "reference": reference,
        "count": fields.len(),
        "coordinates": COORDINATES,
        "bounding_boxes": bounding_box_evidence(&listing.bounding_boxes_unavailable),
        "fields": fields,
    })))
}

async fn handle_edit_footprint_field(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match non_empty_str(args, "reference") {
        Ok(reference) => reference.to_string(),
        Err(result) => return Ok(result),
    };
    let slot = match parse_field_slot(args) {
        Ok(slot) => slot,
        Err(result) => return Ok(result),
    };
    let edit = match parse_text_edit(args, true, false) {
        Ok(edit) => edit,
        Err(result) => return Ok(result),
    };

    let mirror_auto = edit.mirror_to_match_layer;
    let live_reference = reference.clone();
    let outcome = match on_live_board(ctx, &board, move |client, document| {
        client.edit_footprint_field_in(document, &live_reference, slot, &edit)
    })
    .await?
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => return Ok(live_error_result(error, "edit_footprint_field")),
        Err(result) => return Ok(result),
    };
    let mut extra = Map::new();
    extra.insert("reference".into(), json!(reference));
    extra.insert("field".into(), json!(slot.as_str()));
    if mirror_auto {
        if let Some(report) = mirror_auto_report(&outcome, |field| field.state.as_ref()) {
            extra.insert("mirror".into(), report);
        }
    }
    Ok(edit_outcome_result(
        outcome,
        "edit_footprint_field",
        &board,
        field_json,
        extra,
    ))
}

async fn handle_list_board_texts(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let layer = match opt_string(args, "layer") {
        Ok(layer) => layer.map(str::to_string),
        Err(result) => return Ok(result),
    };

    let listing = match on_live_board(ctx, &board, move |client, document| {
        client.list_board_texts_in(document)
    })
    .await?
    {
        Ok(Ok(listing)) => listing,
        Ok(Err(error)) => return Ok(live_error_result(error, "list_board_texts")),
        Err(result) => return Ok(result),
    };

    let texts: Vec<Value> = listing
        .texts
        .iter()
        .filter(|text| {
            layer.as_deref().is_none_or(|layer| {
                text.state
                    .as_ref()
                    .is_some_and(|state| state.layer == layer)
            })
        })
        .map(board_text_json)
        .collect();
    Ok(CallToolResult::json(&json!({
        "source": "ipc",
        "count": texts.len(),
        "coordinates": COORDINATES,
        "bounding_boxes": bounding_box_evidence(&listing.bounding_boxes_unavailable),
        "texts": texts,
    })))
}

async fn handle_edit_board_text(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let uuid = match non_empty_str(args, "uuid") {
        Ok(uuid) => uuid.to_string(),
        Err(result) => return Ok(result),
    };
    let edit = match parse_text_edit(args, false, true) {
        Ok(edit) => edit,
        Err(result) => return Ok(result),
    };

    let mirror_auto = edit.mirror_to_match_layer;
    let live_uuid = uuid.clone();
    let saved_board = board.clone();
    let outcome = match on_live_board(ctx, &board, move |client, document| {
        let caller_text = edit.text.is_some();
        let mut text_check = None;
        let outcome = client.edit_board_text_in(document, &live_uuid, &edit, |before| {
            if caller_text {
                text_check = Some("caller_supplied_text".to_string());
                return Ok(());
            }
            let live_text = before
                .state
                .as_ref()
                .map(|state| state.text.as_str())
                .unwrap_or_default();
            match text_variable_check(&saved_board, &live_uuid, live_text) {
                Ok(check) => {
                    text_check = Some(check);
                    Ok(())
                }
                Err(reason) => Err(TextVariableRefusal {
                    uuid: live_uuid.clone(),
                    reason,
                }
                .into()),
            }
        });
        outcome.map(|outcome| (outcome, text_check))
    })
    .await?
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => return Ok(live_error_result(error, "edit_board_text")),
        Err(result) => return Ok(result),
    };
    let (outcome, text_check) = outcome;
    let mut extra = Map::new();
    extra.insert("uuid".into(), json!(uuid));
    extra.insert("text_check".into(), json!(text_check));
    if mirror_auto {
        if let Some(report) = mirror_auto_report(&outcome, |text| text.state.as_ref()) {
            extra.insert("mirror".into(), report);
        }
    }
    Ok(edit_outcome_result(
        outcome,
        "edit_board_text",
        &board,
        board_text_json,
        extra,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;

    fn invalid_field(result: &CallToolResult) -> String {
        assert!(result.is_error, "expected a refusal");
        assert_eq!(
            extract_error_kind(result).as_deref(),
            Some("invalid_argument")
        );
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        let body: Value = serde_json::from_str(text).unwrap();
        body["error"]["field"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_position_needs_both_coordinates() {
        let refused = parse_text_edit(&json!({ "x": 1.0 }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "y");
        let refused = parse_text_edit(&json!({ "y": 1.0 }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "x");
    }

    #[test]
    fn an_edit_must_name_a_property() {
        let refused =
            parse_text_edit(&json!({ "board": "b", "reference": "R1" }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "edit");
    }

    #[test]
    fn sizes_and_thickness_must_be_positive() {
        for (key, value) in [
            ("size_x", -1.0),
            ("size_y", 0.0),
            ("thickness", -0.1),
            ("size_x", 251.0),
        ] {
            let refused = parse_text_edit(&one(key, json!(value)), true, false).unwrap_err();
            assert_eq!(invalid_field(&refused), key, "{key}={value}");
        }
        let refused = parse_text_edit(&json!({ "size_x": 1e-9 }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "size_x");
    }

    #[test]
    fn malformed_present_values_are_refused_not_ignored() {
        let refused = parse_text_edit(&json!({ "rotation": "90" }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "rotation");
        let refused = parse_text_edit(&json!({ "visible": "no" }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "visible");
        let refused = parse_text_edit(&json!({ "layer": "Nope.Layer" }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "layer");
        let refused = parse_text_edit(&json!({ "x": 1e9, "y": 0.0 }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "x");
    }

    #[test]
    fn each_tool_accepts_only_its_own_properties() {
        let refused = parse_text_edit(&json!({ "visible": false }), false, true).unwrap_err();
        assert_eq!(invalid_field(&refused), "visible");
        let refused = parse_text_edit(&json!({ "text": "R1" }), true, false).unwrap_err();
        assert_eq!(invalid_field(&refused), "text");
        let refused = parse_text_edit(&json!({ "text": "" }), false, true).unwrap_err();
        assert_eq!(invalid_field(&refused), "text");
    }

    #[test]
    fn millimetres_round_to_the_nearest_nanometre() {
        let edit = parse_text_edit(
            &json!({ "x": 1.2, "y": -0.3, "size_x": 1.2, "size_y": 1.3, "thickness": 0.15, "rotation": -90 }),
            true,
            false,
        )
        .unwrap();
        assert_eq!(edit.position_nm, Some((1_200_000, -300_000)));
        assert_eq!(edit.size_x_nm, Some(1_200_000));
        assert_eq!(edit.size_y_nm, Some(1_300_000));
        assert_eq!(edit.stroke_width_nm, Some(150_000));
        assert_eq!(edit.angle_degrees, Some(-90.0));
    }

    #[test]
    fn field_selection_defaults_to_the_reference() {
        assert_eq!(
            parse_field_slot(&json!({})).unwrap(),
            IpcFootprintFieldSlot::Reference
        );
        assert_eq!(
            parse_field_slot(&json!({ "field": "value" })).unwrap(),
            IpcFootprintFieldSlot::Value
        );
        assert_eq!(
            invalid_field(&parse_field_slot(&json!({ "field": "datasheet" })).unwrap_err()),
            "field"
        );
    }

    fn board_text_item(x_nm: i64, font_name: &str) -> IpcBoardTextItem {
        IpcBoardTextItem {
            kiid: "rev-text".to_string(),
            state: Some(IpcTextState {
                text: "Rev ${REVISION}".to_string(),
                x_nm,
                y_nm: 30_000_000,
                angle_degrees: 0.0,
                size_x_nm: 1_200_000,
                size_y_nm: 1_300_000,
                stroke_width_nm: 150_000,
                layer: "F.SilkS".to_string(),
                font_name: font_name.to_string(),
                horizontal_alignment: "left".to_string(),
                vertical_alignment: "bottom".to_string(),
                italic: false,
                bold: false,
                underlined: false,
                mirrored: false,
                multiline: false,
                keep_upright: true,
                line_spacing: 1.0,
                knockout: false,
                locked: false,
            }),
            unreadable_reason: None,
            bounding_box: None,
        }
    }

    #[test]
    fn outcomes_render_readback_evidence_and_uncertainty_is_an_error() {
        let board = Path::new("board.kicad_pcb");
        let applied = edit_outcome_result(
            IpcTextEditOutcome::Applied {
                before: board_text_item(20_000_000, "Osifont"),
                after: board_text_item(25_000_000, "Osifont"),
            },
            "edit_board_text",
            board,
            board_text_json,
            Map::new(),
        );
        assert!(!applied.is_error);
        let crate::mcp::protocol::ToolContent::Text { text } = &applied.content[0] else {
            panic!("expected text");
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["status"], "applied");
        assert_eq!(body["changed"], json!(["x_mm"]));
        assert_eq!(body["after"]["x_mm"], 25.0);
        assert_eq!(body["after"]["size_y_mm"], 1.3);
        assert_eq!(body["after"]["font_name"], "Osifont");

        let uncertain = edit_outcome_result(
            IpcTextEditOutcome::Uncertain {
                before: board_text_item(20_000_000, "Osifont"),
                expected: board_text_item(25_000_000, "Osifont"),
                observed: Some(board_text_item(25_000_000, "")),
                mismatches: vec!["text.font_name: expected \"Osifont\", observed \"\"".into()],
                reason: "KiCad accepted the update".into(),
            },
            "edit_board_text",
            board,
            board_text_json,
            Map::new(),
        );
        assert!(uncertain.is_error);
        assert_eq!(
            extract_error_kind(&uncertain).as_deref(),
            Some("mutation_outcome_uncertain")
        );
    }

    #[test]
    fn variable_tokens_find_variables_and_expressions() {
        assert_eq!(
            variable_tokens("Rev ${REVISION} @{1+1} $ {x} ${OPEN"),
            BTreeSet::from(["${REVISION}", "@{1+1}"])
        );
    }

    const BOARD: &str = r#"(kicad_pcb
	(version 20250114)
	(gr_text_box "boxed ${REVISION}"
		(uuid "box-1")
	)
	(gr_text "Rev ${REVISION}\nline two"
		(at 100 50 0)
		(layer "F.SilkS")
		(uuid "text-1")
	)
	(gr_text "PLAIN"
		(at 10 10 0)
		(layer "F.SilkS")
		(uuid "text-2")
	)
	(footprint "R"
		(fp_text user "${REFERENCE}" (at 0 0) (layer "F.Fab") (uuid "text-3"))
	)
)
"#;

    fn saved_board() -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .suffix(".kicad_pcb")
            .tempfile()
            .unwrap();
        std::fs::write(file.path(), BOARD).unwrap();
        file
    }

    #[test]
    fn the_saved_raw_text_is_read_by_uuid_from_top_level_text_only() {
        let file = saved_board();
        assert_eq!(
            saved_board_text(file.path(), "text-1").unwrap().as_deref(),
            Some("Rev ${REVISION}\nline two")
        );
        assert_eq!(saved_board_text(file.path(), "box-1").unwrap(), None);
        assert_eq!(saved_board_text(file.path(), "text-3").unwrap(), None);
        assert_eq!(
            saved_board_text(&file.path().with_extension("absent"), "text-1").unwrap(),
            None
        );
    }

    #[test]
    fn a_live_text_missing_a_saved_variable_is_refused() {
        let file = saved_board();
        // What an expanded string would look like.
        let refusal = text_variable_check(file.path(), "text-1", "Rev C\nline two").unwrap_err();
        assert!(refusal.contains("${REVISION}"), "{refusal}");

        assert_eq!(
            text_variable_check(file.path(), "text-1", "Rev ${REVISION}\nline two").unwrap(),
            "matches_saved_file"
        );
        assert_eq!(
            text_variable_check(file.path(), "text-1", "Board ${REVISION}").unwrap(),
            "differs_from_saved_file_without_losing_variables"
        );
        assert_eq!(
            text_variable_check(file.path(), "text-2", "PLAIN v2").unwrap(),
            "differs_from_saved_file_without_losing_variables"
        );
        assert_eq!(
            text_variable_check(file.path(), "new-text", "anything").unwrap(),
            "not_in_saved_file"
        );
    }

    #[test]
    fn every_tool_is_live_only_and_requires_what_its_handler_reads() {
        let tools = tools();
        for (name, required) in [
            ("list_footprint_fields", vec!["board"]),
            ("edit_footprint_field", vec!["board", "reference"]),
            ("list_board_texts", vec!["board"]),
            ("edit_board_text", vec!["board", "uuid"]),
        ] {
            let def = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            assert_eq!(def.board_access, BoardAccess::LiveOnly, "{name}");
            let got: Vec<&str> = def.input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(got, required, "{name}");
        }
    }

    fn test_context() -> std::sync::Arc<ToolContext> {
        std::sync::Arc::new(ToolContext::new(
            crate::tools::ServerConfig {
                // A dead endpoint inside the test's own temp dir: nothing
                // here can reach a real KiCad even if validation regressed.
                ipc_address: format!(
                    "ipc://{}",
                    std::env::temp_dir()
                        .join(format!("copperpour-pcb-text-{}.sock", std::process::id()))
                        .display()
                ),
                ..crate::tools::ServerConfig::default()
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        ))
    }

    fn one(key: &str, value: Value) -> Value {
        let mut args = Map::new();
        args.insert(key.to_string(), value);
        Value::Object(args)
    }

    /// The schema gate and the handler refuse the same mistakes through the
    /// served `tools/call` path, each naming the field. `size_x` and `field`
    /// are caught by the compiled schema (bounds, enum), `visible` on board
    /// text by the closed record, and the position pair and empty edit by the
    /// handler — all before IPC, against a dead endpoint.
    #[tokio::test]
    async fn refusals_survive_the_served_dispatch_and_name_the_field() {
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: format!(
                "ipc://{}",
                std::env::temp_dir()
                    .join(format!(
                        "copperpour-pcb-text-served-{}.sock",
                        std::process::id()
                    ))
                    .display()
            ),
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..crate::tools::ServerConfig::default()
        })
        .await
        .expect("handler builds");
        let board = "board.kicad_pcb";
        for (tool, arguments, field) in [
            (
                "edit_footprint_field",
                json!({ "board": board, "reference": "R1", "size_x": -1.0 }),
                "size_x",
            ),
            (
                "edit_footprint_field",
                json!({ "board": board, "reference": "R1", "field": "datasheet", "visible": false }),
                "field",
            ),
            (
                "edit_footprint_field",
                json!({ "board": board, "reference": "R1", "x": 3.0 }),
                "y",
            ),
            (
                "edit_board_text",
                json!({ "board": board, "uuid": "u", "visible": false }),
                "visible",
            ),
            (
                "edit_board_text",
                json!({ "board": board, "uuid": "u" }),
                "edit",
            ),
        ] {
            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": { "name": tool, "arguments": arguments }
                }))
                .await
                .expect("request returns a response");
            let result = response.result.expect("successful JSON-RPC response");
            assert_eq!(result["isError"], true, "{tool} {arguments}");
            let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
                .expect("tool body is JSON");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{tool}: {body}");
            assert_eq!(body["error"]["field"], field, "{tool}: {body}");
        }
    }

    // ─── Served dispatch against a KiCad double ──────────────────────────────

    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    use std::sync::{Arc, Mutex};

    /// What the KiCad double holds, and how it treats an update.
    #[derive(Default)]
    struct Held {
        texts: Vec<kiapi::board::types::BoardText>,
        footprints: Vec<kiapi::board::types::FootprintInstance>,
        /// Accept an update but keep each text's mirror flag as it was: the
        /// KiCad behaviour #202 could not rule out.
        keep_mirror_flag: bool,
        updates: usize,
    }

    fn held_text(
        kiid: &str,
        text: &str,
        layer: &str,
        mirrored: bool,
    ) -> kiapi::board::types::BoardText {
        let mut item = copperpour_ipc::builders::board_text_with_stroke_width(
            layer, text, 20.0, 30.0, 1.0, 0.15, 0.0, mirrored,
        );
        item.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        item
    }

    fn held_footprint(kiid: &str, reference: &str) -> kiapi::board::types::FootprintInstance {
        let field = |id, name: &str, suffix: &str, text: &str, layer: &str| {
            Some(kiapi::board::types::Field {
                id: Some(kiapi::board::types::FieldId { id }),
                name: name.to_string(),
                text: Some(held_text(&format!("{kiid}-{suffix}"), text, layer, false)),
                visible: true,
            })
        };
        kiapi::board::types::FootprintInstance {
            id: Some(kiapi::common::types::Kiid {
                value: kiid.to_string(),
            }),
            position: Some(copperpour_ipc::builders::vec2(20.0, 30.0)),
            orientation: Some(kiapi::common::types::Angle { value_degrees: 0.0 }),
            layer: kiapi::board::types::BoardLayer::BlFCu as i32,
            reference_field: field(1, "Reference", "ref", reference, "F.SilkS"),
            value_field: field(2, "Value", "val", "10k", "F.Fab"),
            definition: Some(Default::default()),
            ..Default::default()
        }
    }

    fn mirror_flag(text: &mut kiapi::board::types::BoardText) -> &mut bool {
        &mut text
            .text
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .mirrored
    }

    fn spawn_text_kicad(
        board: &Path,
        held: Arc<Mutex<Held>>,
    ) -> crate::test_support::MockIpcServer {
        use kiapi::common::commands;
        crate::tools::pcb_board::board_mock::spawn_kicad_holding_board(board, move |command| {
            let mut held = held.lock().unwrap();
            let name = command.type_url.rsplit('.').next().unwrap();
            match name {
                "GetItems" => {
                    let request = commands::GetItems::decode(command.value.as_slice()).unwrap();
                    let mut items = Vec::new();
                    for kind in request.types {
                        if kind == kiapi::common::types::KiCadObjectType::KotPcbText as i32 {
                            items.extend(held.texts.iter().map(|text| {
                                copperpour_ipc::builders::pack_any(
                                    text,
                                    "kiapi.board.types.BoardText",
                                )
                            }));
                        } else if kind
                            == kiapi::common::types::KiCadObjectType::KotPcbFootprint as i32
                        {
                            items.extend(held.footprints.iter().map(|footprint| {
                                copperpour_ipc::builders::pack_any(
                                    footprint,
                                    "kiapi.board.types.FootprintInstance",
                                )
                            }));
                        }
                    }
                    Some(copperpour_ipc::builders::pack_any(
                        &commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items,
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ))
                }
                "BeginCommit" => Some(copperpour_ipc::builders::pack_any(
                    &commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "text-commit".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                )),
                "EndCommit" => Some(copperpour_ipc::builders::pack_any(
                    &commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                )),
                "UpdateItems" => {
                    let request = commands::UpdateItems::decode(command.value.as_slice()).unwrap();
                    held.updates += request.items.len();
                    let keep_mirror_flag = held.keep_mirror_flag;
                    for item in &request.items {
                        if item.type_url.ends_with("BoardText") {
                            let mut updated =
                                kiapi::board::types::BoardText::decode(item.value.as_slice())
                                    .unwrap();
                            let slot = held
                                .texts
                                .iter_mut()
                                .find(|text| text.id == updated.id)
                                .expect("update names a held text");
                            if keep_mirror_flag {
                                *mirror_flag(&mut updated) = *mirror_flag(slot);
                            }
                            *slot = updated;
                        } else {
                            let mut updated = kiapi::board::types::FootprintInstance::decode(
                                item.value.as_slice(),
                            )
                            .unwrap();
                            let slot = held
                                .footprints
                                .iter_mut()
                                .find(|footprint| footprint.id == updated.id)
                                .expect("update names a held footprint");
                            if keep_mirror_flag {
                                let old = slot.reference_field.as_mut().unwrap();
                                let new = updated.reference_field.as_mut().unwrap();
                                *mirror_flag(new.text.as_mut().unwrap()) =
                                    *mirror_flag(old.text.as_mut().unwrap());
                            }
                            *slot = updated;
                        }
                    }
                    Some(copperpour_ipc::builders::pack_any(
                        &commands::UpdateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items: request
                                .items
                                .iter()
                                .map(|_| commands::ItemUpdateResult {
                                    status: Some(commands::ItemStatus {
                                        code: commands::ItemStatusCode::IscOk as i32,
                                        error_message: String::new(),
                                    }),
                                    item: None,
                                })
                                .collect(),
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    ))
                }
                _ => None,
            }
        })
    }

    /// A served handler talking to a KiCad double that holds one board with
    /// one unmirrored text already on `B.SilkS` (the state #202 was filed
    /// from) and one resistor.
    struct LiveScene {
        _dir: tempfile::TempDir,
        board: std::path::PathBuf,
        held: Arc<Mutex<Held>>,
        _kicad: crate::test_support::MockIpcServer,
        handler: crate::mcp::handler::McpHandler,
    }

    impl LiveScene {
        async fn new(keep_mirror_flag: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let board = dir.path().join("silk.kicad_pcb");
            std::fs::write(&board, "(kicad_pcb\n\t(version 20250114)\n)\n").unwrap();
            let held = Arc::new(Mutex::new(Held {
                texts: vec![held_text("title", "Konnect rev A", "B.SilkS", false)],
                footprints: vec![held_footprint("fp-r1", "R1")],
                keep_mirror_flag,
                updates: 0,
            }));
            let kicad = spawn_text_kicad(&board, held.clone());
            let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: kicad.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: true,
                eager_toolsets: false,
            })
            .await
            .expect("handler builds");
            Self {
                _dir: dir,
                board,
                held,
                _kicad: kicad,
                handler,
            }
        }

        /// `tools/call`, returning whether the result is an error and its
        /// JSON body.
        async fn call(&self, tool: &str, mut arguments: Value) -> (bool, Value) {
            arguments["board"] = json!(self.board.to_string_lossy());
            let response = self
                .handler
                .handle_message(json!({
                    "jsonrpc": "2.0", "id": 202, "method": "tools/call",
                    "params": { "name": tool, "arguments": arguments }
                }))
                .await
                .expect("tools/call receives a response");
            let result = response.result.expect("tool errors are MCP results");
            let text = result["content"][0]["text"].as_str().expect("text content");
            let body = serde_json::from_str(text)
                .unwrap_or_else(|_| panic!("expected a JSON body, got {text}"));
            (result["isError"] == true, body)
        }
    }

    /// The issue's case end to end: the title already sits on `B.SilkS`
    /// unmirrored, and one call sets the flag and nothing else.
    #[tokio::test]
    async fn the_served_edit_board_text_mirrors_a_back_side_text() {
        let scene = LiveScene::new(false).await;
        let (is_error, body) = scene
            .call(
                "edit_board_text",
                json!({ "uuid": "title", "mirrored": true }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["status"], "applied");
        assert_eq!(body["changed"], json!(["mirrored"]));
        assert_eq!(body["before"]["mirrored"], false);
        assert_eq!(body["after"]["mirrored"], true);
        assert_eq!(body["after"]["layer"], "B.SilkS");
        assert_eq!(body["after"]["text"], "Konnect rev A");

        let (is_error, listing) = scene.call("list_board_texts", json!({})).await;
        assert!(!is_error, "{listing}");
        assert_eq!(listing["texts"][0]["mirrored"], true);

        // Asking again changes nothing and sends nothing.
        let (is_error, body) = scene
            .call(
                "edit_board_text",
                json!({ "uuid": "title", "mirrored": true }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["status"], "unchanged");
        assert_eq!(scene.held.lock().unwrap().updates, 1);
    }

    #[tokio::test]
    async fn the_served_edits_set_every_style_property_and_report_it_from_readback() {
        let scene = LiveScene::new(false).await;
        let style = json!({
            "mirrored": true, "horizontal_alignment": "left", "vertical_alignment": "top",
            "bold": true, "italic": true, "keep_upright": true, "knockout": true,
            "locked": true, "font": "Arial"
        });
        let expected_changed = BTreeSet::from([
            "bold",
            "font_name",
            "horizontal_alignment",
            "italic",
            "keep_upright",
            "knockout",
            "locked",
            "mirrored",
            "vertical_alignment",
        ]);
        for (tool, target) in [
            ("edit_board_text", json!({ "uuid": "title" })),
            ("edit_footprint_field", json!({ "reference": "R1" })),
        ] {
            let mut arguments = style.clone();
            for (key, value) in target.as_object().unwrap() {
                arguments[key] = value.clone();
            }
            let (is_error, body) = scene.call(tool, arguments).await;
            assert!(!is_error, "{tool}: {body}");
            assert_eq!(body["status"], "applied", "{tool}");
            let changed: BTreeSet<&str> = body["changed"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(changed, expected_changed, "{tool}");
            let after = &body["after"];
            assert_eq!(after["font_name"], "Arial", "{tool}");
            assert_eq!(after["horizontal_alignment"], "left", "{tool}");
            assert_eq!(after["vertical_alignment"], "top", "{tool}");
            // What the edit did not name is what KiCad held.
            assert_eq!(after["x_mm"], 20.0, "{tool}");
            assert_eq!(after["size_x_mm"], 1.0, "{tool}");
            assert_eq!(after["thickness_mm"], 0.15, "{tool}");
        }
        // The value field of the edited footprint was sent back untouched.
        let held = scene.held.lock().unwrap();
        assert_eq!(
            held.footprints[0].value_field,
            held_footprint("fp-r1", "R1").value_field
        );
    }

    /// A KiCad that accepts the update and keeps the old flag must produce an
    /// error naming the property, not `applied`.
    #[tokio::test]
    async fn a_kicad_that_ignores_mirrored_is_an_error_through_the_served_dispatch() {
        let scene = LiveScene::new(true).await;
        for (tool, arguments, mismatch) in [
            (
                "edit_board_text",
                json!({ "uuid": "title", "mirrored": true }),
                "text.mirrored: expected true, observed false",
            ),
            (
                "edit_footprint_field",
                json!({ "reference": "R1", "mirrored": true, "layer": "B.SilkS" }),
                "reference.mirrored: expected true, observed false",
            ),
        ] {
            let (is_error, body) = scene.call(tool, arguments).await;
            assert!(is_error, "{tool}: {body}");
            assert_eq!(
                body["error"]["kind"], "mutation_outcome_uncertain",
                "{tool}: {body}"
            );
            assert!(
                body["error"]["reason"].as_str().unwrap().contains(mismatch),
                "{tool}: {body}"
            );
        }
    }

    /// Item 3 of #202 through `tools/call`: a layer change with
    /// `mirror: "auto"` mirrors on the way to the back and unmirrors on the
    /// way to the front, for board text and for a footprint field.
    #[tokio::test]
    async fn the_served_mirror_auto_follows_the_side_of_the_target_layer() {
        let scene = LiveScene::new(false).await;

        // The title is on B.SilkS unmirrored. No layer named: the layer it
        // is on decides, which is the repair the issue was filed for.
        let (is_error, body) = scene
            .call(
                "edit_board_text",
                json!({ "uuid": "title", "mirror": "auto" }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["changed"], json!(["mirrored"]));
        assert_eq!(body["after"]["mirrored"], true);
        assert_eq!(
            body["mirror"],
            json!({ "mode": "auto", "layer": "B.SilkS", "side": "back" })
        );

        let (is_error, body) = scene
            .call(
                "edit_board_text",
                json!({ "uuid": "title", "layer": "F.SilkS", "mirror": "auto" }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["after"]["layer"], "F.SilkS");
        assert_eq!(body["after"]["mirrored"], false);
        assert_eq!(body["mirror"]["side"], "front");
        // Only the layer and the flag moved.
        assert_eq!(body["after"]["x_mm"], body["before"]["x_mm"]);
        assert_eq!(
            body["after"]["angle_degrees"],
            body["before"]["angle_degrees"]
        );

        let (is_error, body) = scene
            .call(
                "edit_footprint_field",
                json!({ "reference": "R1", "layer": "B.SilkS", "mirror": "auto" }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["after"]["layer"], "B.SilkS");
        assert_eq!(body["after"]["mirrored"], true);
        assert_eq!(body["mirror"]["side"], "back");

        // A layer with no side keeps the flag and says so.
        let (is_error, body) = scene
            .call(
                "edit_footprint_field",
                json!({ "reference": "R1", "layer": "Dwgs.User", "mirror": "auto" }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["changed"], json!(["layer"]));
        assert_eq!(body["after"]["mirrored"], true);
        assert_eq!(body["mirror"]["side"], "none");
        assert!(body["mirror"]["note"].is_string(), "{body}");

        // Without the option a layer change still leaves the flag alone, and
        // the response carries no mirror report.
        let (is_error, body) = scene
            .call(
                "edit_board_text",
                json!({ "uuid": "title", "layer": "B.Fab" }),
            )
            .await;
        assert!(!is_error, "{body}");
        assert_eq!(body["after"]["mirrored"], false);
        assert!(body.get("mirror").is_none(), "{body}");
    }

    #[tokio::test]
    async fn mirror_auto_is_refused_with_an_explicit_flag_or_an_unknown_mode() {
        let scene = LiveScene::new(false).await;
        for arguments in [
            json!({ "uuid": "title", "mirror": "auto", "mirrored": true }),
            json!({ "uuid": "title", "mirror": "flip" }),
        ] {
            let (is_error, body) = scene.call("edit_board_text", arguments.clone()).await;
            assert!(is_error, "{arguments}: {body}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
            assert_eq!(body["error"]["field"], "mirror", "{body}");
        }
        assert_eq!(scene.held.lock().unwrap().updates, 0);

        // 'keep' is the default spelled out: it names no property by itself.
        let refused = parse_text_edit(&json!({ "mirror": "keep" }), false, true).unwrap_err();
        assert_eq!(invalid_field(&refused), "edit");
        let edit =
            parse_text_edit(&json!({ "mirror": "keep", "mirrored": true }), true, false).unwrap();
        assert!(!edit.mirror_to_match_layer);
        assert_eq!(edit.mirrored, Some(true));
    }

    #[tokio::test]
    async fn malformed_style_values_are_refused_naming_the_field() {
        for (args, field) in [
            (
                json!({ "horizontal_alignment": "middle" }),
                "horizontal_alignment",
            ),
            (
                json!({ "vertical_alignment": "indeterminate" }),
                "vertical_alignment",
            ),
            (json!({ "mirrored": "yes" }), "mirrored"),
            (json!({ "locked": 1 }), "locked"),
            (json!({ "font": "Arial\n" }), "font"),
            (json!({ "font": " Arial" }), "font"),
        ] {
            for (allow_visible, allow_text) in [(true, false), (false, true)] {
                let refused = parse_text_edit(&args, allow_visible, allow_text).unwrap_err();
                assert_eq!(invalid_field(&refused), field, "{args}");
            }
        }
        // The empty string is a value: it selects the stroke font.
        let edit = parse_text_edit(&json!({ "font": "" }), false, true).unwrap();
        assert_eq!(edit.font_name.as_deref(), Some(""));
    }

    /// The refusals happen before any transport is dialled: the address in
    /// the context is dead, so a different error proves no IPC was tried.
    #[tokio::test]
    async fn invalid_arguments_are_refused_before_dialling_kicad() {
        let ctx = test_context();
        for (handler, args, field) in [
            (
                "edit_footprint_field",
                json!({ "board": "b.kicad_pcb", "reference": "R1", "x": 1.0 }),
                "y",
            ),
            (
                "edit_footprint_field",
                json!({ "board": "b.kicad_pcb", "reference": "R1" }),
                "edit",
            ),
            (
                "edit_footprint_field",
                json!({ "board": "b.kicad_pcb", "reference": "R1", "size_y": -1.0 }),
                "size_y",
            ),
            (
                "edit_board_text",
                json!({ "board": "b.kicad_pcb", "uuid": "u", "thickness": 0.0 }),
                "thickness",
            ),
            (
                "edit_board_text",
                json!({ "board": "b.kicad_pcb", "uuid": "u" }),
                "edit",
            ),
        ] {
            let result = match handler {
                "edit_footprint_field" => handle_edit_footprint_field(&args, &ctx).await,
                _ => handle_edit_board_text(&args, &ctx).await,
            }
            .unwrap();
            assert_eq!(invalid_field(&result), field, "{handler} {args}");
        }
    }
}
