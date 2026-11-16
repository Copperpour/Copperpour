//! `add_rule_area`: create a KiCad rule area (keep-out zone) on enabled
//! copper layers of a live board open in KiCad (#77).
//!
//! Creates a typed `ZT_RULE_AREA` via `CreateItems` inside an undoable commit,
//! reads back KiCad's stored item by KIID to verify layers, geometry, and
//! keep-out flags, and refills copper zones afterward. No closed-board file
//! fallback exists in this increment: if KiCad is unreachable or holds another
//! board, the tool refuses without touching the file.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::outcome::{self, OutcomeStatus};
use crate::tool;
use crate::tools::pcb_board_outline::{mm_text, COORDINATE_LIMIT_NM};
use crate::tools::{
    get_path, invalid_arg, ipc_failure_result, ipc_target_error_result, opt_bool_flag,
    with_bound_board_ipc_classified, BoardAccess, BoardBinding, ToolContext, ToolDef,
};
use copperpour_ipc::builders::{self, nm_to_mm, RuleAreaSpec};
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::gen::kiapi::board::types::{
    zone::Settings as ZoneSettings, BoardLayer, RuleAreaSettings, Zone, ZoneType,
};
use copperpour_ipc::gen::kiapi::common::types::{poly_line_node::Geometry, DocumentSpecifier};
use prost::Message;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const ZONE_TYPE_URL: &str = "kiapi.board.types.Zone";
const UNDO_DESCRIPTION: &str = "Add PCB rule area";
const REFILL_SETTLE_DELAY: Duration = Duration::from_millis(500);
const REFILL_POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// User-facing warning required by #77 until #81 ships rule-area support in
/// Copperpour's Freerouting bridge.
pub(crate) const FREEROUTING_RULE_AREA_WARNING: &str =
    "A board with a rule area cannot currently use Copperpour's Freerouting round trip: \
     export_specctra_dsn refuses all rule areas (tracked in #81).";

pub(crate) fn tools() -> Vec<ToolDef> {
    vec![tool()]
}

pub(crate) fn tool() -> ToolDef {
    tool!(
        "add_rule_area",
        "Add a KiCad rule area (keep-out zone) on one or more enabled copper layers of a live \
         board open in KiCad. Accepts either a polygon ('points') or an axis-aligned rectangle \
         ('rect') in millimetres, plus keepout flags defaulting to KiCad's new-rule-area defaults \
         (tracks, vias, pads disallowed; copper pours and footprints allowed). Creates a typed \
         ZT_RULE_AREA in one undoable KiCad commit, reads back what KiCad stored, and refills \
         copper zones afterward. Requires the requested board open in KiCad; refuses without \
         modifying the file when KiCad is unreachable or holds another board. Warning: a board \
         with a rule area cannot currently use Copperpour's Freerouting round trip \
         (export_specctra_dsn refuses rule areas; tracked in #81).",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "layers": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Enabled copper layer names (e.g. ['F.Cu', 'B.Cu']), or ['*.Cu'] for every enabled copper layer on the board",
                    "items": { "type": "string", "minLength": 1 }
                },
                "points": {
                    "type": "array",
                    "minItems": 3,
                    "description": "Polygon vertices as [{x, y}] in mm; provide either points or rect, not both",
                    "items": {
                        "type": "object",
                        "properties": {
                            "x": { "type": "number" },
                            "y": { "type": "number" }
                        },
                        "required": ["x", "y"]
                    }
                },
                "rect": {
                    "type": "object",
                    "description": "Axis-aligned rectangle {x1, y1, x2, y2} in mm; provide either rect or points, not both",
                    "properties": {
                        "x1": { "type": "number" },
                        "y1": { "type": "number" },
                        "x2": { "type": "number" },
                        "y2": { "type": "number" }
                    },
                    "required": ["x1", "y1", "x2", "y2"]
                },
                "name": { "type": "string", "description": "Optional rule-area name" },
                "keepout_tracks": { "type": "boolean", "default": true, "description": "Disallow tracks inside the rule area" },
                "keepout_vias": { "type": "boolean", "default": true, "description": "Disallow vias inside the rule area" },
                "keepout_pads": { "type": "boolean", "default": true, "description": "Disallow pads inside the rule area" },
                "keepout_copper_pours": { "type": "boolean", "default": false, "description": "Disallow copper pours (zones) inside the rule area" },
                "keepout_footprints": { "type": "boolean", "default": false, "description": "Disallow footprints inside the rule area" }
            },
            "required": ["board", "layers"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_add_rule_area(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerSelection {
    wildcard: bool,
    explicit: Vec<BoardLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuleAreaRequest {
    layers: LayerSelection,
    points_nm: Vec<(i64, i64)>,
    name: String,
    keepout_tracks: bool,
    keepout_vias: bool,
    keepout_pads: bool,
    keepout_copper_pours: bool,
    keepout_footprints: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedRuleArea {
    uuid: String,
    name: String,
    layers: Vec<String>,
    layer_ids: Vec<BoardLayer>,
    points_nm: Vec<(i64, i64)>,
    keepout_tracks: bool,
    keepout_vias: bool,
    keepout_pads: bool,
    keepout_copper_pours: bool,
    keepout_footprints: bool,
}

enum LiveOutcome {
    Refused(CallToolResult),
    Uncertain {
        kiid: Option<String>,
        reason: String,
        observed: Option<ObservedRuleArea>,
    },
    Applied(Box<AppliedOutcome>),
}

struct AppliedOutcome {
    observed: ObservedRuleArea,
    refill_accepted: bool,
    refill_completed: bool,
    board_ready_after_refill: bool,
    copper_zone_count: usize,
    copper_zones_filled: Option<bool>,
    rule_area_still_present: Option<bool>,
    diagnostic: Option<String>,
}

async fn handle_add_rule_area(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board_path = match get_path(args, "board") {
        Ok(path) => path,
        Err(error) => {
            let reason = error.to_string();
            return Ok(refused_outcome(
                invalid_arg("board", &reason),
                args["board"].as_str().unwrap_or("unknown board"),
                "pre_ipc",
            ));
        }
    };
    let request = match parse_request(args) {
        Ok(req) => req,
        Err(result) => {
            return Ok(refused_outcome(
                result,
                board_path.display().to_string(),
                "pre_ipc",
            ))
        }
    };

    let board_target = board_path.display().to_string();
    let live = with_bound_board_ipc_classified(ctx, &board_path, move |client, document| {
        execute_live(client, document, &request, REFILL_SETTLE_DELAY)
    })
    .await?;

    let outcome = match live {
        Ok(BoardBinding::Bound(outcome)) => outcome,
        Ok(BoardBinding::Unserved { message, .. }) => {
            return Ok(refused_outcome(
                CallToolResult::error_kind(
                    ToolErrorKind::EditorUnavailable {
                        editor: "pcb".to_string(),
                        reason: message.clone(),
                    },
                    format!(
                        "KiCad must be running with the requested board open in the PCB Editor: {message}. \
                         The board file was not modified."
                    ),
                ),
                board_target,
                "ipc",
            ));
        }
        Err(failure) => {
            let uncertain = matches!(failure, copperpour_ipc::IpcFailure::Uncertain(_));
            let result = match failure {
                copperpour_ipc::IpcFailure::Unreachable(message) => CallToolResult::error_kind(
                    ToolErrorKind::EditorUnavailable {
                        editor: "pcb".to_string(),
                        reason: message.clone(),
                    },
                    format!(
                        "KiCad must be running with the requested board open in the PCB Editor \
                         (IPC error: {message}). The board file was not modified."
                    ),
                ),
                copperpour_ipc::IpcFailure::Target { error, .. } => ipc_target_error_result(&error),
                copperpour_ipc::IpcFailure::Rejected(reason)
                | copperpour_ipc::IpcFailure::Preflight(copperpour_ipc::PreflightRefusal {
                    reason,
                    ..
                }) => CallToolResult::error_kind(
                    ToolErrorKind::HandlerError {
                        reason: reason.clone(),
                    },
                    reason,
                ),
                other => ipc_failure_result(other),
            };
            return Ok(outcome::attach(
                result,
                outcome::summary(
                    if uncertain {
                        OutcomeStatus::Uncertain
                    } else {
                        OutcomeStatus::Failed
                    },
                    board_target,
                    "ipc",
                    1,
                    0,
                    1,
                    Some(if uncertain {
                        outcome::inspect_before_retry()
                    } else {
                        outcome::retry_whole_request()
                    }),
                ),
            ));
        }
    };

    Ok(match outcome {
        LiveOutcome::Refused(result) => refused_outcome(result, board_target, "ipc"),
        LiveOutcome::Uncertain {
            kiid,
            reason,
            observed,
        } => uncertain_rule_area(board_target, kiid, reason, observed),
        LiveOutcome::Applied(applied) => applied_result(&board_path, *applied),
    })
}

fn uncertain_rule_area(
    board_target: String,
    kiid: Option<String>,
    reason: String,
    observed: Option<ObservedRuleArea>,
) -> CallToolResult {
    let uuid_note = kiid
        .as_deref()
        .map(|id| format!(" (created UUID '{id}')"))
        .unwrap_or_default();
    let mut result = CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: "add_rule_area".to_string(),
            path: board_target.clone(),
            reason: format!("{reason}{uuid_note}"),
        },
        format!(
            "add_rule_area committed on KiCad, but readback did not prove the requested \
             rule area{uuid_note}: {reason}. Inspect the live board before retrying."
        ),
    );
    if let Some(obs) = observed {
        result = attach_observed_to_error(result, &obs);
    }
    outcome::attach(
        result,
        outcome::summary(
            OutcomeStatus::Uncertain,
            board_target,
            "ipc",
            1,
            0,
            1,
            Some(outcome::inspect_before_retry()),
        ),
    )
}

fn refused_outcome(
    result: CallToolResult,
    target: impl Into<String>,
    source: &str,
) -> CallToolResult {
    if outcome::status(&result).is_some() {
        return result;
    }
    outcome::attach(
        result,
        outcome::summary(
            OutcomeStatus::Failed,
            target,
            source,
            1,
            0,
            1,
            Some(outcome::retry_whole_request()),
        ),
    )
}

fn attach_observed_to_error(
    mut result: CallToolResult,
    observed: &ObservedRuleArea,
) -> CallToolResult {
    if let Some(crate::mcp::protocol::ToolContent::Text { text }) = result.content.first_mut() {
        if let Ok(mut body) = serde_json::from_str::<Value>(text) {
            if let Some(map) = body.as_object_mut() {
                map.insert("uuid".to_string(), json!(observed.uuid));
                map.insert("rule_area".to_string(), observed_json(observed));
                if let Ok(serialized) = serde_json::to_string(&body) {
                    *text = serialized;
                }
            }
        }
    }
    result
}

fn applied_result(board_path: &Path, applied: AppliedOutcome) -> CallToolResult {
    if applied.rule_area_still_present == Some(false) {
        let uuid = applied.observed.uuid.clone();
        let reason = applied.diagnostic.unwrap_or_else(|| {
            format!("rule area '{uuid}' disappeared during post-refill readback")
        });
        return uncertain_rule_area(
            board_path.display().to_string(),
            Some(uuid),
            reason,
            Some(applied.observed),
        );
    }
    let complete = applied.refill_accepted
        && applied.refill_completed
        && applied.board_ready_after_refill
        && applied.copper_zones_filled == Some(true)
        && applied.rule_area_still_present == Some(true);
    let status = if complete {
        OutcomeStatus::Complete
    } else {
        OutcomeStatus::Partial
    };
    let result = CallToolResult::json(&json!({
        "success": complete,
        "applied": true,
        "applied_but_not_refilled": !complete,
        "readback_verified": true,
        "refill_request_accepted": applied.refill_accepted,
        "refill_completed": applied.refill_completed,
        "board_ready_after_refill": applied.board_ready_after_refill,
        "copper_zone_count": applied.copper_zone_count,
        "copper_zones_filled": applied.copper_zones_filled,
        "rule_area_present_after_refill": applied.rule_area_still_present,
        "uuid": applied.observed.uuid,
        "rule_area": observed_json(&applied.observed),
        "source": "ipc_readback",
        "board": board_path,
        "undo": format!(
            "One KiCad undo step ('{UNDO_DESCRIPTION}') removes the rule area. The board was not saved."
        ),
        "warning": FREEROUTING_RULE_AREA_WARNING,
        "diagnostic": applied.diagnostic,
    }));
    outcome::attach(
        result,
        outcome::summary(
            status,
            board_path.display().to_string(),
            "ipc_readback",
            1,
            usize::from(complete),
            usize::from(!complete),
            (!complete).then(outcome::inspect_before_retry),
        ),
    )
}

fn observed_json(observed: &ObservedRuleArea) -> Value {
    json!({
        "uuid": observed.uuid,
        "name": observed.name,
        "kind": "rule_area",
        "layers": observed.layers,
        "points": observed.points_nm.iter().map(|&(x, y)| json!({
            "x": nm_to_mm(x),
            "y": nm_to_mm(y),
        })).collect::<Vec<_>>(),
        "point_count": observed.points_nm.len(),
        "keepout_tracks": observed.keepout_tracks,
        "keepout_vias": observed.keepout_vias,
        "keepout_pads": observed.keepout_pads,
        "keepout_copper_pours": observed.keepout_copper_pours,
        "keepout_footprints": observed.keepout_footprints,
    })
}

// ─── Argument validation ──────────────────────────────────────────────────────

fn parse_request(args: &Value) -> Result<RuleAreaRequest, CallToolResult> {
    let layers = parse_layers(args)?;
    let points_nm = parse_geometry(args)?;
    let name = match &args["name"] {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => return Err(invalid_arg("name", "must be a string")),
    };
    let keepout_tracks = opt_bool_flag(args, "keepout_tracks")?.unwrap_or(true);
    let keepout_vias = opt_bool_flag(args, "keepout_vias")?.unwrap_or(true);
    let keepout_pads = opt_bool_flag(args, "keepout_pads")?.unwrap_or(true);
    let keepout_copper_pours = opt_bool_flag(args, "keepout_copper_pours")?.unwrap_or(false);
    let keepout_footprints = opt_bool_flag(args, "keepout_footprints")?.unwrap_or(false);

    Ok(RuleAreaRequest {
        layers,
        points_nm,
        name,
        keepout_tracks,
        keepout_vias,
        keepout_pads,
        keepout_copper_pours,
        keepout_footprints,
    })
}

fn parse_layers(args: &Value) -> Result<LayerSelection, CallToolResult> {
    let raw = match &args["layers"] {
        Value::Array(items) if !items.is_empty() => items,
        Value::Array(_) => {
            return Err(invalid_arg(
                "layers",
                "must contain at least one copper layer or '*.Cu'",
            ))
        }
        _ => {
            return Err(invalid_arg(
                "layers",
                "missing or not a non-empty array of copper layer names",
            ))
        }
    };

    let mut wildcard = false;
    let mut explicit = Vec::new();
    for (index, item) in raw.iter().enumerate() {
        let Some(name) = item.as_str() else {
            return Err(invalid_arg(
                &format!("layers[{index}]"),
                "must be a copper layer name string or '*.Cu'",
            ));
        };
        if name == "*.Cu" {
            wildcard = true;
            continue;
        }
        let Some(layer) = parse_canonical_copper_layer(name) else {
            return Err(invalid_arg(
                &format!("layers[{index}]"),
                &format!(
                    "'{name}' is not a KiCad copper layer; use F.Cu, B.Cu, In1.Cu..In30.Cu, or '*.Cu'"
                ),
            ));
        };
        explicit.push(layer);
    }

    Ok(LayerSelection { wildcard, explicit })
}

fn parse_canonical_copper_layer(name: &str) -> Option<BoardLayer> {
    if !copperpour_sexp::layers::is_copper_name(name) {
        return None;
    }
    let layer = builders::layer_from_name(name);
    (builders::layer_name(layer) == Some(name)).then_some(layer)
}

fn parse_geometry(args: &Value) -> Result<Vec<(i64, i64)>, CallToolResult> {
    let has_points = !args["points"].is_null();
    let has_rect = !args["rect"].is_null();
    match (has_points, has_rect) {
        (true, true) => Err(invalid_arg(
            "points",
            "provide either 'points' or 'rect', not both",
        )),
        (false, false) => Err(invalid_arg(
            "points",
            "either 'points' or 'rect' is required",
        )),
        (true, false) => parse_points(&args["points"]),
        (false, true) => parse_rect(&args["rect"]),
    }
}

fn parse_points(value: &Value) -> Result<Vec<(i64, i64)>, CallToolResult> {
    let Some(items) = value.as_array() else {
        return Err(invalid_arg("points", "must be an array of {x, y} points"));
    };
    if items.len() < 3 {
        return Err(invalid_arg(
            "points",
            "rule area polygon requires at least 3 vertices",
        ));
    }
    let mut pts = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            return Err(invalid_arg(
                &format!("points[{index}]"),
                "must be an object with numeric 'x' and 'y'",
            ));
        };
        let x = parse_mm_coord(
            obj.get("x").unwrap_or(&Value::Null),
            &format!("points[{index}].x"),
        )?;
        let y = parse_mm_coord(
            obj.get("y").unwrap_or(&Value::Null),
            &format!("points[{index}].y"),
        )?;
        pts.push((x, y));
    }
    if pts.len() >= 4 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.windows(2).any(|pair| pair[0] == pair[1]) || pts.first() == pts.last() {
        return Err(invalid_arg(
            "points",
            "polygon contains consecutive duplicate vertices",
        ));
    }
    if twice_signed_area(&pts) == 0 {
        return Err(invalid_arg(
            "points",
            "rule area polygon has zero area (collinear or coincident vertices)",
        ));
    }
    Ok(pts)
}

fn parse_rect(value: &Value) -> Result<Vec<(i64, i64)>, CallToolResult> {
    let Some(obj) = value.as_object() else {
        return Err(invalid_arg(
            "rect",
            "must be an object with numeric x1, y1, x2, y2",
        ));
    };
    let x1 = parse_mm_coord(obj.get("x1").unwrap_or(&Value::Null), "rect.x1")?;
    let y1 = parse_mm_coord(obj.get("y1").unwrap_or(&Value::Null), "rect.y1")?;
    let x2 = parse_mm_coord(obj.get("x2").unwrap_or(&Value::Null), "rect.x2")?;
    let y2 = parse_mm_coord(obj.get("y2").unwrap_or(&Value::Null), "rect.y2")?;
    if x1 == x2 {
        return Err(invalid_arg(
            "rect.x2",
            "must differ from rect.x1 (zero-width rectangle)",
        ));
    }
    if y1 == y2 {
        return Err(invalid_arg(
            "rect.y2",
            "must differ from rect.y1 (zero-height rectangle)",
        ));
    }
    Ok(vec![(x1, y1), (x2, y1), (x2, y2), (x1, y2)])
}

fn parse_mm_coord(value: &Value, field: &str) -> Result<i64, CallToolResult> {
    let Some(mm) = value.as_f64() else {
        return Err(invalid_arg(field, "must be a number of millimetres"));
    };
    let nm = (mm * 1e6).round();
    if !nm.is_finite() || nm.abs() > COORDINATE_LIMIT_NM as f64 {
        return Err(invalid_arg(
            field,
            &format!(
                "{mm} mm is beyond the ±{} mm KiCad keeps on a board",
                mm_text(COORDINATE_LIMIT_NM)
            ),
        ));
    }
    Ok(nm as i64)
}

fn twice_signed_area(points: &[(i64, i64)]) -> i128 {
    let mut sum: i128 = 0;
    for index in 0..points.len() {
        let (x1, y1) = points[index];
        let (x2, y2) = points[(index + 1) % points.len()];
        sum += (x1 as i128) * (y2 as i128) - (x2 as i128) * (y1 as i128);
    }
    sum
}

// ─── Live IPC execution ───────────────────────────────────────────────────────

fn execute_live(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    request: &RuleAreaRequest,
    settle_delay: Duration,
) -> anyhow::Result<LiveOutcome> {
    let enabled = client.get_enabled_layers_in(document.clone())?;
    let enabled_copper = enabled_copper_layers(&enabled.layers);
    if enabled_copper.is_empty() {
        anyhow::bail!("KiCad reported no enabled copper layers for the requested board");
    }

    let resolved_layers = match resolve_enabled_layers(&request.layers, &enabled_copper) {
        Ok(layers) => layers,
        Err(refusal) => return Ok(LiveOutcome::Refused(refusal)),
    };

    let zone = builders::build_rule_area(&RuleAreaSpec {
        layers: &resolved_layers,
        points_nm: &request.points_nm,
        name: &request.name,
        keepout_tracks: request.keepout_tracks,
        keepout_vias: request.keepout_vias,
        keepout_pads: request.keepout_pads,
        keepout_copper_pours: request.keepout_copper_pours,
        keepout_footprints: request.keepout_footprints,
    });
    let any = builders::pack_any(&zone, ZONE_TYPE_URL);

    let mut commit_refusal: Option<String> = None;
    let commit = client.run_commit_recovering_in(document.clone(), UNDO_DESCRIPTION, |client| {
        let results = client.create_items_in_reporting(document.clone(), vec![any])?;
        let Some(created) = results.into_iter().next() else {
            commit_refusal = Some("KiCad returned no item creation result".to_string());
            anyhow::bail!("KiCad returned no item creation result");
        };
        if !created.created {
            let detail = if created.error_message.is_empty() {
                created.status
            } else {
                format!("{} ({})", created.status, created.error_message)
            };
            commit_refusal = Some(format!("KiCad refused to create the rule area: {detail}"));
            anyhow::bail!("KiCad refused to create the rule area");
        }
        let Some(kiid) = created.item.as_ref().and_then(extract_zone_kiid) else {
            commit_refusal = Some(
                "KiCad reported the rule area created without returning a zone KIID, so it \
                 could not be verified; the commit was dropped"
                    .to_string(),
            );
            anyhow::bail!("KiCad returned no zone KIID for created rule area");
        };
        Ok(kiid)
    });

    let kiid = match commit {
        Ok(kiid) => kiid,
        Err(error) => {
            if let Some(reason) = commit_refusal {
                if error
                    .downcast_ref::<copperpour_ipc::client::IpcOutcomeUnknown>()
                    .is_some()
                {
                    return Err(error);
                }
                return Ok(LiveOutcome::Refused(CallToolResult::error_kind(
                    ToolErrorKind::HandlerError {
                        reason: reason.clone(),
                    },
                    format!("{reason}. Nothing was written."),
                )));
            }
            return Err(error);
        }
    };

    let items = match client.get_items_by_id_in(document.clone(), std::slice::from_ref(&kiid)) {
        Ok(items) => items,
        Err(error) => {
            return Ok(LiveOutcome::Uncertain {
                kiid: Some(kiid),
                reason: format!("post-commit readback failed: {error:#}"),
                observed: None,
            })
        }
    };

    let observed = match verify_created_readback(&items, &kiid, &resolved_layers, request) {
        Ok(observed) => observed,
        Err(boxed) => {
            let (reason, observed) = *boxed;
            return Ok(LiveOutcome::Uncertain {
                kiid: Some(kiid),
                reason,
                observed,
            });
        }
    };

    let refill = client.refill_zones_in(document.clone());
    if refill.is_ok() && !settle_delay.is_zero() {
        std::thread::sleep(settle_delay);
    }
    let settled = if refill.is_ok() {
        read_settled_zones(REFILL_POLL_TIMEOUT, &kiid, || {
            client.get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbZone,
            )
        })
    } else {
        // Still try to inspect current zone state without waiting on an unrequested refill.
        client
            .get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbZone,
            )
            .and_then(|items| inspect_zones_after_refill(&items, &kiid))
    };

    Ok(LiveOutcome::Applied(Box::new(build_applied_outcome(
        observed, refill, settled,
    ))))
}

fn enabled_copper_layers(layers: &[copperpour_ipc::IpcLayer]) -> Vec<BoardLayer> {
    let mut out = Vec::new();
    for layer in layers {
        let Ok(bl) = BoardLayer::try_from(layer.id) else {
            continue;
        };
        if is_copper_board_layer(bl) && !out.contains(&bl) {
            out.push(bl);
        }
    }
    out
}

fn is_copper_board_layer(layer: BoardLayer) -> bool {
    let raw = layer as i32;
    (BoardLayer::BlFCu as i32..=BoardLayer::BlBCu as i32).contains(&raw)
}

fn resolve_enabled_layers(
    selection: &LayerSelection,
    enabled_copper: &[BoardLayer],
) -> Result<Vec<BoardLayer>, CallToolResult> {
    let enabled_names = || {
        enabled_copper
            .iter()
            .filter_map(|layer| builders::layer_name(*layer))
            .collect::<Vec<_>>()
            .join(", ")
    };

    for &requested in &selection.explicit {
        if !enabled_copper.contains(&requested) {
            let name = builders::layer_name(requested).unwrap_or("unknown");
            return Err(invalid_arg(
                "layers",
                &format!(
                    "copper layer '{name}' is not enabled on this board; enabled copper layers are {}",
                    enabled_names()
                ),
            ));
        }
    }

    let resolved: Vec<BoardLayer> = enabled_copper
        .iter()
        .copied()
        .filter(|layer| selection.wildcard || selection.explicit.contains(layer))
        .collect();
    if resolved.is_empty() {
        return Err(invalid_arg(
            "layers",
            "resolved to no enabled copper layers",
        ));
    }
    Ok(resolved)
}

fn extract_zone_kiid(any: &prost_types::Any) -> Option<String> {
    if !builders::any_is(any, ZONE_TYPE_URL) {
        return None;
    }
    let zone = Zone::decode(any.value.as_slice()).ok()?;
    let kiid = zone.id?.value;
    (!kiid.is_empty()).then_some(kiid)
}

fn decode_observed_rule_area(zone: &Zone) -> Result<ObservedRuleArea, String> {
    let uuid = zone
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "readback zone has no UUID".to_string())?;
    if zone.r#type != ZoneType::ZtRuleArea as i32 {
        return Err(format!(
            "zone '{uuid}' read back with type {}, expected ZT_RULE_AREA",
            zone.r#type
        ));
    }
    let settings = match zone.settings.as_ref() {
        Some(ZoneSettings::RuleAreaSettings(RuleAreaSettings {
            keepout_copper,
            keepout_vias,
            keepout_tracks,
            keepout_pads,
            keepout_footprints,
            ..
        })) => (
            *keepout_tracks,
            *keepout_vias,
            *keepout_pads,
            *keepout_copper,
            *keepout_footprints,
        ),
        _ => {
            return Err(format!(
                "rule area '{uuid}' read back without RuleAreaSettings"
            ))
        }
    };

    let mut layer_ids = Vec::with_capacity(zone.layers.len());
    let mut layers = Vec::with_capacity(zone.layers.len());
    for &id in &zone.layers {
        let bl = BoardLayer::try_from(id)
            .map_err(|_| format!("rule area '{uuid}' read back with unknown layer id {id}"))?;
        let name = builders::layer_name(bl)
            .ok_or_else(|| format!("rule area '{uuid}' read back with unmapped layer {bl:?}"))?;
        layer_ids.push(bl);
        layers.push(name.to_string());
    }

    let points_nm = extract_outline_points(zone, &uuid)?;
    Ok(ObservedRuleArea {
        uuid,
        name: zone.name.clone(),
        layers,
        layer_ids,
        points_nm,
        keepout_tracks: settings.0,
        keepout_vias: settings.1,
        keepout_pads: settings.2,
        keepout_copper_pours: settings.3,
        keepout_footprints: settings.4,
    })
}

fn extract_outline_points(zone: &Zone, uuid: &str) -> Result<Vec<(i64, i64)>, String> {
    let outline = zone
        .outline
        .as_ref()
        .ok_or_else(|| format!("rule area '{uuid}' read back without an outline"))?;
    if outline.polygons.len() != 1 {
        return Err(format!(
            "rule area '{uuid}' read back with {} polygon(s), expected 1",
            outline.polygons.len()
        ));
    }
    let poly = &outline.polygons[0];
    if !poly.holes.is_empty() {
        return Err(format!(
            "rule area '{uuid}' read back with unexpected polygon holes"
        ));
    }
    let polyline = poly
        .outline
        .as_ref()
        .ok_or_else(|| format!("rule area '{uuid}' read back without a polyline"))?;
    if !polyline.closed {
        return Err(format!(
            "rule area '{uuid}' read back with an unclosed outline"
        ));
    }
    let mut points = Vec::with_capacity(polyline.nodes.len());
    for node in &polyline.nodes {
        match node.geometry.as_ref() {
            Some(Geometry::Point(pt)) => points.push((pt.x_nm, pt.y_nm)),
            other => {
                return Err(format!(
                    "rule area '{uuid}' read back with non-point outline geometry {other:?}"
                ))
            }
        }
    }
    Ok(points)
}

fn verify_created_readback(
    items: &[prost_types::Any],
    expected_kiid: &str,
    expected_layers: &[BoardLayer],
    request: &RuleAreaRequest,
) -> Result<ObservedRuleArea, Box<(String, Option<ObservedRuleArea>)>> {
    if items.len() != 1 {
        return Err(Box::new((
            format!(
                "GetItemsById returned {} item(s) for KIID '{expected_kiid}', expected 1",
                items.len()
            ),
            None,
        )));
    }
    let item = &items[0];
    if !builders::any_is(item, ZONE_TYPE_URL) {
        return Err(Box::new((
            format!(
                "GetItemsById returned unexpected type URL '{}' for KIID '{expected_kiid}'",
                item.type_url
            ),
            None,
        )));
    }
    let zone = Zone::decode(item.value.as_slice()).map_err(|e| {
        Box::new((
            format!("failed to decode readback Zone '{expected_kiid}': {e}"),
            None,
        ))
    })?;
    let observed = decode_observed_rule_area(&zone).map_err(|reason| Box::new((reason, None)))?;

    let mut mismatches = Vec::new();
    if observed.uuid != expected_kiid {
        mismatches.push(format!(
            "uuid '{}' != expected '{expected_kiid}'",
            observed.uuid
        ));
    }
    if observed.name != request.name {
        mismatches.push(format!(
            "name {:?} != expected {:?}",
            observed.name, request.name
        ));
    }
    let expected_layer_set: BTreeSet<i32> = expected_layers.iter().map(|l| *l as i32).collect();
    let observed_layer_set: BTreeSet<i32> = observed.layer_ids.iter().map(|l| *l as i32).collect();
    if observed_layer_set != expected_layer_set || observed.layer_ids.len() != expected_layers.len()
    {
        mismatches.push(format!(
            "layers {:?} != expected {:?}",
            observed.layers,
            expected_layers
                .iter()
                .filter_map(|l| builders::layer_name(*l))
                .collect::<Vec<_>>()
        ));
    }
    if !same_closed_polygon(&request.points_nm, &observed.points_nm) {
        mismatches.push("outline vertices differ from the requested polygon".to_string());
    }
    for (flag, actual, expected) in [
        (
            "keepout_tracks",
            observed.keepout_tracks,
            request.keepout_tracks,
        ),
        ("keepout_vias", observed.keepout_vias, request.keepout_vias),
        ("keepout_pads", observed.keepout_pads, request.keepout_pads),
        (
            "keepout_copper_pours",
            observed.keepout_copper_pours,
            request.keepout_copper_pours,
        ),
        (
            "keepout_footprints",
            observed.keepout_footprints,
            request.keepout_footprints,
        ),
    ] {
        if actual != expected {
            mismatches.push(format!("{flag} was {actual}, expected {expected}"));
        }
    }

    if mismatches.is_empty() {
        Ok(observed)
    } else {
        Err(Box::new((
            format!(
                "KiCad readback did not match the requested rule area: {}",
                mismatches.join("; ")
            ),
            Some(observed),
        )))
    }
}

fn same_closed_polygon(expected: &[(i64, i64)], actual: &[(i64, i64)]) -> bool {
    if expected.len() != actual.len() || expected.is_empty() {
        return false;
    }
    let n = expected.len();
    let matches_forward =
        (0..n).any(|offset| (0..n).all(|i| expected[i] == actual[(i + offset) % n]));
    if matches_forward {
        return true;
    }
    (0..n).any(|offset| (0..n).all(|i| expected[i] == actual[(offset + n - i) % n]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostRefillZones {
    rule_area_present: bool,
    copper_zone_count: usize,
    all_copper_filled: bool,
    zone_snapshot_key: Vec<(String, i32, bool, usize)>,
}

fn inspect_zones_after_refill(
    items: &[prost_types::Any],
    created_uuid: &str,
) -> anyhow::Result<PostRefillZones> {
    let mut rule_area_present = false;
    let mut copper_zone_count = 0usize;
    let mut all_copper_filled = true;
    let mut snapshot_key = Vec::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        anyhow::ensure!(
            builders::any_is(item, ZONE_TYPE_URL),
            "KiCad returned unexpected type URL '{}' for zone inventory item {index}",
            item.type_url
        );
        let zone = Zone::decode(item.value.as_slice())
            .map_err(|e| anyhow::anyhow!("unreadable zone at index {index}: {e}"))?;
        let uuid = zone
            .id
            .as_ref()
            .map(|id| id.value.clone())
            .unwrap_or_default();
        if zone.r#type == ZoneType::ZtRuleArea as i32 && uuid == created_uuid {
            rule_area_present = true;
        }
        if zone.r#type == ZoneType::ZtCopper as i32 {
            copper_zone_count += 1;
            if !zone.filled {
                all_copper_filled = false;
            }
        }
        snapshot_key.push((uuid, zone.r#type, zone.filled, zone.filled_polygons.len()));
    }
    snapshot_key.sort();

    Ok(PostRefillZones {
        rule_area_present,
        copper_zone_count,
        all_copper_filled,
        zone_snapshot_key: snapshot_key,
    })
}

fn read_settled_zones(
    timeout: Duration,
    created_uuid: &str,
    mut read: impl FnMut() -> anyhow::Result<Vec<prost_types::Any>>,
) -> anyhow::Result<PostRefillZones> {
    super::pcb_board_zones::read_settled_by_key(
        timeout,
        || inspect_zones_after_refill(&read()?, created_uuid),
        |zones| zones.zone_snapshot_key.clone(),
    )
}

fn build_applied_outcome(
    observed: ObservedRuleArea,
    refill: anyhow::Result<()>,
    settled: anyhow::Result<PostRefillZones>,
) -> AppliedOutcome {
    let refill_accepted = refill.is_ok();
    let refill_err = refill
        .err()
        .map(|e| format!("applied, but zone refill failed: {e:#}"));
    match settled {
        Ok(after) => {
            let rule_area_still_present = after.rule_area_present;
            let copper_zones_filled = Some(after.all_copper_filled);
            let refill_completed =
                refill_accepted && rule_area_still_present && after.all_copper_filled;
            let diagnostic = refill_err
                .or_else(|| {
                    (!rule_area_still_present).then(|| {
                        format!(
                            "rule area '{}' disappeared during post-refill readback",
                            observed.uuid
                        )
                    })
                })
                .or_else(|| {
                    (!after.all_copper_filled).then(|| {
                        "applied, but KiCad did not report every copper zone as filled after refill"
                            .to_string()
                    })
                });
            AppliedOutcome {
                observed,
                refill_accepted,
                refill_completed,
                board_ready_after_refill: true,
                copper_zone_count: after.copper_zone_count,
                copper_zones_filled,
                rule_area_still_present: Some(rule_area_still_present),
                diagnostic,
            }
        }
        Err(error) => AppliedOutcome {
            observed,
            refill_accepted,
            refill_completed: false,
            board_ready_after_refill: false,
            copper_zone_count: 0,
            copper_zones_filled: None,
            rule_area_still_present: None,
            diagnostic: Some(refill_err.unwrap_or_else(|| {
                format!("applied, but post-refill zone readback failed: {error:#}")
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::handler::McpHandler;
    use crate::test_support::MockIpcServer;
    use crate::tools::pcb_board::board_mock::board_document;
    use crate::tools::ServerConfig;
    use kiapi::common::commands as cmd;
    use kiapi::common::types::{ItemRequestStatus, Kiid};
    use kiapi::common::{ApiRequest, ApiResponse, ApiResponseStatus, ApiStatusCode};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    const REAL_RULE_AREA_IPC: &[u8] =
        include_bytes!("../../tests/fixtures/issue_474_zone_0.ipc.bin");
    const REAL_COPPER_ZONE_IPC: &[u8] =
        include_bytes!("../../tests/fixtures/issue_474_copper_zone_0.ipc.bin");
    const REAL_BOARD: &str =
        include_str!("../../tests/fixtures/six_layer_power_planes_kicad10.kicad_pcb");

    #[derive(Default)]
    struct MockBoardState {
        enabled_copper_layers: Vec<BoardLayer>,
        reject_create: Option<String>,
        alter_readback_tracks: Option<bool>,
        alter_readback_layers: Option<Vec<BoardLayer>>,
        alter_readback_name: bool,
        alter_readback_vertex: bool,
        drop_after_refill: bool,
        fail_readback: bool,
        fail_refill: bool,
        leave_copper_unfilled: bool,
        has_copper_zone: bool,
        committed_zones: Vec<Zone>,
        staged_zones: Vec<Zone>,
        sent_commands: Vec<String>,
    }

    fn ok_resp(msg: Option<prost_types::Any>) -> ApiResponse {
        ApiResponse {
            header: None,
            status: Some(ApiResponseStatus {
                status: ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            message: msg,
        }
    }

    fn err_resp(code: ApiStatusCode, msg: &str) -> ApiResponse {
        ApiResponse {
            header: None,
            status: Some(ApiResponseStatus {
                status: code as i32,
                error_message: msg.to_string(),
            }),
            message: None,
        }
    }

    fn spawn_rule_area_mock(board: &Path, state: Arc<Mutex<MockBoardState>>) -> MockIpcServer {
        let canonical = board.canonicalize().unwrap_or_else(|_| board.to_path_buf());
        let doc = board_document(&canonical.to_string_lossy());
        MockIpcServer::spawn("add_rule_area", move |req: ApiRequest| {
            let msg = req.message.expect("request message");
            let cmd_name = msg
                .type_url
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .to_string();
            let mut st = state.lock().unwrap();
            st.sent_commands.push(cmd_name.clone());

            match cmd_name.as_str() {
                "GetOpenDocuments" => ok_resp(Some(builders::pack_any(
                    &cmd::GetOpenDocumentsResponse {
                        documents: vec![doc.clone()],
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                ))),
                "GetBoardEnabledLayers" => {
                    let layers = st
                        .enabled_copper_layers
                        .iter()
                        .map(|l| *l as i32)
                        .collect::<Vec<_>>();
                    ok_resp(Some(builders::pack_any(
                        &kiapi::board::commands::BoardEnabledLayersResponse {
                            copper_layer_count: layers.len() as u32,
                            layers,
                        },
                        "kiapi.board.commands.BoardEnabledLayersResponse",
                    )))
                }
                "SaveDocumentToString" => {
                    let asked = cmd::SaveDocumentToString::decode(msg.value.as_slice()).unwrap();
                    ok_resp(Some(builders::pack_any(
                        &cmd::SavedDocumentResponse {
                            document: asked.document,
                            contents: "(kicad_pcb (snapshot))".to_string(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    )))
                }
                "BeginCommit" => ok_resp(Some(builders::pack_any(
                    &cmd::BeginCommitResponse {
                        id: Some(Kiid {
                            value: "commit-77".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                ))),
                "EndCommit" => {
                    let end = cmd::EndCommit::decode(msg.value.as_slice()).unwrap();
                    if end.action == cmd::CommitAction::CmaCommit as i32 {
                        st.sent_commands.push("EndCommit:commit".to_string());
                        let staged = std::mem::take(&mut st.staged_zones);
                        st.committed_zones.extend(staged);
                    } else {
                        st.sent_commands.push("EndCommit:drop".to_string());
                        st.staged_zones.clear();
                    }
                    ok_resp(Some(builders::pack_any(
                        &cmd::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    )))
                }
                "CreateItems" => {
                    let create = cmd::CreateItems::decode(msg.value.as_slice()).unwrap();
                    let results = create
                        .items
                        .iter()
                        .map(|any| {
                            if let Some(reason) = &st.reject_create {
                                return cmd::ItemCreationResult {
                                    status: Some(cmd::ItemStatus {
                                        code: cmd::ItemStatusCode::IscInvalidData as i32,
                                        error_message: reason.clone(),
                                    }),
                                    item: None,
                                };
                            }
                            let mut zone = Zone::decode(any.value.as_slice()).unwrap();
                            let id = format!(
                                "11111111-2222-3333-4444-{:012}",
                                st.committed_zones.len() + st.staged_zones.len() + 1
                            );
                            zone.id = Some(Kiid { value: id });
                            let echoed = builders::pack_any(&zone, ZONE_TYPE_URL);
                            st.staged_zones.push(zone);
                            cmd::ItemCreationResult {
                                status: Some(cmd::ItemStatus {
                                    code: cmd::ItemStatusCode::IscOk as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(echoed),
                            }
                        })
                        .collect();
                    ok_resp(Some(builders::pack_any(
                        &cmd::CreateItemsResponse {
                            header: None,
                            status: ItemRequestStatus::IrsOk as i32,
                            created_items: results,
                        },
                        "kiapi.common.commands.CreateItemsResponse",
                    )))
                }
                "GetItemsById" => {
                    if st.fail_readback {
                        return err_resp(ApiStatusCode::AsBadRequest, "simulated readback failure");
                    }
                    let asked = cmd::GetItemsById::decode(msg.value.as_slice()).unwrap();
                    let ids: Vec<String> = asked.items.into_iter().map(|k| k.value).collect();
                    let items = st
                        .committed_zones
                        .iter()
                        .filter(|z| z.id.as_ref().is_some_and(|id| ids.contains(&id.value)))
                        .cloned()
                        .map(|mut zone| {
                            if let Some(tracks) = st.alter_readback_tracks {
                                if let Some(ZoneSettings::RuleAreaSettings(ref mut ras)) =
                                    zone.settings
                                {
                                    ras.keepout_tracks = tracks;
                                }
                            }
                            if let Some(ref layers) = st.alter_readback_layers {
                                zone.layers = layers.iter().map(|l| *l as i32).collect();
                            }
                            if st.alter_readback_name {
                                zone.name = "unexpected name".to_string();
                            }
                            if st.alter_readback_vertex {
                                let node = &mut zone.outline.as_mut().unwrap().polygons[0]
                                    .outline
                                    .as_mut()
                                    .unwrap()
                                    .nodes[0];
                                if let Some(Geometry::Point(ref mut point)) = node.geometry {
                                    point.x_nm += 1_000_000;
                                }
                            }
                            builders::pack_any(&zone, ZONE_TYPE_URL)
                        })
                        .collect();
                    ok_resp(Some(builders::pack_any(
                        &cmd::GetItemsResponse {
                            header: None,
                            status: ItemRequestStatus::IrsOk as i32,
                            items,
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    )))
                }
                "RefillZones" => {
                    if st.drop_after_refill {
                        st.committed_zones.clear();
                        // A different rule area remains: checking only the type,
                        // rather than the created KIID, must not prove presence.
                        st.committed_zones
                            .push(Zone::decode(REAL_RULE_AREA_IPC).unwrap());
                    }
                    if st.fail_refill {
                        err_resp(ApiStatusCode::AsBadRequest, "refill failed")
                    } else {
                        ok_resp(None)
                    }
                }
                "GetItems" => {
                    let mut items: Vec<prost_types::Any> = st
                        .committed_zones
                        .iter()
                        .map(|z| builders::pack_any(z, ZONE_TYPE_URL))
                        .collect();
                    if st.has_copper_zone {
                        let mut copper = Zone::decode(REAL_COPPER_ZONE_IPC).unwrap();
                        copper.filled = !st.leave_copper_unfilled;
                        items.push(builders::pack_any(&copper, ZONE_TYPE_URL));
                    }
                    ok_resp(Some(builders::pack_any(
                        &cmd::GetItemsResponse {
                            header: None,
                            status: ItemRequestStatus::IrsOk as i32,
                            items,
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    )))
                }
                other => err_resp(
                    ApiStatusCode::AsUnhandled,
                    &format!("unexpected mock command: {other}"),
                ),
            }
        })
    }

    async fn served_handler(ipc_address: String) -> McpHandler {
        McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address,
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds")
    }

    async fn call_served(handler: &McpHandler, arguments: Value) -> CallToolResult {
        let resp = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 77,
                "method": "tools/call",
                "params": {
                    "name": "add_rule_area",
                    "arguments": arguments,
                }
            }))
            .await
            .expect("tools/call succeeds");
        assert!(
            resp.error.is_none(),
            "unexpected JSON-RPC error: {:?}",
            resp.error
        );
        serde_json::from_value(resp.result.unwrap()).expect("CallToolResult")
    }

    fn body(result: &CallToolResult) -> Value {
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text response");
        };
        serde_json::from_str(text).expect("JSON tool body")
    }

    fn temp_board() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, REAL_BOARD).unwrap();
        (dir, board)
    }

    #[test]
    fn real_kicad_rule_area_fixture_decodes_layers_points_and_flags() {
        let zone = Zone::decode(REAL_RULE_AREA_IPC).unwrap();
        let observed = decode_observed_rule_area(&zone).expect("decodes real rule area");
        assert_eq!(observed.uuid, "b762a88c-55f2-48e9-8e30-f803555c9eb3");
        assert_eq!(observed.layers, vec!["F.Cu", "B.Cu"]);
        assert_eq!(observed.points_nm.len(), 4);
        assert!(observed.keepout_tracks);
        assert!(observed.keepout_vias);
        assert!(observed.keepout_pads);
        assert!(!observed.keepout_copper_pours);
        assert!(observed.keepout_footprints);
    }

    #[test]
    fn parse_request_validates_layers_geometry_and_flag_types() {
        let valid_rect = json!({
            "board": "b.kicad_pcb",
            "layers": ["F.Cu", "B.Cu"],
            "rect": { "x1": 1.0, "y1": 2.0, "x2": 10.0, "y2": 8.0 }
        });
        assert!(parse_request(&valid_rect).is_ok());

        // All-false keepout flags are valid in KiCad (e.g. insideArea() DRC targets).
        let all_false = json!({
            "board": "b.kicad_pcb",
            "layers": ["*.Cu"],
            "rect": { "x1": 0, "y1": 0, "x2": 5, "y2": 5 },
            "keepout_tracks": false,
            "keepout_vias": false,
            "keepout_pads": false,
            "keepout_copper_pours": false,
            "keepout_footprints": false
        });
        let req = parse_request(&all_false).unwrap();
        assert!(req.layers.wildcard);
        assert!(!req.keepout_tracks && !req.keepout_vias && !req.keepout_pads);

        // Closed 5-point rect polygon trims duplicate closing vertex cleanly.
        let closed_poly = json!({
            "board": "b.kicad_pcb",
            "layers": ["F.Cu"],
            "points": [
                { "x": 0.0, "y": 0.0 },
                { "x": 5.0, "y": 0.0 },
                { "x": 5.0, "y": 5.0 },
                { "x": 0.0, "y": 5.0 },
                { "x": 0.0, "y": 0.0 }
            ]
        });
        assert_eq!(parse_request(&closed_poly).unwrap().points_nm.len(), 4);

        let invalid_cases = [
            (
                "empty layers",
                json!({ "layers": [], "rect": { "x1": 0, "y1": 0, "x2": 1, "y2": 1 } }),
            ),
            (
                "non-copper layer",
                json!({ "layers": ["F.SilkS"], "rect": { "x1": 0, "y1": 0, "x2": 1, "y2": 1 } }),
            ),
            (
                "malformed inner layer",
                json!({ "layers": ["In01.Cu"], "rect": { "x1": 0, "y1": 0, "x2": 1, "y2": 1 } }),
            ),
            (
                "unknown layer",
                json!({ "layers": ["Foo.Cu"], "rect": { "x1": 0, "y1": 0, "x2": 1, "y2": 1 } }),
            ),
            (
                "both points and rect",
                json!({
                    "layers": ["F.Cu"],
                    "rect": { "x1": 0, "y1": 0, "x2": 1, "y2": 1 },
                    "points": [{ "x": 0, "y": 0 }, { "x": 1, "y": 0 }, { "x": 1, "y": 1 }]
                }),
            ),
            ("neither points nor rect", json!({ "layers": ["F.Cu"] })),
            (
                "fewer than 3 points",
                json!({ "layers": ["F.Cu"], "points": [{ "x": 0, "y": 0 }, { "x": 1, "y": 0 }] }),
            ),
            (
                "collinear zero-area polygon",
                json!({
                    "layers": ["F.Cu"],
                    "points": [{ "x": 0, "y": 0 }, { "x": 1, "y": 0 }, { "x": 2, "y": 0 }]
                }),
            ),
            (
                "zero-width rect",
                json!({ "layers": ["F.Cu"], "rect": { "x1": 2.0, "y1": 0.0, "x2": 2.0, "y2": 5.0 } }),
            ),
            (
                "out-of-range coordinate",
                json!({ "layers": ["F.Cu"], "rect": { "x1": 0.0, "y1": 0.0, "x2": 3000.0, "y2": 5.0 } }),
            ),
            (
                "wrong-typed keepout flag",
                json!({
                    "layers": ["F.Cu"],
                    "rect": { "x1": 0, "y1": 0, "x2": 5, "y2": 5 },
                    "keepout_tracks": "yes"
                }),
            ),
        ];
        for (label, case) in invalid_cases {
            let err = parse_request(&case).expect_err(label);
            assert_eq!(
                extract_error_kind(&err).as_deref(),
                Some("invalid_argument"),
                "{label}"
            );
        }
    }

    #[test]
    fn catalogue_discloses_live_only_and_freerouting_limitation() {
        let def = tool();
        assert_eq!(def.name, "add_rule_area");
        assert_eq!(def.board_access, BoardAccess::LiveOnly);
        assert!(def.description.contains("export_specctra_dsn"));
        assert!(def.description.contains("#81"));
        assert!(def
            .description
            .contains("refuses without modifying the file"));
    }

    #[tokio::test]
    async fn served_no_live_kicad_refuses_without_touching_board_file() {
        let (dir, board) = temp_board();
        let before = std::fs::read(&board).unwrap();
        let handler = served_handler(format!(
            "ipc://{}",
            dir.path().join("missing-kicad.sock").display()
        ))
        .await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "B.Cu"],
                "rect": { "x1": 10.0, "y1": 10.0, "x2": 25.0, "y2": 20.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("editor_unavailable")
        );
        assert_eq!(body(&res)["outcome"]["status"], "failed");
        assert_eq!(std::fs::read(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn served_wrong_open_board_refuses_without_creating_anything() {
        let (_dir, board) = temp_board();
        let other_dir = tempfile::tempdir().unwrap();
        let other_board = other_dir.path().join("other.kicad_pcb");
        std::fs::write(&other_board, REAL_BOARD).unwrap();

        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&other_board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu"],
                "rect": { "x1": 0.0, "y1": 0.0, "x2": 5.0, "y2": 5.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(extract_error_kind(&res).as_deref(), Some("wrong_document"));
        assert_eq!(body(&res)["outcome"]["status"], "failed");
        assert!(state.lock().unwrap().committed_zones.is_empty());
        assert!(!state
            .lock()
            .unwrap()
            .sent_commands
            .iter()
            .any(|c| c == "CreateItems"));
    }

    #[tokio::test]
    async fn served_disabled_copper_layer_refuses_before_commit() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "In1.Cu"],
                "rect": { "x1": 0.0, "y1": 0.0, "x2": 10.0, "y2": 10.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("invalid_argument")
        );
        let json = body(&res);
        assert_eq!(json["error"]["field"], "layers");
        assert!(json["error"]["reason"].as_str().unwrap().contains("In1.Cu"));
        assert_eq!(json["outcome"]["status"], "failed");
        assert!(!state
            .lock()
            .unwrap()
            .sent_commands
            .iter()
            .any(|c| c == "BeginCommit" || c == "CreateItems"));
    }

    #[tokio::test]
    async fn served_wildcard_antenna_keepout_succeeds_and_reads_back_with_refill() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![
                BoardLayer::BlFCu,
                BoardLayer::BlIn1Cu,
                BoardLayer::BlIn2Cu,
                BoardLayer::BlBCu,
            ],
            has_copper_zone: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["*.Cu"],
                "rect": { "x1": 40.0, "y1": 0.0, "x2": 58.0, "y2": 12.0 },
                "name": "antenna_keepout",
                "keepout_copper_pours": true
            }),
        )
        .await;
        assert!(!res.is_error, "{}", body(&res));
        let json = body(&res);
        assert_eq!(json["success"], true);
        assert_eq!(json["applied"], true);
        assert_eq!(json["applied_but_not_refilled"], false);
        assert_eq!(json["readback_verified"], true);
        assert_eq!(json["refill_request_accepted"], true);
        assert_eq!(json["refill_completed"], true);
        assert_eq!(json["copper_zone_count"], 1);
        assert_eq!(json["copper_zones_filled"], true);
        assert_eq!(json["outcome"]["status"], "complete");
        assert_eq!(json["source"], "ipc_readback");
        assert_eq!(json["rule_area"]["name"], "antenna_keepout");
        assert_eq!(
            json["rule_area"]["layers"],
            json!(["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"])
        );
        assert_eq!(json["rule_area"]["keepout_tracks"], true);
        assert_eq!(json["rule_area"]["keepout_vias"], true);
        assert_eq!(json["rule_area"]["keepout_pads"], true);
        assert_eq!(json["rule_area"]["keepout_copper_pours"], true);
        assert_eq!(json["rule_area"]["keepout_footprints"], false);
        assert!(json["warning"].as_str().unwrap().contains("#81"));
        assert!(state
            .lock()
            .unwrap()
            .sent_commands
            .iter()
            .any(|c| c == "RefillZones"));
    }

    #[tokio::test]
    async fn served_create_refusal_drops_commit_and_leaves_no_zone() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            reject_create: Some("invalid polygon".to_string()),
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu"],
                "points": [{ "x": 0.0, "y": 0.0 }, { "x": 5.0, "y": 0.0 }, { "x": 5.0, "y": 5.0 }]
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(body(&res)["outcome"]["status"], "failed");
        assert!(format!("{res:?}").contains("invalid polygon"));
        let st = state.lock().unwrap();
        assert!(st.committed_zones.is_empty());
        assert!(st.sent_commands.iter().any(|c| c == "EndCommit:drop"));
        assert!(!st.sent_commands.iter().any(|c| c == "EndCommit:commit"));
    }

    #[tokio::test]
    async fn served_flag_mismatch_reports_uncertain_with_created_uuid() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            alter_readback_tracks: Some(false),
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "B.Cu"],
                "rect": { "x1": 1.0, "y1": 1.0, "x2": 6.0, "y2": 6.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert!(json["uuid"].as_str().is_some());
        assert!(
            json["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("keepout_tracks"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn served_layers_mismatch_reports_uncertain_with_created_uuid() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            alter_readback_layers: Some(vec![BoardLayer::BlFCu]),
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "B.Cu"],
                "rect": { "x1": 1.0, "y1": 1.0, "x2": 6.0, "y2": 6.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert!(json["uuid"].as_str().is_some());
        assert!(
            json["error"]["reason"].as_str().unwrap().contains("layers"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn served_name_mismatch_reports_uncertain_with_created_uuid() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            alter_readback_name: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "B.Cu"],
                "rect": { "x1": 1.0, "y1": 1.0, "x2": 6.0, "y2": 6.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert!(json["uuid"].as_str().is_some());
        assert!(
            json["error"]["reason"].as_str().unwrap().contains("name"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn served_vertex_mismatch_reports_uncertain_with_created_uuid() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            alter_readback_vertex: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu", "B.Cu"],
                "rect": { "x1": 1.0, "y1": 1.0, "x2": 6.0, "y2": 6.0 }
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert!(json["uuid"].as_str().is_some());
        assert!(
            json["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("outline"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn served_refill_failure_reports_applied_but_not_refilled_partial_outcome() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            fail_refill: true,
            has_copper_zone: true,
            leave_copper_unfilled: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu"],
                "rect": { "x1": 2.0, "y1": 2.0, "x2": 8.0, "y2": 8.0 },
                "keepout_copper_pours": true
            }),
        )
        .await;
        assert!(!res.is_error);
        let json = body(&res);
        assert_eq!(json["success"], false);
        assert_eq!(json["applied"], true);
        assert_eq!(json["applied_but_not_refilled"], true);
        assert_eq!(json["readback_verified"], true);
        assert_eq!(json["refill_request_accepted"], false);
        assert_eq!(json["refill_completed"], false);
        assert_eq!(json["copper_zones_filled"], false);
        assert_eq!(json["outcome"]["status"], "partial");
        assert!(json["diagnostic"]
            .as_str()
            .unwrap()
            .contains("applied, but zone refill failed"));
    }

    #[tokio::test]
    async fn served_accepted_refill_with_unfilled_copper_reports_partial() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            fail_refill: false,
            has_copper_zone: true,
            leave_copper_unfilled: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;

        let res = call_served(
            &handler,
            json!({
                "board": board,
                "layers": ["F.Cu"],
                "rect": { "x1": 2.0, "y1": 2.0, "x2": 8.0, "y2": 8.0 },
                "keepout_copper_pours": true
            }),
        )
        .await;
        assert!(!res.is_error);
        let json = body(&res);
        assert_eq!(json["success"], false);
        assert_eq!(json["applied"], true);
        assert_eq!(json["applied_but_not_refilled"], true);
        assert_eq!(json["readback_verified"], true);
        assert_eq!(json["refill_request_accepted"], true);
        assert_eq!(json["refill_completed"], false);
        assert_eq!(json["copper_zones_filled"], false);
        assert_eq!(json["outcome"]["status"], "partial");
    }

    #[tokio::test]
    async fn served_rule_area_missing_after_refill_is_uncertain() {
        let (_dir, board) = temp_board();
        let state = Arc::new(Mutex::new(MockBoardState {
            enabled_copper_layers: vec![BoardLayer::BlFCu, BoardLayer::BlBCu],
            drop_after_refill: true,
            ..Default::default()
        }));
        let server = spawn_rule_area_mock(&board, state.clone());
        let handler = served_handler(server.address().to_string()).await;
        let res = call_served(
            &handler,
            json!({
                "board": board, "layers": ["F.Cu"],
                "rect": {"x1": 1, "y1": 1, "x2": 6, "y2": 6}
            }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert!(json["uuid"].as_str().is_some());
        assert!(state
            .lock()
            .unwrap()
            .sent_commands
            .iter()
            .any(|c| c == "RefillZones"));
    }

    #[test]
    fn missing_rule_area_after_refill_is_uncertain_not_partial() {
        let zone = Zone::decode(REAL_RULE_AREA_IPC).unwrap();
        let observed = decode_observed_rule_area(&zone).unwrap();
        let applied = build_applied_outcome(
            observed,
            Ok(()),
            Ok(PostRefillZones {
                rule_area_present: false,
                copper_zone_count: 0,
                all_copper_filled: true,
                zone_snapshot_key: vec![],
            }),
        );
        let res = applied_result(Path::new("board.kicad_pcb"), applied);
        assert!(res.is_error);
        assert_eq!(
            extract_error_kind(&res).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        let json = body(&res);
        assert_eq!(json["outcome"]["status"], "uncertain");
        assert_eq!(json["uuid"], "b762a88c-55f2-48e9-8e30-f803555c9eb3");
    }
}
