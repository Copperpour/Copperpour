//! Revision-bound moves of existing vias, preserving KiCad's native messages (#651).

use crate::hex::encode_hex;
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::outcome::{self, OutcomeStatus};
use crate::tool;
use crate::tools::{
    get_path, invalid_arg, ipc_failure_result, opt_bool_flag, require_array, require_f64,
    require_str, with_bound_board_ipc_classified, BoardAccess, BoardBinding, ToolContext, ToolDef,
};
use copperpour_ipc::builders::{any_is, mm_to_nm, nm_to_mm, pack_any};
use copperpour_ipc::client::{IpcBatchRecovered, IpcOutcomeUnknown, KiCadIpcClient};
use copperpour_ipc::gen::kiapi::{
    board::types::Via,
    common::types::{DocumentSpecifier, LockedState, Vector2},
};
use prost::Message;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const VIA: &str = "kiapi.board.types.Via";
const UNDO: &str = "Set via positions";
const LIMIT: i64 = super::pcb_board_outline::COORDINATE_LIMIT_NM;

type Refusal<T> = Result<T, CallToolResult>;

pub(crate) fn tool() -> ToolDef {
    tool!(
        "set_via_positions",
        "Move existing vias in one live KiCad commit and undo step, preserving UUIDs and all \
         native properties. Give placements [{uuid,x,y}] OR uuids plus offset {x,y}, in mm. \
         Dry run by default; apply requires its expected_plan_revision. Rejects locked, missing, \
         duplicate and non-via targets before mutation. Reads every via back after commit; if \
         KiCad changes a net, restores the whole batch in a second commit and verifies it. \
         Unconfirmed writes or restoration report mutation_outcome_uncertain with target UUIDs; \
         one undo step is not a guarantee of atomicity. No save, zone refill or DRC. Connected \
         tracks are not moved or stretched; refill zones and validate routing afterward.",
        json!({
            "type": "object",
            "properties": {
                "board": {"type": "string"},
                "placements": {"type": "array", "minItems": 1, "items": {
                    "type": "object", "properties": {
                        "uuid": {"type": "string", "minLength": 1},
                        "x": {"type": "number", "description": "Absolute X in mm"},
                        "y": {"type": "number", "description": "Absolute Y in mm"}
                    }, "required": ["uuid", "x", "y"]
                }},
                "uuids": {"type": "array", "minItems": 1,
                    "items": {"type": "string", "minLength": 1}},
                "offset": {"type": "object", "properties": {
                    "x": {"type": "number", "description": "X offset in mm"},
                    "y": {"type": "number", "description": "Y offset in mm"}
                }, "required": ["x", "y"]},
                "dry_run": {"type": "boolean", "default": true},
                "expected_plan_revision": {"type": "string"}
            },
            "required": ["board"],
            "oneOf": [
                {"required": ["placements"], "not": {"anyOf": [
                    {"required": ["uuids"]}, {"required": ["offset"]}
                ]}},
                {"required": ["uuids", "offset"], "not": {"required": ["placements"]}}
            ]
        }),
        |args, ctx| async move { handle_set_via_positions(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

#[derive(Debug)]
struct Request {
    /// BTreeMap gives both duplicate detection and stable plan ordering.
    targets: BTreeMap<String, Vector2>,
    relative: bool,
}

fn coordinate(args: &Value, field: &str) -> Refusal<i64> {
    let mm = require_f64(args, field)?;
    if !mm.is_finite() || (mm * 1e6).round().abs() > LIMIT as f64 {
        return Err(invalid_arg(
            field,
            &format!(
                "must be a finite board coordinate within ±{} mm",
                nm_to_mm(LIMIT)
            ),
        ));
    }
    Ok(mm_to_nm(mm))
}

fn point(args: &Value) -> Refusal<Vector2> {
    Ok(Vector2 {
        x_nm: coordinate(args, "x")?,
        y_nm: coordinate(args, "y")?,
    })
}

fn parse(args: &Value) -> Refusal<Request> {
    let mut targets = BTreeMap::new();
    let relative = args.get("placements").is_none();
    if !relative && (args.get("uuids").is_some() || args.get("offset").is_some()) {
        return Err(invalid_arg(
            "placements",
            "give placements OR uuids with offset, never both",
        ));
    }
    let (items, offset) = if relative {
        (require_array(args, "uuids")?, Some(point(&args["offset"])?))
    } else {
        (require_array(args, "placements")?, None)
    };
    if items.is_empty() {
        return Err(invalid_arg(
            if relative { "uuids" } else { "placements" },
            "must not be empty",
        ));
    }
    for (index, item) in items.iter().enumerate() {
        let (id, position) = if let Some(offset) = offset {
            let id = item
                .as_str()
                .ok_or_else(|| invalid_arg(&format!("uuids[{index}]"), "must be a UUID string"))?;
            (id, offset)
        } else {
            (require_str(item, "uuid")?, point(item)?)
        };
        if id.trim().is_empty() || targets.insert(id.to_string(), position).is_some() {
            return Err(invalid_arg("uuid", "UUIDs must be nonempty and unique"));
        }
    }
    Ok(Request { targets, relative })
}

fn blocked(reasons: Vec<String>) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: "set_via_positions".into(),
            reasons: reasons.clone(),
        },
        format!(
            "set_via_positions refused: {}. No requested move remains applied.",
            reasons.join("; ")
        ),
    )
}

fn stale(path: &Path) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: path.display().to_string(),
            reason: "board or via move plan changed since the dry run".into(),
        },
        "The plan is stale. Nothing was written; dry-run again against the live board.",
    )
}

fn uncertain(path: &Path, ids: &[String], reason: String) -> CallToolResult {
    let result = CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: "set_via_positions".into(),
            path: path.display().to_string(),
            reason: reason.clone(),
        },
        format!(
            "{reason}. Inspect these via UUIDs before retrying: {}",
            ids.join(", ")
        ),
    );
    let mut body: Value = serde_json::from_str(match &result.content[0] {
        crate::mcp::protocol::ToolContent::Text { text } => text,
        _ => unreachable!(),
    })
    .expect("structured error JSON");
    body["uuids"] = json!(ids);
    {
        let mut result = CallToolResult::json(&body);
        result.is_error = true;
        result
    }
}

/// Use the shared live-board serializer; this is never a saved-file fallback.
fn snapshot(client: &KiCadIpcClient, document: &DocumentSpecifier) -> anyhow::Result<String> {
    client.save_document_to_string_in(document.clone())
}

/// Every requested ID must occur exactly once, with the declared via type.
fn read_vias(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    ids: &[String],
) -> anyhow::Result<BTreeMap<String, Via>> {
    let mut vias = BTreeMap::new();
    let wanted: BTreeSet<_> = ids.iter().map(String::as_str).collect();
    for any in client.get_items_by_id_in(document.clone(), ids)? {
        anyhow::ensure!(any_is(&any, VIA), "a requested item is not a via");
        let via = Via::decode(any.value.as_slice())?;
        let id = via
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("via has no UUID"))?
            .value
            .clone();
        anyhow::ensure!(
            wanted.contains(id.as_str()),
            "unrequested via '{id}' in readback"
        );
        anyhow::ensure!(
            vias.insert(id, via).is_none(),
            "duplicate via UUID in readback"
        );
    }
    anyhow::ensure!(
        vias.len() == wanted.len(),
        "one or more requested via UUIDs are missing"
    );
    Ok(vias)
}

fn plan(request: &Request, before: &BTreeMap<String, Via>) -> Refusal<BTreeMap<String, Via>> {
    let mut after = before.clone();
    for (id, via) in &mut after {
        if via.locked != LockedState::LsUnlocked as i32 {
            return Err(blocked(vec![format!(
                "via '{id}' is locked or its lock state is unknown"
            )]));
        }
        let old = via
            .position
            .ok_or_else(|| blocked(vec![format!("via '{id}' has no position")]))?;
        let target = request.targets[id];
        let new = if request.relative {
            Vector2 {
                x_nm: old
                    .x_nm
                    .checked_add(target.x_nm)
                    .ok_or_else(|| invalid_arg("offset.x", "coordinate overflow"))?,
                y_nm: old
                    .y_nm
                    .checked_add(target.y_nm)
                    .ok_or_else(|| invalid_arg("offset.y", "coordinate overflow"))?,
            }
        } else {
            target
        };
        if new.x_nm.unsigned_abs() > LIMIT as u64 || new.y_nm.unsigned_abs() > LIMIT as u64 {
            return Err(invalid_arg(
                "offset",
                "destination is outside KiCad's board coordinate range",
            ));
        }
        via.position = Some(new);
    }
    Ok(after)
}

fn revision(
    document: &DocumentSpecifier,
    contents: &str,
    before: &BTreeMap<String, Via>,
    after: &BTreeMap<String, Via>,
) -> String {
    let mut hash = Sha256::new();
    for bytes in std::iter::once(b"set_via_positions/1".to_vec())
        .chain([document.encode_to_vec(), contents.as_bytes().to_vec()])
        .chain(
            before
                .values()
                .chain(after.values())
                .map(Message::encode_to_vec),
        )
    {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("sha256:{}", encode_hex(&hash.finalize()))
}

fn rows(vias: &BTreeMap<String, Via>) -> Vec<Value> {
    vias.iter()
        .map(|(id, via)| {
            let p = via.position.expect("validated via position");
            json!({"uuid": id, "x": nm_to_mm(p.x_nm), "y": nm_to_mm(p.y_nm)})
        })
        .collect()
}

fn net_name(via: &Via) -> &str {
    via.net
        .as_ref()
        .map(|net| net.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("no net")
}

fn packed(vias: &BTreeMap<String, Via>) -> Vec<prost_types::Any> {
    vias.values().map(|via| pack_any(via, VIA)).collect()
}

async fn handle_request(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "board")?;
    let request = match parse(args) {
        Ok(value) => value,
        Err(result) => return Ok(result),
    };
    let dry = match opt_bool_flag(args, "dry_run") {
        Ok(value) => value.unwrap_or(true),
        Err(result) => return Ok(result),
    };
    let expected = match args.get("expected_plan_revision") {
        None if dry => None,
        None => {
            return Ok(invalid_arg(
                "expected_plan_revision",
                "required when dry_run is false",
            ))
        }
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => {
            return Ok(invalid_arg(
                "expected_plan_revision",
                "must be a nonempty string",
            ))
        }
    };
    let target = path.clone();
    let result = with_bound_board_ipc_classified(ctx, &path, move |client, document| {
        apply(
            client,
            &document,
            &target,
            &request,
            dry,
            expected.as_deref(),
        )
    })
    .await?;
    let result = match result {
        Ok(BoardBinding::Bound(result)) => result,
        Ok(BoardBinding::Unserved { absence, message }) => {
            crate::tools::ipc_unserved_result(absence, &message)
        }
        Err(failure) => ipc_failure_result(failure),
    };
    Ok(result)
}

async fn handle_set_via_positions(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "board")?;
    let count = args
        .get("placements")
        .or_else(|| args.get("uuids"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let result = handle_request(args, ctx).await?;
    Ok(envelope(result, &path, count))
}

fn envelope(result: CallToolResult, path: &Path, count: usize) -> CallToolResult {
    let kind = crate::mcp::error::extract_error_kind(&result);
    let uncertain = matches!(
        kind.as_deref(),
        Some("mutation_outcome_uncertain" | "ipc_outcome_unknown")
    );
    let inspect = uncertain || kind.as_deref() == Some("ipc_batch_recovered");
    let status = if uncertain {
        OutcomeStatus::Uncertain
    } else if result.is_error {
        OutcomeStatus::Failed
    } else {
        OutcomeStatus::Complete
    };
    outcome::attach(
        result,
        outcome::summary(
            status,
            path.display().to_string(),
            "live_editor",
            count,
            if status == OutcomeStatus::Complete {
                count
            } else {
                0
            },
            if status == OutcomeStatus::Complete {
                0
            } else {
                count
            },
            if inspect {
                Some(outcome::inspect_before_retry())
            } else if status == OutcomeStatus::Failed {
                Some(outcome::retry_whole_request())
            } else {
                None
            },
        ),
    )
}

fn apply(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    path: &Path,
    request: &Request,
    dry: bool,
    expected: Option<&str>,
) -> anyhow::Result<CallToolResult> {
    let ids: Vec<_> = request.targets.keys().cloned().collect();
    let contents = snapshot(client, document)?;
    let before = match read_vias(client, document, &ids) {
        Ok(vias) => vias,
        Err(error) => {
            return Ok(blocked(vec![format!(
                "cannot resolve all requested vias: {error:#}"
            )]))
        }
    };
    let after = match plan(request, &before) {
        Ok(value) => value,
        Err(result) => return Ok(result),
    };
    let rev = revision(document, &contents, &before, &after);
    if dry {
        return Ok(CallToolResult::json(
            &json!({"dry_run": true, "plan_revision": rev,
            "via_count": after.len(), "vias": rows(&after), "source": "ipc_plan"}),
        ));
    }
    if expected != Some(rev.as_str()) {
        return Ok(stale(path));
    }
    if before == after {
        return Ok(CallToolResult::json(
            &json!({"dry_run": false, "unchanged": true,
            "via_count": before.len(), "vias": rows(&before), "source": "ipc_readback"}),
        ));
    }
    let mut stale_inside = false;
    let mut update_attempted = false;
    let committed = client.run_commit_recovering_in(document.clone(), UNDO, |scoped| {
        // Recheck after BeginCommit, before any update; both board copper and target properties matter.
        if snapshot(scoped, document)? != contents || read_vias(scoped, document, &ids)? != before {
            stale_inside = true;
            anyhow::bail!("live board changed before the batch update");
        }
        update_attempted = true;
        scoped.update_items_in(document.clone(), packed(&after))
    });
    if let Err(error) = committed {
        if error.downcast_ref::<IpcBatchRecovered>().is_some() {
            return Ok(crate::tools::ipc_recovered_result(&format!("{error:#}")));
        }
        if error.downcast_ref::<IpcOutcomeUnknown>().is_some() {
            return Ok(uncertain(
                path,
                &ids,
                format!("batch outcome cannot be confirmed: {error:#}"),
            ));
        }
        if stale_inside {
            // The closure sent no UpdateItems and the empty commit was dropped.
            // External target edits are not writes made by this call.
            return Ok(stale(path));
        }
        if !update_attempted {
            return Ok(blocked(vec![format!(
                "pre-update check failed; no update was sent: {error:#}"
            )]));
        }
        return Ok(match read_vias(client, document, &ids) {
            Ok(observed) if observed == before => blocked(vec![format!(
                "commit failed; original vias verified: {error:#}"
            )]),
            _ => uncertain(
                path,
                &ids,
                format!("commit failed and original vias could not be verified: {error:#}"),
            ),
        });
    }
    let observed = match read_vias(client, document, &ids) {
        Ok(value) => value,
        Err(error) => {
            return Ok(uncertain(
                path,
                &ids,
                format!("committed vias cannot be read back: {error:#}"),
            ))
        }
    };
    if observed == after {
        return Ok(CallToolResult::json(
            &json!({"dry_run": false, "plan_revision": rev,
            "via_count": observed.len(), "vias": rows(&observed), "source": "ipc_readback",
            "undo": "One KiCad undo step; board not saved", "refill_required_before_drc": true}),
        ));
    }
    let net_changed = observed.iter().any(|(id, via)| via.net != before[id].net);
    let mut without_net_changes = observed.clone();
    for (id, via) in &mut without_net_changes {
        via.net = before[id].net.clone();
    }
    if !net_changed || without_net_changes != after {
        return Ok(uncertain(
            path,
            &ids,
            "readback differs in position or a preserved native property".into(),
        ));
    }
    let reasons: Vec<_> = observed
        .iter()
        .filter(|(id, via)| via.net != before[*id].net)
        .map(|(id, via)| {
            format!(
                "via '{id}' changed net from '{}' to '{}'",
                net_name(&before[id]),
                net_name(via)
            )
        })
        .collect();
    // Only a proven net-only mismatch triggers restoration. No blind rollback after uncertainty.
    let restoration = client.run_commit_recovering_in(
        document.clone(),
        "Restore via positions after net change",
        |scoped| {
            anyhow::ensure!(
                read_vias(scoped, document, &ids)? == observed,
                "vias changed before restoration"
            );
            scoped.update_items_in(document.clone(), packed(&before))
        },
    );
    match restoration {
        Ok(()) => match read_vias(client, document, &ids) {
            Ok(value) if value == before => Ok(blocked(reasons)),
            _ => Ok(uncertain(
                path,
                &ids,
                format!(
                    "net changed; restoration not verified: {}",
                    reasons.join("; ")
                ),
            )),
        },
        Err(error) => Ok(uncertain(
            path,
            &ids,
            format!(
                "net changed; restoration failed: {error:#}; {}",
                reasons.join("; ")
            ),
        )),
    }
}

#[cfg(test)]
#[path = "pcb_via_positions_tests.rs"]
mod tests;
