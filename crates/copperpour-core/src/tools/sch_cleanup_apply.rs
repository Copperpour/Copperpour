//! Commit the cleanup planner's final candidate, never an intermediate step.

use super::{
    cleanup_schema, cleanup_selection, plan_with_candidate, sch_layout_score, DocumentRevision,
    RouteGuard, Sheet, MAX_CLEANUP_STEPS,
};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use crate::tools::{get_path, require_str, sch_block_move::misplaced_item_in, ToolDef};
use copperpour_sexp::{
    parse_sexp, schematic::parse_at, writer::write_atomic_if_unchanged, SexpError, SexpNode,
};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(test)]
#[path = "sch_cleanup_apply_tests.rs"]
mod tests;

const TOOL: &str = "apply_schematic_cleanup";

pub(in crate::tools) fn definition() -> ToolDef {
    let mut schema = cleanup_schema();
    schema["properties"]["revision"] = json!({
        "type": "string", "description": "The revision returned by plan_schematic_cleanup"
    });
    schema["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("revision"));
    tool!(
        "apply_schematic_cleanup",
        "Apply a previewed schematic cleanup in one revision-checked write. Pass the \
         same arguments as plan_schematic_cleanup plus its revision, not its steps. \
         An empty plan does not write. The tool refuses detected file changes, \
         changes to pin nets, and foreign joins. Use a regular file, not a symbolic \
         link. Close the sheet in KiCad and stop other writers before applying; \
         a writer that ignores Copperpour's lock can still race the final rename.",
        schema,
        |args: &Value, _ctx| {
            let args = args.clone();
            async move {
                tokio::task::spawn_blocking(move || handle_apply_schematic_cleanup(&args)).await?
            }
        }
    )
}

fn handle_apply_schematic_cleanup(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let expected_revision = match require_str(args, "revision") {
        Ok(revision) => revision,
        Err(refusal) => return Ok(refusal),
    };
    let (region, movable) = match cleanup_selection(args) {
        Ok(selection) => selection,
        Err(refusal) => return Ok(refusal),
    };
    match std::fs::symlink_metadata(&path) {
        Ok(entry) if entry.is_file() => (),
        Ok(_) => {
            return Ok(crate::tools::invalid_arg(
                "schematic",
                "use a regular schematic file, not a symbolic link or another file type",
            ));
        }
        Err(error) => return Ok(sch_layout_score::unreadable_sheet(&path, error)),
    }
    let bytes = match sch_layout_score::read_saved_bytes(&path) {
        Ok(bytes) => bytes,
        Err(refusal) => return Ok(refusal),
    };
    let revision = DocumentRevision::of_bytes(&bytes).to_string();
    if revision != expected_revision {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::Conflict {
                paths: vec![path.display().to_string()],
            },
            format!(
                "Schematic '{}' changed since the preview. Nothing was written. Run \
                 plan_schematic_cleanup again and use its revision.",
                path.display()
            ),
        ));
    }
    let source = match sch_layout_score::decode_saved_source(&path, bytes) {
        Ok(source) => source,
        Err(refusal) => return Ok(refusal),
    };
    let before = match sch_layout_score::parse_saved_sheet(&path, &source) {
        Ok(tree) => tree,
        Err(refusal) => return Ok(refusal),
    };
    let sheet = match Sheet::for_cleanup(&path, &before) {
        Ok(sheet) => sheet,
        Err(refusal) => return Ok(refusal),
    };

    #[cfg(test)]
    let fault = tests::take_fault(&path);
    let limit = MAX_CLEANUP_STEPS;
    #[cfg(test)]
    let limit = fault.step_limit(limit);
    let mut reasons = Vec::new();
    let Some((steps, skipped, candidate)) =
        plan_with_candidate(&source, &sheet, region, &movable, limit, |step, guard| {
            #[cfg(test)]
            let injected = fault.route_candidate(step);
            #[cfg(test)]
            let step = injected.as_ref();
            reasons.extend(route_recheck(step, guard));
        })?
    else {
        return Ok(plan_blocked(vec![format!(
            "the region still has cleanup steps after {limit} edits; select a smaller region"
        )]));
    };
    if steps.is_empty() {
        return Ok(reply(&path, &revision, steps, skipped, true));
    }

    #[cfg(test)]
    let candidate = fault.candidate(candidate);
    let expected = parse_sexp(&candidate)?;
    let removed_power = steps
        .iter()
        .flat_map(|step| {
            step["power_symbols_removed"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .map(|id| {
            id.as_str()
                .expect("planner records power symbol UUIDs")
                .to_owned()
        })
        .collect::<Vec<_>>();
    // The cleanup variant of the pin-net gate preserves names, scopes and
    // peers while allowing only the redundant power pins named by the plan
    // to disappear. The route guard checks each route in its own sheet state.
    reasons.extend(sch_layout_score::cleanup_gate(
        &before,
        &expected,
        &removed_power,
    ));
    if !reasons.is_empty() {
        return Ok(plan_blocked(reasons));
    }

    let to_write = candidate.as_str();
    #[cfg(test)]
    let injected = fault.write_candidate(to_write);
    #[cfg(test)]
    let to_write = injected.as_ref();
    write_atomic_if_unchanged(&path, &source, to_write)?;

    let mismatch = || SexpError::ReadbackMismatch { path: path.clone() };
    let written = std::fs::read_to_string(&path).map_err(|_| mismatch())?;
    let revision = verified_revision(&before, &expected, &removed_power, to_write, &written)
        .map_err(|reason| {
            tracing::warn!(file = %path.display(), %reason, "schematic cleanup readback mismatch");
            mismatch()
        })?;
    Ok(reply(&path, &revision, steps, skipped, false))
}

fn reply(
    path: &Path,
    revision: &str,
    steps: Vec<Value>,
    skipped: Vec<Value>,
    unchanged: bool,
) -> CallToolResult {
    CallToolResult::json(&json!({
        "schematic": path, "source": "saved_schematic", "revision": revision,
        "steps": steps, "skipped": skipped, "unchanged": unchanged
    }))
}

fn plan_blocked(reasons: Vec<String>) -> CallToolResult {
    let message = format!(
        "Cleanup plan blocked. Nothing was written: {}",
        reasons.join("; ")
    );
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: TOOL.to_owned(),
            reasons,
        },
        message,
    )
}

fn route_recheck(source: &str, guard: &RouteGuard) -> Vec<String> {
    match parse_sexp(source) {
        Ok(tree) => guard.foreign_joins(&tree),
        Err(error) => vec![format!("cannot check route joins: {error}")],
    }
}

fn verified_revision(
    before: &SexpNode,
    expected: &SexpNode,
    removed_power: &[String],
    to_write: &str,
    written: &str,
) -> Result<String, String> {
    let observed = parse_sexp(written).map_err(|error| error.to_string())?;
    verify_readback(before, expected, &observed, removed_power)?;
    // Do not certify a later save, even if it preserved the checked items.
    if written != to_write {
        return Err("a later save changed the written bytes".into());
    }
    Ok(DocumentRevision::of(written).to_string())
}

fn verify_readback(
    before: &SexpNode,
    expected: &SexpNode,
    observed: &SexpNode,
    removed_power: &[String],
) -> Result<(), String> {
    let mut anchors = Vec::new();
    for tag in ["symbol", "no_connect"] {
        let previous = before
            .find_all(tag)
            .into_iter()
            .filter_map(|node| Some((node.find_str("uuid")?, parse_at(node)?)))
            .collect::<std::collections::BTreeMap<_, _>>();
        for node in expected.find_all(tag) {
            let uuid = node.find_str("uuid").expect("planner checks item UUIDs");
            let at = parse_at(node).expect("planner checks item positions");
            if previous.get(uuid) != Some(&at) {
                anchors.push((uuid.to_owned(), (at.0, at.1)));
            }
        }
    }
    if let Some(reason) = misplaced_item_in(observed, &anchors) {
        return Err(reason);
    }
    for tag in ["wire", "junction"] {
        if observed.find_all(tag) != expected.find_all(tag) {
            return Err(format!(
                "written schematic does not hold the planned {tag} inventory"
            ));
        }
    }
    let changes = sch_layout_score::cleanup_gate(before, observed, removed_power);
    if !changes.is_empty() {
        return Err(changes.join("; "));
    }
    Ok(())
}
