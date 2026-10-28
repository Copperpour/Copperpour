//! Live, revision-aware edits to 3D models on placed footprints (#305).

use crate::hex::encode_hex;
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{
    get_path, invalid_arg, ipc_target_error_result, require_str, with_bound_board_ipc_classified,
    BoardAccess, BoardBinding, ToolContext, ToolDef,
};
use anyhow::Error;
use konnect_ipc::client::FootprintTargetError;
use konnect_ipc::{
    IpcFootprint3DModel, IpcFootprint3DModelEdit, IpcFootprint3DModelEditOutcome,
    IpcFootprint3DModelSnapshot, IpcVector3,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn tool() -> ToolDef {
    tool!(
        "set_placed_footprint_models",
        "Inspect, append, replace, or remove one exact 3D-model entry on a placed footprint. \
         Requires the requested board open in KiCad and uses one native undo transaction; it \
         never rewrites an open board file. Inspect first, then pass its exact models_revision \
         for every mutation. Results come from a fresh live readback.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string" },
                "reference": { "type": "string", "description": "Exact reference designator, e.g. U1" },
                "mode": { "type": "string", "enum": ["inspect", "append", "replace", "remove"] },
                "model_index": { "type": "integer", "minimum": 0, "description": "Exact zero-based entry index; required for replace/remove" },
                "expected_models_revision": { "type": "string", "description": "Exact revision from inspect; required for append/replace/remove" },
                "model": {
                    "type": "object",
                    "description": "Complete replacement model; required for append/replace",
                    "properties": {
                        "path": { "type": "string" },
                        "offset_mm": { "$ref": "#/$defs/vector3Zero" },
                        "rotation_degrees": { "$ref": "#/$defs/vector3Zero" },
                        "scale": { "$ref": "#/$defs/vector3One" },
                        "visible": { "type": "boolean", "default": true },
                        "opacity": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 }
                    },
                    "required": ["path"]
                }
            },
            "required": ["board", "reference", "mode"],
            "$defs": {
                "vector3Zero": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "default": 0.0 },
                        "y": { "type": "number", "default": 0.0 },
                        "z": { "type": "number", "default": 0.0 }
                    }
                },
                "vector3One": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "default": 1.0 },
                        "y": { "type": "number", "default": 1.0 },
                        "z": { "type": "number", "default": 1.0 }
                    }
                }
            }
        }),
        |args, ctx| async move { handle_set_placed_footprint_models(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

enum LiveResult {
    Snapshot(Result<IpcFootprint3DModelSnapshot, Error>),
    Conflict(IpcFootprint3DModelSnapshot),
    Edited(Result<IpcFootprint3DModelEditOutcome, Error>),
}

async fn handle_set_placed_footprint_models(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match require_str(args, "reference") {
        Ok(value) => value.to_string(),
        Err(result) => return Ok(result),
    };
    if reference.trim().is_empty() {
        return Ok(invalid_arg("reference", "must not be empty"));
    }
    let mode = match require_str(args, "mode") {
        Ok(value) => value,
        Err(result) => return Ok(result),
    };
    let edit = match parse_edit(args, mode) {
        Ok(edit) => edit,
        Err(result) => return Ok(result),
    };
    let expected_revision = if edit.is_some() {
        match args.get("expected_models_revision").and_then(Value::as_str) {
            Some(value) if !value.is_empty() => Some(value.to_string()),
            _ => {
                return Ok(invalid_arg(
                    "expected_models_revision",
                    "is required for append, replace, and remove; inspect first",
                ))
            }
        }
    } else {
        None
    };
    let live_reference = reference.clone();
    let revision_board = board.clone();
    let live = with_bound_board_ipc_classified(ctx, &board, move |client, document| {
        let snapshot = client.footprint_3d_model_snapshot_in(document.clone(), &live_reference);
        let Some(edit) = edit else {
            return Ok(LiveResult::Snapshot(snapshot));
        };
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => return Ok(LiveResult::Snapshot(Err(error))),
        };
        if expected_revision.as_deref()
            != Some(models_revision(&revision_board, &snapshot).as_str())
        {
            return Ok(LiveResult::Conflict(snapshot));
        }
        Ok(LiveResult::Edited(client.edit_footprint_3d_models_in(
            document,
            &live_reference,
            snapshot,
            edit,
        )))
    })
    .await?;

    let live = match live {
        Ok(BoardBinding::Bound(value)) => value,
        Ok(BoardBinding::Unserved { message, .. }) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::EditorUnavailable {
                    editor: "pcb".to_string(),
                    reason: message.clone(),
                },
                format!("KiCad must be running with the requested board open: {message}"),
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
                format!("KiCad could not serve the requested live-board operation: {error}"),
            ))
        }
    };

    match live {
        LiveResult::Snapshot(Ok(snapshot)) => {
            Ok(snapshot_result("inspected", false, &board, &snapshot))
        }
        LiveResult::Snapshot(Err(error)) | LiveResult::Edited(Err(error)) => {
            Ok(footprint_error(error, &reference))
        }
        LiveResult::Conflict(snapshot) => {
            let actual = models_revision(&board, &snapshot);
            Ok(CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: format!("{} footprint {reference}", board.display()),
                    reason: format!("3D-model revision is now {actual}"),
                },
                format!(
                    "The placed footprint's 3D-model list changed after inspection. Re-inspect and retry with revision {actual}."
                ),
            ))
        }
        LiveResult::Edited(Ok(IpcFootprint3DModelEditOutcome::Applied {
            after, changed, ..
        })) => Ok(snapshot_result(
            if changed { "applied" } else { "unchanged" },
            changed,
            &board,
            &after,
        )),
        LiveResult::Edited(Ok(IpcFootprint3DModelEditOutcome::Conflict { observed, .. })) => {
            let actual = models_revision(&board, &observed);
            Ok(CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: format!("{} footprint {reference}", board.display()),
                    reason: format!("3D-model list changed immediately before mutation; revision is now {actual}"),
                },
                format!("The model list changed before KiCad could apply the edit. Re-inspect and retry with revision {actual}."),
            ))
        }
        LiveResult::Edited(Ok(IpcFootprint3DModelEditOutcome::Uncertain {
            reason, ..
        })) => Ok(CallToolResult::error_kind(
            ToolErrorKind::MutationOutcomeUncertain {
                operation: "set_placed_footprint_models".to_string(),
                path: board.display().to_string(),
                reason: reason.clone(),
            },
            format!(
                "KiCad may have changed footprint {reference}, but fresh readback did not prove the requested model list. Inspect before retrying. {reason}"
            ),
        )),
    }
}

fn parse_edit(args: &Value, mode: &str) -> Result<Option<IpcFootprint3DModelEdit>, CallToolResult> {
    let index = || {
        args.get("model_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid_arg("model_index", "is required for replace and remove"))
    };
    let model = || {
        args.get("model")
            .ok_or_else(|| invalid_arg("model", "is required for append and replace"))
            .and_then(parse_model)
    };
    match mode {
        "inspect" => Ok(None),
        "append" => Ok(Some(IpcFootprint3DModelEdit::Append { model: model()? })),
        "replace" => Ok(Some(IpcFootprint3DModelEdit::Replace {
            index: index()?,
            model: model()?,
        })),
        "remove" => Ok(Some(IpcFootprint3DModelEdit::Remove { index: index()? })),
        _ => Err(invalid_arg(
            "mode",
            "must be inspect, append, replace, or remove",
        )),
    }
}

fn parse_model(value: &Value) -> Result<IpcFootprint3DModel, CallToolResult> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if path.trim().is_empty() || path.contains('\0') {
        return Err(invalid_arg(
            "model.path",
            "must be non-empty and contain no NUL",
        ));
    }
    let offset_mm = parse_vector(value.get("offset_mm"), 0.0, "model.offset_mm")?;
    let rotation_degrees =
        parse_vector(value.get("rotation_degrees"), 0.0, "model.rotation_degrees")?;
    let scale = parse_vector(value.get("scale"), 1.0, "model.scale")?;
    let opacity = value.get("opacity").and_then(Value::as_f64).unwrap_or(1.0);
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(invalid_arg(
            "model.opacity",
            "must be finite and between 0 and 1",
        ));
    }
    Ok(IpcFootprint3DModel {
        filename: path.to_string(),
        offset_mm,
        rotation_degrees,
        scale,
        visible: value
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        opacity,
    })
}

fn parse_vector(
    value: Option<&Value>,
    default: f64,
    field: &str,
) -> Result<IpcVector3, CallToolResult> {
    let component = |name| {
        value
            .and_then(|value| value.get(name))
            .and_then(Value::as_f64)
            .unwrap_or(default)
    };
    let vector = IpcVector3 {
        x: component("x"),
        y: component("y"),
        z: component("z"),
    };
    if [vector.x, vector.y, vector.z]
        .into_iter()
        .all(f64::is_finite)
    {
        Ok(vector)
    } else {
        Err(invalid_arg(field, "components must be finite numbers"))
    }
}

fn models_revision(board: &std::path::Path, snapshot: &IpcFootprint3DModelSnapshot) -> String {
    let mut hasher = Sha256::new();
    hasher.update(board.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(snapshot.reference.as_bytes());
    hasher.update([0]);
    hasher.update(snapshot.kiid.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(&snapshot.models).expect("model snapshots serialize"));
    encode_hex(&hasher.finalize())
}

fn snapshot_result(
    status: &str,
    applied: bool,
    board: &std::path::Path,
    snapshot: &IpcFootprint3DModelSnapshot,
) -> CallToolResult {
    let models = snapshot
        .models
        .iter()
        .enumerate()
        .map(|(index, model)| json!({ "index": index, "model": model }))
        .collect::<Vec<_>>();
    CallToolResult::json(&json!({
        "status": status,
        "source": "ipc",
        "applied": applied,
        "reference": snapshot.reference,
        "kiid": snapshot.kiid,
        "models_revision": models_revision(board, snapshot),
        "models": models,
        "model_count": models.len(),
        "undo": if applied { Value::String("one KiCad undo step".to_string()) } else { Value::Null }
    }))
}

fn footprint_error(error: Error, reference: &str) -> CallToolResult {
    if let Some(target) = error.downcast_ref::<FootprintTargetError>() {
        return match target {
            FootprintTargetError::Missing { .. } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: reference.to_string(),
                    reason: "no placed footprint has this exact reference".to_string(),
                },
                target.to_string(),
            ),
            FootprintTargetError::Ambiguous { matches, .. } => CallToolResult::error_kind(
                ToolErrorKind::AmbiguousTarget {
                    target: reference.to_string(),
                    candidates: (0..*matches)
                        .map(|index| format!("match {index}"))
                        .collect(),
                },
                target.to_string(),
            ),
        };
    }
    CallToolResult::error_kind(
        ToolErrorKind::HandlerError {
            reason: format!("{error:#}"),
        },
        format!("Placed-footprint 3D-model operation failed: {error:#}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path: &str) -> IpcFootprint3DModelSnapshot {
        IpcFootprint3DModelSnapshot {
            reference: "U1".to_string(),
            kiid: "abc".to_string(),
            models: vec![IpcFootprint3DModel {
                filename: path.to_string(),
                offset_mm: IpcVector3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                rotation_degrees: IpcVector3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                scale: IpcVector3 {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
                visible: true,
                opacity: 1.0,
            }],
        }
    }

    #[test]
    fn revision_tracks_exact_ordered_model_state() {
        assert_eq!(
            models_revision(std::path::Path::new("board.kicad_pcb"), &snapshot("a.step")),
            models_revision(std::path::Path::new("board.kicad_pcb"), &snapshot("a.step"))
        );
        assert_ne!(
            models_revision(std::path::Path::new("board.kicad_pcb"), &snapshot("a.step")),
            models_revision(std::path::Path::new("board.kicad_pcb"), &snapshot("b.step"))
        );
    }

    #[test]
    fn mutation_modes_require_their_exact_arguments() {
        assert!(parse_edit(&json!({}), "inspect").unwrap().is_none());
        assert!(parse_edit(&json!({}), "append").is_err());
        assert!(parse_edit(&json!({ "model": { "path": "x.step" } }), "replace").is_err());
        assert!(parse_edit(&json!({}), "remove").is_err());
    }

    #[test]
    fn model_defaults_are_complete_and_invalid_values_are_refused() {
        let model = parse_model(&json!({ "path": "x.step" })).unwrap();
        assert_eq!(
            model.scale,
            IpcVector3 {
                x: 1.0,
                y: 1.0,
                z: 1.0
            }
        );
        assert_eq!(model.opacity, 1.0);
        assert!(parse_model(&json!({ "path": "x.step", "opacity": 2.0 })).is_err());
        assert!(parse_model(&json!({ "path": "" })).is_err());
    }
}
