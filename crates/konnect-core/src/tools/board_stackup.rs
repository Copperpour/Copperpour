//! The board's physical stackup, read from the board KiCad holds open (#716).
//!
//! Read-only by design. KiCad 10 declares `UpdateBoardStackup` but does not
//! implement it, and #716 settled that Konnect will not write a stackup into a
//! board file meanwhile: the answer names KiCad's Board Setup as the place to
//! change one. There is no saved-file fallback either — reading the file's
//! `(stackup …)` block would be a second parser of KiCad's format — so a board
//! KiCad does not hold open is refused.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::board_source::{self, FROM_DERIVED, FROM_IPC};
use crate::tools::{
    get_path, invalid_arg, ipc_target_error_result, with_bound_board_ipc_classified, BoardAccess,
    BoardBinding, ToolContext, ToolDef,
};
use konnect_ipc::{IpcBoardStackup, IpcStackupLayer};
use serde_json::{json, Value};

/// KiCad's marker for a parameter the board leaves unset
/// (`stackup_predefined_prms.h`, `NotSpecifiedPrm`).
const NOT_SPECIFIED: &str = "Not specified";

/// Thickness is compared to within a micrometre, the finest step a
/// fabricator's stackup table states.
const THICKNESS_TOLERANCE_MM: f64 = 0.001;

/// εr and loss tangent are compared to within this.
const PROPERTY_TOLERANCE: f64 = 1e-6;

const BOARD_SETUP_HINT: &str = "Copperpour cannot change a stackup: KiCad 10 declares \
     UpdateBoardStackup but does not implement it (#716). Change it in KiCad's Board Setup, \
     under Board Stackup, then read it again.";

/// KiCad's own default finish is "None" (`BOARD_STACKUP::BOARD_STACKUP`), and
/// "None" is also a finish its list offers, so the answer cannot tell a board
/// left at its default from one that chose no finish.
const DEFAULT_NOTE: &str = "KiCad serves its default stackup for a board that defines \
     none, and its answer does not say which this is. Its default finish, \"None\", is also \
     a finish a board can name, so it is not reported as missing: give expected.finish to \
     check it.";

pub(crate) fn tool() -> ToolDef {
    tool!(
        "get_board_stackup",
        "Read the physical stackup of the board KiCad holds open: each layer's type, \
         thickness and material, dielectric εr and loss tangent, the copper finish, impedance \
         control, edge settings, and the board thickness KiCad computes from the stack. \
         'findings' lists unspecified fabrication values and, given 'expected', every value \
         that differs from it. Live only, with no file fallback; read-only, so changes are \
         made in Board Setup.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "expected": {
                    "type": "object",
                    "description": "The stackup the fabricator will build, to compare against. Only the fields given are compared.",
                    "properties": {
                        "board_thickness_mm": { "type": "number", "exclusiveMinimum": 0 },
                        "copper_layer_count": { "type": "integer", "minimum": 1 },
                        "finish": { "type": "string", "description": "KiCad's finish name, e.g. 'ENIG' or 'HAL lead-free'; compared ignoring case" },
                        "impedance_controlled": { "type": "boolean" },
                        "layers": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "A layer name as this tool reports it: 'F.Cu', 'F.Mask', 'dielectric 1'" },
                                    "thickness_mm": { "type": "number", "exclusiveMinimum": 0, "description": "A dielectric's is the slot's total over its sub-layers" },
                                    "material": { "type": "string", "description": "Compared ignoring case; for a dielectric, against every sub-layer" },
                                    "epsilon_r": { "type": "number", "exclusiveMinimum": 0 },
                                    "loss_tangent": { "type": "number", "minimum": 0 }
                                },
                                "required": ["name"]
                            }
                        }
                    }
                }
            },
            "required": ["board"]
        }),
        |args, ctx| async move { handle_get_board_stackup(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

async fn handle_get_board_stackup(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let expected = match Expected::from_args(args) {
        Ok(expected) => expected,
        Err(refusal) => return Ok(refusal),
    };

    // The read's own failure is carried back as data, so an answer Konnect
    // could not decode is reported as that, never as KiCad refusing (#658).
    let live = with_bound_board_ipc_classified(ctx, &board, move |client, document| {
        Ok(client.get_board_stackup_in(document))
    })
    .await?;

    let stackup = match live {
        Ok(BoardBinding::Bound(Ok(stackup))) => stackup,
        Ok(BoardBinding::Bound(Err(error))) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::HandlerError {
                    reason: format!("{error:#}"),
                },
                format!("The stackup read did not complete: {error:#}"),
            ))
        }
        Ok(BoardBinding::Unserved { message, .. }) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::EditorUnavailable {
                    editor: "pcb".to_string(),
                    reason: message.clone(),
                },
                format!(
                    "KiCad must be running with the requested board open: {message}. The \
                     stackup is read from the live board only; there is no file fallback."
                ),
            ))
        }
        Err(konnect_ipc::IpcFailure::Target { error, .. }) => {
            return Ok(ipc_target_error_result(&error))
        }
        Err(error) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::EditorUnavailable {
                    editor: "pcb".to_string(),
                    reason: error.message().to_string(),
                },
                format!(
                    "KiCad could not serve the stackup read for this board: {error}. The \
                     stackup is read from the live board only; there is no file fallback."
                ),
            ))
        }
    };

    Ok(CallToolResult::json(&stackup_body(
        &board.display().to_string(),
        &stackup,
        &expected,
    )))
}

// ─── The answer ───────────────────────────────────────────────────────────────

fn mm(nm: i64) -> f64 {
    nm as f64 / 1_000_000.0
}

/// Whether KiCad left a text parameter unset.
fn unspecified(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case(NOT_SPECIFIED)
}

/// Each entry's name: its canonical board layer, or "dielectric N" in stack order,
/// the name KiCad's own board file gives a dielectric.
fn layer_names(stackup: &IpcBoardStackup) -> Vec<String> {
    let mut dielectrics = 0;
    stackup
        .layers
        .iter()
        .map(|layer| match &layer.layer {
            Some(name) if layer.kind != "dielectric" => name.clone(),
            _ if layer.kind == "dielectric" => {
                dielectrics += 1;
                format!("dielectric {dielectrics}")
            }
            _ => layer.kind.clone(),
        })
        .collect()
}

/// A dielectric slot's thickness: all its sub-layers, as KiCad adds them.
fn slot_thickness_nm(layer: &IpcStackupLayer) -> i64 {
    if layer.dielectric.is_empty() {
        layer.thickness_nm
    } else {
        layer.dielectric.iter().map(|sub| sub.thickness_nm).sum()
    }
}

/// The board thickness KiCad derives from a stackup: every enabled copper,
/// dielectric and solder-mask entry, a dielectric with all its sub-layers
/// (`BOARD_STACKUP::BuildBoardThicknessFromStackup`).
fn board_thickness_nm(stackup: &IpcBoardStackup) -> i64 {
    stackup
        .layers
        .iter()
        .filter(|layer| layer.enabled)
        .map(|layer| match layer.kind.as_str() {
            "dielectric" => slot_thickness_nm(layer),
            "copper" | "soldermask" => layer.thickness_nm,
            _ => 0,
        })
        .sum()
}

fn copper_layer_count(stackup: &IpcBoardStackup) -> usize {
    stackup
        .layers
        .iter()
        .filter(|layer| layer.enabled && layer.kind == "copper")
        .count()
}

fn layer_json(id: &str, layer: &IpcStackupLayer) -> Value {
    let text = |value: &str| {
        if value.is_empty() {
            Value::Null
        } else {
            json!(value)
        }
    };
    let mut out = json!({
        "name": id,
        "type": layer.kind,
        "enabled": layer.enabled,
        "user_name": layer.user_name,
    });
    // Color is left out. KiCad's API sends a stackup color as wxWidgets' RGBA
    // for its name (`COLOR4D( const wxString& )`), and #00000000 for a name
    // wxWidgets does not know, such as "FR4 natural"; the name itself is never
    // sent, so the value cannot say which color the board names.
    match layer.kind.as_str() {
        "dielectric" => {
            out["dielectric_type"] = json!(layer.dielectric_type);
            out["thickness_mm"] = json!(mm(slot_thickness_nm(layer)));
            out["sublayers"] = json!(layer
                .dielectric
                .iter()
                .map(|sub| json!({
                    "thickness_mm": mm(sub.thickness_nm),
                    "material": text(&sub.material),
                    "epsilon_r": sub.epsilon_r,
                    "loss_tangent": sub.loss_tangent,
                    "thickness_locked": sub.thickness_locked,
                }))
                .collect::<Vec<_>>());
        }
        "copper" => {
            out["thickness_mm"] = json!(mm(layer.thickness_nm));
            out["material"] = text(&layer.material);
        }
        "soldermask" => {
            out["thickness_mm"] = json!(mm(layer.thickness_nm));
            out["material"] = text(&layer.material);
            out["epsilon_r"] = json!(layer.epsilon_r);
            out["loss_tangent"] = json!(layer.loss_tangent);
        }
        "silkscreen" => {
            out["material"] = text(&layer.material);
        }
        _ => {}
    }
    out
}

fn stackup_body(board: &str, stackup: &IpcBoardStackup, expected: &Expected) -> Value {
    let names = layer_names(stackup);
    let layers: Vec<Value> = names
        .iter()
        .zip(&stackup.layers)
        .map(|(id, layer)| layer_json(id, layer))
        .collect();
    let mut findings = missing_values(stackup, &names);
    findings.extend(expected.mismatches(stackup, &names));

    let mut body = json!({
        "board": board,
        "copper_layer_count": copper_layer_count(stackup),
        "board_thickness_mm": mm(board_thickness_nm(stackup)),
        "finish": stackup.finish,
        "impedance_controlled": stackup.impedance_controlled,
        "edge_connector": stackup.edge_connector,
        "has_edge_plating": stackup.has_edge_plating,
        "count": layers.len(),
        "layers": layers,
        "finding_count": findings.len(),
        "findings": findings,
        "note": DEFAULT_NOTE,
        "hint": BOARD_SETUP_HINT,
    });
    board_source::provenance(
        &mut body,
        json!({
            "stackup": FROM_IPC,
            "copper_layer_count": FROM_DERIVED,
            "board_thickness_mm": FROM_DERIVED,
            "findings": FROM_DERIVED,
        }),
        board_source::live_evidence(),
    );
    body
}

// ─── Findings ────────────────────────────────────────────────────────────────

fn finding(id: &str, field: &str, kind: &str, detail: String) -> Value {
    json!({ "name": id, "field": field, "kind": kind, "detail": detail })
}

/// Fabrication values the board leaves unset: what a fabricator or an
/// impedance calculation needs and KiCad reports as missing.
fn missing_values(stackup: &IpcBoardStackup, names: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    if unspecified(&stackup.finish) {
        out.push(finding(
            "board",
            "finish",
            "missing",
            "The board names no copper finish.".to_string(),
        ));
    }
    for (id, layer) in names.iter().zip(&stackup.layers) {
        if !layer.enabled {
            continue;
        }
        match layer.kind.as_str() {
            "copper" | "soldermask" if layer.thickness_nm <= 0 => out.push(finding(
                id,
                "thickness_mm",
                "missing",
                format!("{id} has no thickness."),
            )),
            "dielectric" => {
                if slot_thickness_nm(layer) <= 0 {
                    out.push(finding(
                        id,
                        "thickness_mm",
                        "missing",
                        format!("{id} has no thickness."),
                    ));
                }
                let plies = layer.dielectric.len();
                for (index, sub) in layer.dielectric.iter().enumerate() {
                    let at = |field: &str, what: &str| {
                        if plies > 1 {
                            let mut found = finding(
                                id,
                                field,
                                "missing",
                                format!("{id}, sub-layer {}, {what}.", index + 1),
                            );
                            found["sublayer"] = json!(index + 1);
                            found
                        } else {
                            finding(id, field, "missing", format!("{id} {what}."))
                        }
                    };
                    // The slot's own thickness is checked above; one ply of
                    // several can still have none.
                    if plies > 1 && sub.thickness_nm <= 0 {
                        out.push(at("thickness_mm", "has no thickness"));
                    }
                    if unspecified(&sub.material) {
                        out.push(at("material", "names no material"));
                    }
                    if !sub.epsilon_r.is_finite() || sub.epsilon_r <= 0.0 {
                        out.push(at("epsilon_r", "has no εr"));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One expected layer, as the caller gave it.
#[derive(Debug, Default)]
struct ExpectedLayer {
    name: String,
    thickness_mm: Option<f64>,
    material: Option<String>,
    epsilon_r: Option<f64>,
    loss_tangent: Option<f64>,
}

/// The stackup the caller expects, all optional.
#[derive(Debug, Default)]
struct Expected {
    board_thickness_mm: Option<f64>,
    copper_layer_count: Option<u64>,
    finish: Option<String>,
    impedance_controlled: Option<bool>,
    layers: Vec<ExpectedLayer>,
}

impl Expected {
    /// Read `expected`. The served schema refuses a malformed one before
    /// dispatch; this keeps the same refusals for direct handler calls.
    fn from_args(args: &Value) -> Result<Self, CallToolResult> {
        let Some(value) = args.get("expected").filter(|v| !v.is_null()) else {
            return Ok(Self::default());
        };
        let Some(object) = value.as_object() else {
            return Err(invalid_arg("expected", "must be an object"));
        };
        let number = |field: &str, value: Option<&Value>| -> Result<Option<f64>, CallToolResult> {
            match value {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .map(Some)
                    .ok_or_else(|| invalid_arg(field, "must be a finite number")),
            }
        };
        let string =
            |field: &str, value: Option<&Value>| -> Result<Option<String>, CallToolResult> {
                match value {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(s)) => Ok(Some(s.clone())),
                    Some(_) => Err(invalid_arg(field, "must be a string")),
                }
            };

        let mut expected = Expected {
            board_thickness_mm: number(
                "expected.board_thickness_mm",
                object.get("board_thickness_mm"),
            )?,
            copper_layer_count: match object.get("copper_layer_count") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().filter(|n| *n >= 1).ok_or_else(|| {
                    invalid_arg("expected.copper_layer_count", "must be a positive integer")
                })?),
            },
            finish: string("expected.finish", object.get("finish"))?,
            impedance_controlled: match object.get("impedance_controlled") {
                None | Some(Value::Null) => None,
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => {
                    return Err(invalid_arg(
                        "expected.impedance_controlled",
                        "must be a boolean",
                    ))
                }
            },
            layers: Vec::new(),
        };
        match object.get("layers") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for item in items {
                    let Some(layer) = item.as_object() else {
                        return Err(invalid_arg(
                            "expected.layers",
                            "each entry must be an object",
                        ));
                    };
                    let Some(name) = layer.get("name").and_then(Value::as_str) else {
                        return Err(invalid_arg(
                            "expected.layers",
                            "each entry needs a string 'name'",
                        ));
                    };
                    expected.layers.push(ExpectedLayer {
                        name: name.to_string(),
                        thickness_mm: number(
                            "expected.layers.thickness_mm",
                            layer.get("thickness_mm"),
                        )?,
                        material: string("expected.layers.material", layer.get("material"))?,
                        epsilon_r: number("expected.layers.epsilon_r", layer.get("epsilon_r"))?,
                        loss_tangent: number(
                            "expected.layers.loss_tangent",
                            layer.get("loss_tangent"),
                        )?,
                    });
                }
            }
            Some(_) => return Err(invalid_arg("expected.layers", "must be an array")),
        }
        Ok(expected)
    }

    /// Every expected value the stackup does not carry.
    fn mismatches(&self, stackup: &IpcBoardStackup, names: &[String]) -> Vec<Value> {
        let mut out = Vec::new();
        let differs = |id: &str, field: &str, expected: Value, actual: Value| {
            json!({
                "name": id, "field": field, "kind": "mismatch",
                "expected": expected, "actual": actual,
                "detail": format!("{id} {field} is {actual}, expected {expected}."),
            })
        };
        let same_text = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());

        if let Some(want) = self.board_thickness_mm {
            let have = mm(board_thickness_nm(stackup));
            if (have - want).abs() > THICKNESS_TOLERANCE_MM {
                out.push(differs(
                    "board",
                    "board_thickness_mm",
                    json!(want),
                    json!(have),
                ));
            }
        }
        if let Some(want) = self.copper_layer_count {
            let have = copper_layer_count(stackup) as u64;
            if have != want {
                out.push(differs(
                    "board",
                    "copper_layer_count",
                    json!(want),
                    json!(have),
                ));
            }
        }
        if let Some(want) = &self.finish {
            if !same_text(want, &stackup.finish) {
                out.push(differs(
                    "board",
                    "finish",
                    json!(want),
                    json!(stackup.finish),
                ));
            }
        }
        if let Some(want) = self.impedance_controlled {
            if want != stackup.impedance_controlled {
                out.push(differs(
                    "board",
                    "impedance_controlled",
                    json!(want),
                    json!(stackup.impedance_controlled),
                ));
            }
        }

        for want in &self.layers {
            let Some(index) = names.iter().position(|id| id == &want.name) else {
                out.push(finding(
                    &want.name,
                    "name",
                    "unknown_layer",
                    format!(
                        "The stackup has no layer '{}'. Its layers are: {}.",
                        want.name,
                        names.join(", ")
                    ),
                ));
                continue;
            };
            let layer = &stackup.layers[index];
            let id = want.name.as_str();

            if let Some(thickness) = want.thickness_mm {
                let have = match layer.kind.as_str() {
                    "dielectric" => Some(mm(slot_thickness_nm(layer))),
                    "copper" | "soldermask" => Some(mm(layer.thickness_nm)),
                    _ => None,
                };
                match have {
                    Some(have) if (have - thickness).abs() > THICKNESS_TOLERANCE_MM => {
                        out.push(differs(id, "thickness_mm", json!(thickness), json!(have)))
                    }
                    Some(_) => {}
                    None => out.push(not_reported(id, "thickness_mm", &layer.kind)),
                }
            }

            // A dielectric's material, εr and loss tangent are per sub-layer;
            // every sub-layer is held to the expected value.
            if layer.kind == "dielectric" {
                let plies = layer.dielectric.len();
                for (index, sub) in layer.dielectric.iter().enumerate() {
                    // A slot of several sub-layers gives one finding each, so
                    // each says which sub-layer it is.
                    let at = |field: &str, expected: Value, actual: Value| {
                        let mut found = differs(id, field, expected.clone(), actual.clone());
                        if plies > 1 {
                            found["sublayer"] = json!(index + 1);
                            found["detail"] = json!(format!(
                                "{id}, sub-layer {}, {field} is {actual}, expected {expected}.",
                                index + 1
                            ));
                        }
                        found
                    };
                    if let Some(material) = &want.material {
                        if !same_text(material, &sub.material) {
                            out.push(at("material", json!(material), json!(sub.material)));
                        }
                    }
                    if let Some(er) = want.epsilon_r {
                        if (sub.epsilon_r - er).abs() > PROPERTY_TOLERANCE {
                            out.push(at("epsilon_r", json!(er), json!(sub.epsilon_r)));
                        }
                    }
                    if let Some(tan) = want.loss_tangent {
                        if (sub.loss_tangent - tan).abs() > PROPERTY_TOLERANCE {
                            out.push(at("loss_tangent", json!(tan), json!(sub.loss_tangent)));
                        }
                    }
                }
                continue;
            }

            if let Some(material) = &want.material {
                // KiCad serializes no material for solder paste.
                if layer.material.is_empty() {
                    out.push(not_reported(id, "material", &layer.kind));
                } else if !same_text(material, &layer.material) {
                    out.push(differs(
                        id,
                        "material",
                        json!(material),
                        json!(layer.material),
                    ));
                }
            }
            for (field, want_value, have) in [
                ("epsilon_r", want.epsilon_r, layer.epsilon_r),
                ("loss_tangent", want.loss_tangent, layer.loss_tangent),
            ] {
                match (want_value, have) {
                    (Some(want_value), Some(have))
                        if (have - want_value).abs() > PROPERTY_TOLERANCE =>
                    {
                        out.push(differs(id, field, json!(want_value), json!(have)))
                    }
                    (Some(_), None) => out.push(not_reported(id, field, &layer.kind)),
                    _ => {}
                }
            }
        }
        out
    }
}

fn not_reported(id: &str, field: &str, kind: &str) -> Value {
    finding(
        id,
        field,
        "not_reported",
        format!("KiCad reports no {field} for a {kind} layer, so {id} cannot be compared."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::pcb_board::board_mock::{
        board_document, ctx_talking_to, spawn_kicad_holding_board, spawn_kicad_holding_boards,
    };
    use konnect_ipc::gen::kiapi;
    use konnect_ipc::IpcStackupDielectric;
    use std::sync::{Arc, Mutex};

    fn layer(name: &str, kind: &str, thickness_nm: i64) -> IpcStackupLayer {
        IpcStackupLayer {
            layer: Some(name.to_string()),
            user_name: Some(name.to_string()),
            kind: kind.to_string(),
            enabled: true,
            thickness_nm,
            material: String::new(),
            color: None,
            dielectric_type: None,
            dielectric: Vec::new(),
            epsilon_r: None,
            loss_tangent: None,
        }
    }

    fn dielectric(kind: &str, sublayers: &[(i64, &str, f64, f64)]) -> IpcStackupLayer {
        IpcStackupLayer {
            layer: None,
            user_name: None,
            kind: "dielectric".to_string(),
            enabled: true,
            thickness_nm: sublayers.first().map_or(0, |s| s.0),
            material: String::new(),
            color: None,
            dielectric_type: Some(kind.to_string()),
            dielectric: sublayers
                .iter()
                .map(
                    |&(thickness_nm, material, epsilon_r, loss_tangent)| IpcStackupDielectric {
                        thickness_nm,
                        material: material.to_string(),
                        epsilon_r,
                        loss_tangent,
                        thickness_locked: false,
                    },
                )
                .collect(),
            epsilon_r: None,
            loss_tangent: None,
        }
    }

    fn mask(name: &str) -> IpcStackupLayer {
        IpcStackupLayer {
            material: NOT_SPECIFIED.to_string(),
            epsilon_r: Some(3.3),
            loss_tangent: Some(0.0),
            ..layer(name, "soldermask", 10_000)
        }
    }

    /// A four-layer stackup as KiCad 10.0.6 served it live for a real board.
    fn four_layer() -> IpcBoardStackup {
        let copper = |name: &str, nm: i64| IpcStackupLayer {
            material: "copper".to_string(),
            ..layer(name, "copper", nm)
        };
        IpcBoardStackup {
            finish: "None".to_string(),
            impedance_controlled: false,
            edge_connector: "none".to_string(),
            has_edge_plating: false,
            layers: vec![
                layer("F.SilkS", "silkscreen", 0),
                layer("F.Paste", "solderpaste", 0),
                mask("F.Mask"),
                copper("F.Cu", 35_000),
                dielectric("prepreg", &[(210_400, "FR4", 4.5, 0.02)]),
                copper("In1.Cu", 15_200),
                dielectric("core", &[(250_000, "FR4", 4.5, 0.02)]),
                copper("In2.Cu", 15_200),
                dielectric("prepreg", &[(210_400, "FR4", 4.5, 0.02)]),
                copper("B.Cu", 35_000),
                mask("B.Mask"),
                layer("B.Paste", "solderpaste", 0),
                layer("B.SilkS", "silkscreen", 0),
            ],
        }
    }

    fn body_for(stackup: &IpcBoardStackup, expected: Value) -> Value {
        let expected = Expected::from_args(&json!({ "expected": expected })).unwrap();
        stackup_body("board.kicad_pcb", stackup, &expected)
    }

    fn fields(body: &Value) -> Vec<(String, String, String)> {
        body["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["name"].as_str().unwrap().to_string(),
                    f["field"].as_str().unwrap().to_string(),
                    f["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn triple(id: &str, field: &str, kind: &str) -> (String, String, String) {
        (id.to_string(), field.to_string(), kind.to_string())
    }

    #[test]
    fn the_board_thickness_is_kicads_sum_of_copper_dielectric_and_mask() {
        // 0.01 + 0.035 + 0.2104 + 0.0152 + 0.25 + 0.0152 + 0.2104 + 0.035 + 0.01,
        // the thickness that board's own file records and KiCad's job file reports.
        let body = body_for(&four_layer(), Value::Null);
        assert!((body["board_thickness_mm"].as_f64().unwrap() - 0.7912).abs() < 1e-9);
        assert_eq!(body["copper_layer_count"], 4);

        // Silkscreen and paste carry no thickness in KiCad's sum even when they
        // report one, a disabled layer drops out, and a dielectric counts every
        // sub-layer.
        let mut stackup = four_layer();
        stackup.layers[0].thickness_nm = 1_000_000;
        stackup.layers[1].thickness_nm = 1_000_000;
        stackup.layers[5].enabled = false;
        stackup.layers[6] = dielectric(
            "core",
            &[(200_000, "FR4", 4.5, 0.02), (50_000, "FR4", 4.4, 0.02)],
        );
        let body = body_for(&stackup, Value::Null);
        assert!((body["board_thickness_mm"].as_f64().unwrap() - 0.776).abs() < 1e-9);
        assert_eq!(body["copper_layer_count"], 3);
    }

    #[test]
    fn dielectrics_are_numbered_in_stack_order_as_kicads_file_names_them() {
        let body = body_for(&four_layer(), Value::Null);
        let names: Vec<&str> = body["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "F.SilkS",
                "F.Paste",
                "F.Mask",
                "F.Cu",
                "dielectric 1",
                "In1.Cu",
                "dielectric 2",
                "In2.Cu",
                "dielectric 3",
                "B.Cu",
                "B.Mask",
                "B.Paste",
                "B.SilkS"
            ]
        );
        assert_eq!(body["layers"][4]["thickness_mm"], 0.2104);
        assert_eq!(body["layers"][4]["sublayers"][0]["epsilon_r"], 4.5);

        // A color KiCad sends is left out of the answer; see `layer_json`.
        let mut stackup = four_layer();
        stackup.layers[2].color = Some("#008000FF".to_string());
        stackup.layers[4].color = Some("#00000000".to_string());
        let body = body_for(&stackup, Value::Null);
        for layer in body["layers"].as_array().unwrap() {
            assert!(layer.get("color").is_none(), "{layer}");
        }
    }

    #[test]
    fn a_fully_specified_stackup_has_no_findings() {
        let body = body_for(&four_layer(), Value::Null);
        assert_eq!(body["finding_count"], 0, "{body}");
        assert_eq!(body["sources"]["stackup"], "ipc");
        assert_eq!(body["source_evidence"]["board_state"], "ipc");
        assert!(body["hint"].as_str().unwrap().contains("Board Setup"));
        assert!(body["note"].as_str().unwrap().contains("default stackup"));
    }

    #[test]
    fn unspecified_fabrication_values_are_reported_missing() {
        let mut stackup = four_layer();
        stackup.finish = NOT_SPECIFIED.to_string();
        stackup.layers[4] = dielectric("prepreg", &[(210_400, NOT_SPECIFIED, 0.0, 0.02)]);
        stackup.layers[3].thickness_nm = 0;
        // A disabled layer is not built, so nothing is missing from it.
        stackup.layers[7].enabled = false;
        stackup.layers[7].thickness_nm = 0;

        let body = body_for(&stackup, Value::Null);
        assert_eq!(
            fields(&body),
            [
                triple("board", "finish", "missing"),
                triple("F.Cu", "thickness_mm", "missing"),
                triple("dielectric 1", "material", "missing"),
                triple("dielectric 1", "epsilon_r", "missing"),
            ]
        );
    }

    #[test]
    fn a_named_finish_of_none_is_a_finish_not_a_gap() {
        // KiCad lists "None" among its finishes, written to the job file; only
        // "Not specified" and an empty name are unset.
        assert!(!unspecified("None"));
        assert!(unspecified("Not specified"));
        assert!(unspecified("  "));
        // "None" is also KiCad's default, which the answer cannot tell apart,
        // so every answer says how to check it.
        let body = body_for(&four_layer(), Value::Null);
        assert_eq!(body["finish"], "None");
        assert!(body["note"].as_str().unwrap().contains("expected.finish"));
        let body = body_for(&four_layer(), json!({ "finish": "ENIG" }));
        assert_eq!(fields(&body), [triple("board", "finish", "mismatch")]);
    }

    #[test]
    fn each_sub_layer_of_a_multi_ply_dielectric_is_its_own_finding() {
        let mut stackup = four_layer();
        stackup.layers[4] = dielectric(
            "prepreg",
            &[
                (210_400, "FR4", 4.4, 0.02),
                (99_400, NOT_SPECIFIED, 4.1, 0.02),
            ],
        );
        let body = body_for(
            &stackup,
            json!({ "layers": [{ "name": "dielectric 1", "epsilon_r": 4.4 }] }),
        );
        let findings = body["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 2, "{body}");
        assert_eq!(findings[0]["kind"], "missing");
        assert_eq!(findings[0]["field"], "material");
        assert_eq!(findings[0]["sublayer"], 2);
        assert_eq!(findings[1]["kind"], "mismatch");
        assert_eq!(findings[1]["field"], "epsilon_r");
        assert_eq!(findings[1]["sublayer"], 2);
        assert!(findings[1]["detail"]
            .as_str()
            .unwrap()
            .contains("sub-layer 2"));

        // A ply added in Board Setup starts with no thickness, which the slot's
        // total does not show.
        stackup.layers[4] = dielectric(
            "prepreg",
            &[(210_400, "FR4", 4.4, 0.02), (0, "FR4", 4.4, 0.02)],
        );
        let body = body_for(&stackup, Value::Null);
        assert_eq!(
            fields(&body),
            [triple("dielectric 1", "thickness_mm", "missing")],
            "{body}"
        );
        assert_eq!(body["findings"][0]["sublayer"], 2);

        // A single-ply slot needs no number.
        let body = body_for(
            &four_layer(),
            json!({ "layers": [{ "name": "dielectric 1", "epsilon_r": 4.05 }] }),
        );
        assert!(body["findings"][0].get("sublayer").is_none(), "{body}");
    }

    #[test]
    fn the_values_the_board_holds_match_themselves() {
        let body = body_for(
            &four_layer(),
            json!({
                "board_thickness_mm": 0.7912,
                "copper_layer_count": 4,
                "finish": "none",
                "impedance_controlled": false,
                "layers": [
                    { "name": "F.Cu", "thickness_mm": 0.035 },
                    { "name": "dielectric 1", "thickness_mm": 0.2104, "material": "fr4", "epsilon_r": 4.5, "loss_tangent": 0.02 },
                    { "name": "F.Mask", "thickness_mm": 0.01, "epsilon_r": 3.3 }
                ]
            }),
        );
        assert_eq!(body["finding_count"], 0, "{body}");
    }

    #[test]
    fn every_difference_from_the_expected_stackup_is_reported() {
        let body = body_for(
            &four_layer(),
            json!({
                "board_thickness_mm": 1.6,
                "copper_layer_count": 2,
                "finish": "ENIG",
                "impedance_controlled": true,
                "layers": [
                    { "name": "dielectric 1", "thickness_mm": 0.2, "material": "FR408", "epsilon_r": 4.05, "loss_tangent": 0.01 },
                    { "name": "In1.Cu", "thickness_mm": 0.035 },
                    { "name": "F.Cu", "epsilon_r": 1.0 },
                    { "name": "dielectric 9" }
                ]
            }),
        );
        assert_eq!(
            fields(&body),
            [
                triple("board", "board_thickness_mm", "mismatch"),
                triple("board", "copper_layer_count", "mismatch"),
                triple("board", "finish", "mismatch"),
                triple("board", "impedance_controlled", "mismatch"),
                triple("dielectric 1", "thickness_mm", "mismatch"),
                triple("dielectric 1", "material", "mismatch"),
                triple("dielectric 1", "epsilon_r", "mismatch"),
                triple("dielectric 1", "loss_tangent", "mismatch"),
                triple("In1.Cu", "thickness_mm", "mismatch"),
                triple("F.Cu", "epsilon_r", "not_reported"),
                triple("dielectric 9", "name", "unknown_layer"),
            ]
        );
        let thickness = &body["findings"][0];
        assert_eq!(thickness["expected"], 1.6);
        assert!((thickness["actual"].as_f64().unwrap() - 0.7912).abs() < 1e-9);
        assert!(body["findings"][10]["detail"]
            .as_str()
            .unwrap()
            .contains("dielectric 3"));
    }

    #[test]
    fn thickness_within_a_micrometre_is_the_same_thickness() {
        let body = body_for(
            &four_layer(),
            json!({ "layers": [{ "name": "dielectric 2", "thickness_mm": 0.2509 }] }),
        );
        assert_eq!(body["finding_count"], 0, "{body}");
        let body = body_for(
            &four_layer(),
            json!({ "layers": [{ "name": "dielectric 2", "thickness_mm": 0.2511 }] }),
        );
        assert_eq!(body["finding_count"], 1, "{body}");
    }

    #[test]
    fn a_material_kicad_does_not_report_is_not_reported_not_a_mismatch() {
        let body = body_for(
            &four_layer(),
            json!({ "layers": [
                { "name": "F.Paste", "material": "SAC305" },
                { "name": "F.Cu", "material": "copper" },
            ] }),
        );
        assert_eq!(
            fields(&body),
            [triple("F.Paste", "material", "not_reported")],
            "{body}"
        );
    }

    #[test]
    fn a_malformed_expected_is_refused_on_a_direct_call() {
        for (expected, field) in [
            (json!("1.6 mm"), "expected"),
            (
                json!({ "board_thickness_mm": "1.6" }),
                "expected.board_thickness_mm",
            ),
            (
                json!({ "copper_layer_count": 0 }),
                "expected.copper_layer_count",
            ),
            (json!({ "finish": 3 }), "expected.finish"),
            (json!({ "layers": {} }), "expected.layers"),
            (
                json!({ "layers": [{ "thickness_mm": 0.035 }] }),
                "expected.layers",
            ),
        ] {
            let refused = Expected::from_args(&json!({ "expected": expected }))
                .expect_err("malformed expected");
            let text = match &refused.content[0] {
                crate::mcp::protocol::ToolContent::Text { text } => text.clone(),
                other => panic!("expected text, got {other:?}"),
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["error"]["field"], field, "{text}");
        }
    }

    /// A misspelt field would otherwise be compared against nothing and the
    /// call would report no findings: the served schema is closed at every
    /// level, so it is refused before dispatch.
    #[test]
    fn the_served_schema_refuses_a_field_it_does_not_compare() {
        let def = tool();
        let call = |expected: Value| json!({ "board": "/b.kicad_pcb", "expected": expected });
        assert!(def.input_validator.is_valid(&call(json!({
            "board_thickness_mm": 0.7912,
            "layers": [{ "name": "dielectric 1", "epsilon_r": 4.5 }],
        }))));
        for expected in [
            json!({ "board_thicknes_mm": 0.7912 }),
            json!({ "layers": [{ "name": "dielectric 1", "er": 4.5 }] }),
            json!({ "board_thickness_mm": 0.0 }),
            json!({ "layers": [{ "name": "dielectric 1", "loss_tangent": -0.02 }] }),
        ] {
            assert!(
                !def.input_validator.is_valid(&call(expected.clone())),
                "{expected}"
            );
        }
    }

    // ─── Through the handler, against a KiCad double ─────────────────────────

    fn stackup_reply() -> prost_types::Any {
        konnect_ipc::builders::pack_any(
            &kiapi::board::commands::BoardStackupResponse {
                stackup: Some(kiapi::board::BoardStackup {
                    finish: Some(kiapi::board::BoardFinish {
                        type_name: "None".to_string(),
                    }),
                    layers: vec![
                        kiapi::board::BoardStackupLayer {
                            thickness: Some(kiapi::common::types::Distance { value_nm: 35_000 }),
                            layer: kiapi::board::types::BoardLayer::BlFCu as i32,
                            enabled: true,
                            r#type: kiapi::board::BoardStackupLayerType::BsltCopper as i32,
                            material_name: "copper".to_string(),
                            ..Default::default()
                        },
                        kiapi::board::BoardStackupLayer {
                            thickness: Some(kiapi::common::types::Distance {
                                value_nm: 1_510_000,
                            }),
                            layer: kiapi::board::types::BoardLayer::BlUndefined as i32,
                            enabled: true,
                            r#type: kiapi::board::BoardStackupLayerType::BsltDielectric as i32,
                            details: Some(kiapi::board::board_stackup_layer::Details::Dielectric(
                                kiapi::board::BoardStackupDielectricLayer {
                                    layer: vec![kiapi::board::BoardStackupDielectricProperties {
                                        epsilon_r: 4.5,
                                        loss_tangent: 0.02,
                                        material_name: "FR4".to_string(),
                                        thickness: Some(kiapi::common::types::Distance {
                                            value_nm: 1_510_000,
                                        }),
                                        ..Default::default()
                                    }],
                                    r#type: kiapi::board::BoardStackupDielectricType::BsdtCore
                                        as i32,
                                },
                            )),
                            ..Default::default()
                        },
                        kiapi::board::BoardStackupLayer {
                            thickness: Some(kiapi::common::types::Distance { value_nm: 35_000 }),
                            layer: kiapi::board::types::BoardLayer::BlBCu as i32,
                            enabled: true,
                            r#type: kiapi::board::BoardStackupLayerType::BsltCopper as i32,
                            material_name: "copper".to_string(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
            },
            "kiapi.board.commands.BoardStackupResponse",
        )
    }

    fn text_of(result: &CallToolResult) -> String {
        match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A board path with nothing at it: the tool must never need the file.
    fn absent_board() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("stack.kicad_pcb");
        (dir, board)
    }

    #[tokio::test]
    async fn the_live_board_is_read_and_the_request_names_its_document() {
        let (_dir, board) = absent_board();
        let named = Arc::new(Mutex::new(Vec::new()));
        let seen = named.clone();
        let server = spawn_kicad_holding_board(&board, move |command| {
            if command.type_url.ends_with("GetBoardStackup") {
                let get = <kiapi::board::commands::GetBoardStackup as prost::Message>::decode(
                    command.value.as_slice(),
                )
                .unwrap();
                seen.lock().unwrap().push(get.board);
                return Some(stackup_reply());
            }
            None
        });
        let ctx = ctx_talking_to(server.address().to_string());

        let result = handle_get_board_stackup(
            &json!({ "board": board, "expected": { "board_thickness_mm": 1.58 } }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["copper_layer_count"], 2);
        assert!((body["board_thickness_mm"].as_f64().unwrap() - 1.58).abs() < 1e-9);
        assert_eq!(body["layers"][1]["name"], "dielectric 1");
        assert_eq!(body["layers"][1]["dielectric_type"], "core");
        assert_eq!(body["finding_count"], 0, "{body}");
        assert_eq!(
            *named.lock().unwrap(),
            [Some(board_document(&board.to_string_lossy()))],
            "one GetBoardStackup, addressed to the board KiCad resolved"
        );
        assert!(
            !board.exists(),
            "the saved file is neither read nor written"
        );
    }

    #[tokio::test]
    async fn a_board_kicad_does_not_hold_is_refused_without_a_file_fallback() {
        let (dir, board) = absent_board();
        let other = dir.path().join("other.kicad_pcb");
        let server = spawn_kicad_holding_boards(&[&other], |_| Some(stackup_reply()));
        let ctx = ctx_talking_to(server.address().to_string());

        let result = handle_get_board_stackup(&json!({ "board": board }), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "wrong_document", "{body}");
    }

    #[tokio::test]
    async fn no_kicad_at_all_is_refused_as_editor_unavailable() {
        let (_dir, board) = absent_board();
        let ctx = ctx_talking_to(String::new());

        let result = handle_get_board_stackup(&json!({ "board": board }), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "editor_unavailable", "{body}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("no file fallback"));
    }

    #[tokio::test]
    async fn an_answer_konnect_cannot_read_is_its_own_failure_not_kicads() {
        let (_dir, board) = absent_board();
        // KiCad answers `AS_OK` with no body: Konnect's read fails, and the
        // result says so rather than reporting KiCad as having refused.
        let server = spawn_kicad_holding_board(&board, |_| None);
        let ctx = ctx_talking_to(server.address().to_string());

        let result = handle_get_board_stackup(&json!({ "board": board }), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("returned no stackup"));
    }

    /// A KiCad whose every answer is `status`, as the project manager alone
    /// answers (`AS_UNHANDLED`) or a build without the open-document command
    /// (`AS_UNIMPLEMENTED`).
    fn spawn_kicad_answering(
        status: kiapi::common::ApiStatusCode,
    ) -> crate::test_support::MockIpcServer {
        crate::test_support::MockIpcServer::spawn("stackup-status", move |request| {
            assert!(request.message.is_some(), "a command");
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: status as i32,
                    error_message: format!("the double answers {}", status.as_str_name()),
                }),
                header: None,
                message: None,
            }
        })
    }

    #[tokio::test]
    async fn a_kicad_serving_no_board_is_editor_unavailable_without_a_file_fallback() {
        for status in [
            kiapi::common::ApiStatusCode::AsUnhandled,
            kiapi::common::ApiStatusCode::AsUnimplemented,
        ] {
            let (_dir, board) = absent_board();
            let server = spawn_kicad_answering(status);
            let ctx = ctx_talking_to(server.address().to_string());

            let result = handle_get_board_stackup(&json!({ "board": board }), &ctx)
                .await
                .unwrap();

            assert!(result.is_error, "{status:?}");
            let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
            assert_eq!(body["error"]["kind"], "editor_unavailable", "{body}");
            assert!(body["message"]
                .as_str()
                .unwrap()
                .contains("no file fallback"));
        }
    }

    #[tokio::test]
    async fn kicad_refusing_the_read_is_a_handler_error_carrying_its_reason() {
        let (_dir, board) = absent_board();
        let document = board_document(&board.to_string_lossy());
        let server = crate::test_support::MockIpcServer::spawn("stackup-refused", move |request| {
            let command = request.message.expect("a command");
            let (status, message) = if command.type_url.ends_with("GetOpenDocuments") {
                (
                    kiapi::common::ApiStatusCode::AsOk,
                    Some(konnect_ipc::builders::pack_any(
                        &kiapi::common::commands::GetOpenDocumentsResponse {
                            documents: vec![document.clone()],
                        },
                        "kiapi.common.commands.GetOpenDocumentsResponse",
                    )),
                )
            } else {
                (kiapi::common::ApiStatusCode::AsBusy, None)
            };
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: status as i32,
                    error_message: "the double is busy".to_string(),
                }),
                header: None,
                message,
            }
        });
        let ctx = ctx_talking_to(server.address().to_string());

        let result = handle_get_board_stackup(&json!({ "board": board }), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("the double is busy"),
            "{body}"
        );
    }
}
