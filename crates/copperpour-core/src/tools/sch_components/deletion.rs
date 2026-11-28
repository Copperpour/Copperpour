//! Component and schematic item deletion planning and commit.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::{get_path, require_str, ToolContext};
use copperpour_sexp::{
    commit_command, parse_sexp, prepare_command,
    schematic::{extract_symbol_instances, extract_wires, pin_endpoint},
    writer::{find_direct_child_blocks, read_consistent},
    ItemAnchor, ItemId, SchematicCommand, SexpError,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn handle_delete_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };

    let content = read_consistent(&sch_path)?;
    let plan = match plan_component_deletion(&sch_path, &content, &reference) {
        Ok(plan) => plan,
        Err(error) => return Ok(error.into_result()),
    };
    let outcome = match commit_component_deletion(&sch_path, plan)? {
        Ok(outcome) => outcome,
        Err(refusal) => return Ok(refusal),
    };
    let unit_uuids = outcome
        .units_by_reference
        .get(&reference)
        .cloned()
        .unwrap_or_default();

    Ok(CallToolResult::json(&json!({
        "deleted": reference,
        "deleted_units": unit_uuids.len(),
        "deleted_unit_uuids": unit_uuids,
        "removed_no_connects_count": outcome.marker_uuids.len(),
        "removed_no_connect_uuids": outcome.marker_uuids,
        "junctions_added_count": outcome.added_junctions.len(),
        "junctions_added_uuids": outcome.added_junctions,
        "junctions_pruned_count": outcome.pruned_junctions.len(),
        "junctions_pruned_uuids": outcome.pruned_junctions
    })))
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedSchematicItem {
    pub(crate) kind: String,
    pub(super) source: String,
}

pub(crate) struct ComponentDeletePlan {
    pub(super) command: SchematicCommand,
    before_items: BTreeMap<String, IndexedSchematicItem>,
    unit_uuids: Vec<String>,
    units_by_reference: BTreeMap<String, Vec<String>>,
    marker_uuids: Vec<String>,
    item_uuids: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ComponentDeleteOutcome {
    pub(crate) units_by_reference: BTreeMap<String, Vec<String>>,
    pub(crate) marker_uuids: Vec<String>,
    pub(crate) item_uuids: Vec<String>,
    pub(crate) added_junctions: Vec<String>,
    pub(crate) pruned_junctions: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum ComponentDeleteTargetError {
    Ambiguous {
        target: String,
        candidates: Vec<String>,
    },
    Stale {
        target: String,
        reason: String,
    },
}

impl ComponentDeleteTargetError {
    pub(super) fn stale(path: &std::path::Path, reason: impl Into<String>) -> Self {
        Self::Stale {
            target: path.display().to_string(),
            reason: reason.into(),
        }
    }

    pub(super) fn from_sexp(path: &std::path::Path, error: SexpError) -> Self {
        Self::stale(path, error.to_string())
    }

    pub(crate) fn into_result(self) -> CallToolResult {
        match self {
            Self::Ambiguous { target, candidates } => {
                let reason = format!(
                    "more than one schematic item identifies the target: {}",
                    candidates.join(", ")
                );
                CallToolResult::error_kind(
                    ToolErrorKind::AmbiguousTarget {
                        target: target.clone(),
                        candidates,
                    },
                    format!("cannot safely resolve {target}: {reason}"),
                )
            }
            Self::Stale { target, reason } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: target.clone(),
                    reason: reason.clone(),
                },
                format!("cannot safely resolve {target}: {reason}"),
            ),
        }
    }
}

pub(crate) fn indexed_uuid_items(
    path: &std::path::Path,
    content: &str,
) -> Result<BTreeMap<String, IndexedSchematicItem>, ComponentDeleteTargetError> {
    let ranges = find_direct_child_blocks(content, "kicad_sch");
    if ranges.is_empty() {
        return Err(ComponentDeleteTargetError::stale(
            path,
            "the kicad_sch root is missing or malformed",
        ));
    }
    let mut items = BTreeMap::new();
    for (start, end) in ranges {
        let node = parse_sexp(&content[start..end])
            .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        let Some(uuid) = node.find_str("uuid") else {
            continue;
        };
        let item = IndexedSchematicItem {
            kind: node.head().unwrap_or("unknown").to_owned(),
            source: content[start..end].to_owned(),
        };
        if let Some(previous) = items.insert(uuid.to_owned(), item) {
            return Err(ComponentDeleteTargetError::Ambiguous {
                target: format!("schematic UUID {uuid}"),
                candidates: vec![previous.kind, node.head().unwrap_or("unknown").to_owned()],
            });
        }
    }
    Ok(items)
}

pub(super) fn plan_component_deletion(
    path: &std::path::Path,
    content: &str,
    reference: &str,
) -> Result<ComponentDeletePlan, ComponentDeleteTargetError> {
    plan_component_deletions(path, content, &[reference.to_owned()])
}

pub(crate) fn plan_component_deletions(
    path: &std::path::Path,
    content: &str,
    references: &[String],
) -> Result<ComponentDeletePlan, ComponentDeleteTargetError> {
    plan_component_and_item_deletions(path, content, references, &[])
}

pub(crate) fn plan_component_and_item_deletions(
    path: &std::path::Path,
    content: &str,
    references: &[String],
    item_uuids: &[String],
) -> Result<ComponentDeletePlan, ComponentDeleteTargetError> {
    let references = references.iter().cloned().collect::<BTreeSet<_>>();
    let mut item_uuids = item_uuids.to_vec();
    item_uuids.sort();
    item_uuids.dedup();
    if references.is_empty() && item_uuids.is_empty() {
        return Err(ComponentDeleteTargetError::stale(
            path,
            "no schematic items were selected",
        ));
    }
    let reference_label = if references.is_empty() {
        "selected items".to_owned()
    } else {
        references.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let tree =
        parse_sexp(content).map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
    let before_items = indexed_uuid_items(path, content)?;
    for uuid in &item_uuids {
        if !before_items.contains_key(uuid) {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("schematic item UUID {uuid} is not present"),
            ));
        }
    }
    let instances = extract_symbol_instances(&tree);
    let selected = instances
        .iter()
        .filter(|instance| references.contains(&instance.reference))
        .collect::<Vec<_>>();
    let found = selected
        .iter()
        .map(|instance| instance.reference.clone())
        .collect::<BTreeSet<_>>();
    if let Some(missing) = references.difference(&found).next() {
        return Err(ComponentDeleteTargetError::stale(
            path,
            format!("component {missing} is not present"),
        ));
    }

    let mut by_unit: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
    let mut units_by_reference: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unit_uuids = Vec::new();
    for instance in &selected {
        let uuid = instance.uuid.clone().ok_or_else(|| {
            ComponentDeleteTargetError::stale(
                path,
                format!(
                    "component {} unit {} has no UUID",
                    instance.reference, instance.unit
                ),
            )
        })?;
        if before_items
            .get(&uuid)
            .is_none_or(|item| item.kind != "symbol")
        {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("UUID {uuid} no longer identifies a top-level symbol"),
            ));
        }
        by_unit
            .entry((instance.reference.clone(), instance.unit))
            .or_default()
            .push(uuid.clone());
        units_by_reference
            .entry(instance.reference.clone())
            .or_default()
            .push(uuid.clone());
        unit_uuids.push(uuid);
    }
    if let Some(((reference, unit), uuids)) = by_unit.iter().find(|(_, uuids)| uuids.len() > 1) {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: format!("component {reference} unit {unit}"),
            candidates: uuids.clone(),
        });
    }
    for uuids in units_by_reference.values_mut() {
        uuids.sort();
        uuids.dedup();
    }
    unit_uuids.sort();
    unit_uuids.dedup();
    if unit_uuids.len() != selected.len() {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: format!("components {reference_label}"),
            candidates: unit_uuids,
        });
    }

    // Require every placed symbol's library definition to resolve before
    // deciding marker ownership or junction validity. Unknown pins are stale
    // state, not evidence that no pin remains at a coordinate.
    let grouped = if selected.is_empty() {
        Vec::new()
    } else {
        crate::tools::resolved_placed_pins_by_reference(&tree).ok_or_else(|| {
            ComponentDeleteTargetError::stale(path, crate::tools::UNRESOLVED_PIN_GEOMETRY)
        })?
    };
    let selected_ids = unit_uuids.iter().cloned().collect::<BTreeSet<_>>();
    let mut affected = Vec::new();
    let mut remaining = Vec::new();
    for (instance, pins) in grouped {
        let points = pins
            .into_iter()
            .map(|(pin, transform)| pin_endpoint(&pin, transform));
        if instance
            .uuid
            .as_ref()
            .is_some_and(|uuid| selected_ids.contains(uuid))
        {
            affected.extend(points);
        } else {
            remaining.extend(points);
        }
    }
    let nearby_affected = crate::tools::dedup_points(affected.iter().copied());
    let remaining = crate::tools::dedup_points(remaining);

    let mut marker_uuids = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_sch") {
        let node = parse_sexp(&content[start..end])
            .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        if node.head() != Some("no_connect") {
            continue;
        }
        let Some((x, y, _)) = copperpour_sexp::schematic::parse_at(&node) else {
            continue;
        };
        let attached = nearby_affected
            .iter()
            .any(|&(px, py)| copperpour_sexp::geometry::points_coincident(x, y, px, py, 0.01));
        let still_owned = remaining
            .iter()
            .any(|&(px, py)| copperpour_sexp::geometry::points_coincident(x, y, px, py, 0.01));
        if attached && !still_owned {
            let uuid = node.find_str("uuid").ok_or_else(|| {
                ComponentDeleteTargetError::stale(
                    path,
                    format!("attached no-connect at ({x}, {y}) has no UUID"),
                )
            })?;
            marker_uuids.push(uuid.to_owned());
        }
    }
    marker_uuids.sort();
    marker_uuids.dedup();

    let initial_uuid_set = unit_uuids
        .iter()
        .chain(&marker_uuids)
        .chain(&item_uuids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let initial_ids = initial_uuid_set
        .into_iter()
        .map(ItemId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
    let initial = SchematicCommand::delete_items(content, initial_ids, "prepare component delete")
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
    let candidate = prepare_command(path, content, &initial)
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?
        .0;
    let removed_wires = extract_wires(&tree)
        .into_iter()
        .filter(|wire| {
            wire.uuid
                .as_ref()
                .is_some_and(|uuid| item_uuids.binary_search(uuid).is_ok())
        })
        .collect::<Vec<_>>();
    let (reconciled, _, _) = if removed_wires.is_empty() {
        crate::tools::sch_wiring::reconcile_junctions_at(candidate, &nearby_affected)
    } else {
        crate::tools::sch_wiring::reconcile_junctions_after_wire_deletions(
            candidate,
            &removed_wires,
            &affected,
        )
    };

    let after_items = indexed_uuid_items(path, &reconciled)?;
    let before_ids = before_items.keys().cloned().collect::<BTreeSet<_>>();
    let after_ids = after_items.keys().cloned().collect::<BTreeSet<_>>();
    let removed = before_ids
        .difference(&after_ids)
        .cloned()
        .collect::<Vec<_>>();
    let added = after_ids
        .difference(&before_ids)
        .cloned()
        .collect::<Vec<_>>();
    let modified = before_ids
        .intersection(&after_ids)
        .filter(|uuid| before_items[*uuid].source != after_items[*uuid].source)
        .cloned()
        .collect::<Vec<_>>();

    let mut changes = Vec::new();
    if !removed.is_empty() {
        let command = SchematicCommand::delete_items(
            content,
            removed
                .iter()
                .map(|uuid| ItemId::new(uuid.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?,
            format!("delete {reference_label} and dependent markers"),
        )
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        changes.extend(command.changes);
    }
    for uuid in added {
        let command = SchematicCommand::insert_item(
            content,
            after_items[&uuid].source.clone(),
            ItemAnchor::EndOfDocument,
            format!("restore junction after deleting {reference_label}"),
        )
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        changes.extend(command.changes);
    }
    for uuid in modified {
        let command = SchematicCommand::replace_item(
            content,
            ItemId::new(uuid.clone())
                .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?,
            after_items[&uuid].source.clone(),
            format!("reconcile {uuid} after deleting {reference_label}"),
        )
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        changes.extend(command.changes);
    }
    let command = SchematicCommand::from_changes(
        content,
        format!("delete components {reference_label}"),
        changes,
    )
    .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?
    .requiring_unchanged_document();
    let prepared = prepare_command(path, content, &command)
        .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?
        .0;
    if parse_sexp(&prepared).ok() != parse_sexp(&reconciled).ok() {
        return Err(ComponentDeleteTargetError::stale(
            path,
            "the structural command cannot represent every dependent connectivity edit",
        ));
    }

    Ok(ComponentDeletePlan {
        command,
        before_items,
        unit_uuids,
        units_by_reference,
        marker_uuids,
        item_uuids,
    })
}

pub(crate) fn commit_component_deletion(
    path: &std::path::Path,
    plan: ComponentDeletePlan,
) -> anyhow::Result<Result<ComponentDeleteOutcome, CallToolResult>> {
    if let Err(error) = commit_command(path, &plan.command) {
        if let Some(refusal) = component_delete_commit_refusal(path, &error) {
            return Ok(Err(refusal));
        }
        return Err(error.into());
    }

    let committed = read_consistent(path)?;
    let after_items = match indexed_uuid_items(path, &committed) {
        Ok(items) => items,
        Err(error) => return Ok(Err(error.into_result())),
    };
    let after_tree = match parse_sexp(&committed) {
        Ok(tree) => tree,
        Err(error) => {
            return Ok(Err(
                ComponentDeleteTargetError::from_sexp(path, error).into_result()
            ));
        }
    };
    for reference in plan.units_by_reference.keys() {
        let remaining = extract_symbol_instances(&after_tree)
            .into_iter()
            .filter(|instance| instance.reference == *reference)
            .count();
        if remaining != 0 {
            return Ok(Err(ComponentDeleteTargetError::stale(
                path,
                format!("post-write readback still contains {remaining} unit(s) of {reference}"),
            )
            .into_result()));
        }
    }
    for uuid in plan
        .unit_uuids
        .iter()
        .chain(&plan.marker_uuids)
        .chain(&plan.item_uuids)
    {
        if after_items.contains_key(uuid) {
            return Ok(Err(ComponentDeleteTargetError::stale(
                path,
                format!("post-write readback still contains deleted item UUID {uuid}"),
            )
            .into_result()));
        }
    }

    let before_junctions = plan
        .before_items
        .iter()
        .filter_map(|(uuid, item)| (item.kind == "junction").then_some(uuid.clone()))
        .collect::<BTreeSet<_>>();
    let after_junctions = after_items
        .iter()
        .filter_map(|(uuid, item)| (item.kind == "junction").then_some(uuid.clone()))
        .collect::<BTreeSet<_>>();
    let pruned_junctions = before_junctions
        .difference(&after_junctions)
        .cloned()
        .collect::<Vec<_>>();
    let added_junctions = after_junctions
        .difference(&before_junctions)
        .cloned()
        .collect::<Vec<_>>();

    Ok(Ok(ComponentDeleteOutcome {
        units_by_reference: plan.units_by_reference,
        marker_uuids: plan.marker_uuids,
        item_uuids: plan.item_uuids,
        added_junctions,
        pruned_junctions,
    }))
}

pub(super) fn component_delete_commit_refusal(
    path: &std::path::Path,
    error: &SexpError,
) -> Option<CallToolResult> {
    let reason = match error {
        SexpError::Conflict { .. } => "the schematic changed after deletion was planned",
        SexpError::ItemConflict { reason, .. } => reason,
        SexpError::KiCadEditorLocked { .. } => {
            "KiCad owns the schematic; use a live editor mutation or close the document"
        }
        _ => return None,
    };
    Some(ComponentDeleteTargetError::stale(path, reason).into_result())
}
