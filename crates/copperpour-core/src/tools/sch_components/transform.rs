//! Component geometric transforms: move, rotate, mirror, and region moves.

use super::placement::mirror_arg;
use super::readback::{
    component_target_from_source, copy_component_observation, load_committed_component_snapshot,
    load_component_mutation_readback, tool_error_summary, verified_component_readback,
    ComponentTarget,
};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::sch_block_move::{misplaced_item, move_net_changes, translate_block, Region};
use crate::tools::sch_field_autoplace::{
    field_preview_conflict, read_preview_source, rotation_fields, RotationFields,
};
use crate::tools::sch_wiring::{
    carry_no_connects, observed_no_connect_moves, reconcile_junctions_for_placement,
};
use crate::tools::{
    get_path, invalid_arg, is_symbol_quarter_turn, opt_bool_flag, opt_str, opt_u32, require_f64,
    require_str, symbol_rotation_arg, ToolContext,
};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    geometry::snap_point,
    parse_sexp,
    writer::{read_consistent, write_atomic_if_unchanged},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn handle_move_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    let new_x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let new_y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let (new_x, new_y) = snap_point(new_x, new_y, 1.27);
    // Unit 0 needs no check of its own: no unit 0 is ever placed, so it is
    // refused below with the units that are.
    let only_unit = match opt_u32(args, "unit") {
        Ok(unit) => unit,
        Err(error) => return Ok(error),
    };

    let before_source = read_consistent(&sch_path)?;
    let mut sch = cse::Schematic::from_source(&sch_path, before_source.clone())?;
    // A unit carried by two placements of the reference is refused here as an
    // ambiguous target, before anything is selected.
    let mut target = match component_target_from_source(&sch_path, &before_source, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };
    let moving = match select_units(&target, &reference, only_unit) {
        Ok(moving) => moving,
        Err(error) => return Ok(error),
    };
    let selected = moving.iter().cloned().collect::<BTreeSet<_>>();
    // The lowest-numbered unit of the selection is the one placed at (x, y);
    // the others of it follow by the same delta.
    let anchor_uuid = &moving[0];
    let anchor = sch
        .symbols
        .iter()
        .find(|symbol| symbol.uuid == *anchor_uuid)
        .expect("structural target UUID is present in the loaded schematic");
    let (old_x, old_y) = anchor.position();
    let (dx, dy) = (new_x - old_x, new_y - old_y);
    // Every unit left out is bound where the file has it, so the readback
    // proves it did not move.
    for unit in target
        .units
        .iter_mut()
        .filter(|unit| selected.contains(&unit.uuid))
    {
        unit.x += dx;
        unit.y += dy;
    }
    for symbol in sch
        .symbols
        .iter_mut()
        .filter(|symbol| selected.contains(&symbol.uuid))
    {
        symbol.translate(dx, dy);
    }
    // A no-connect belongs to the pin, not to the coordinate it was dropped
    // on, so it travels in this same write (#626).
    let carried = match carry_no_connects(&sch_path, &before_source, &mut sch) {
        Ok(carried) => carried,
        Err(error) => return Ok(error),
    };
    let candidate = sch.to_source();
    let (candidate, added, pruned) = reconcile_junctions_for_placement(&before_source, candidate);
    write_atomic_if_unchanged(&sch_path, &before_source, &candidate)?;
    let moved_markers =
        match observed_no_connect_moves(&sch_path, "move_schematic_component", &carried)? {
            Ok(moved) => moved,
            Err(error) => return Ok(error),
        };
    let observed = match load_component_mutation_readback(&sch_path, &target)? {
        Ok(observed) => observed,
        Err(error) => return Ok(error),
    };
    let mut result = json!({
        "moved": observed["reference"],
        "x": observed["x"],
        "y": observed["y"],
        "moved_units": selected.len(),
        "placements": observed["units"],
        "junctions_added_count": added,
        "junctions_pruned_count": pruned,
        "no_connects_moved_count": moved_markers.len(),
        "no_connects_moved": moved_markers
    });
    copy_component_observation(&mut result, &observed);
    Ok(CallToolResult::json(&result))
}

pub(super) async fn handle_rotate_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let autoplace = match opt_bool_flag(args, "autoplace_fields") {
        Ok(value) => value == Some(true),
        Err(error) => return Ok(error),
    };
    if !autoplace
        && ["approvals", "revision", "turn"]
            .iter()
            .any(|key| !args[*key].is_null())
    {
        return Ok(invalid_arg(
            "autoplace_fields",
            "requires true when supplying revision, turn or approvals",
        ));
    }
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    let rotation = match symbol_rotation_arg(&args["rotation"], "rotation") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let (before_source, authorization) = if autoplace {
        match read_preview_source(args, &sch_path)? {
            Ok((source, authorization)) => {
                if let Err(error) =
                    crate::tools::sch_layout_score::parse_saved_sheet(&sch_path, &source)
                {
                    return Ok(error);
                }
                (source, Some(authorization))
            }
            Err(error) => return Ok(error),
        }
    } else {
        (read_consistent(&sch_path)?, None)
    };
    let mut sch = cse::Schematic::from_source(&sch_path, before_source.clone())?;
    let mut target = match component_target_from_source(&sch_path, &before_source, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };
    if let Err(error) =
        check_saved_rotation("rotate_schematic_component", &reference, &target.units)
    {
        return Ok(error);
    }
    let target_uuids = target.uuids();
    let selected = target_uuids.iter().cloned().collect::<BTreeSet<_>>();
    let rotation_delta = rotation - target.units[0].rotation.rem_euclid(360.0);
    for unit in &mut target.units {
        let next = unit.rotation.rem_euclid(360.0) + rotation_delta;
        let (canonical_rotation, canonical_mirror) =
            cse::kicad_orientation(next, unit.mirror.as_deref());
        unit.rotation = canonical_rotation;
        unit.mirror = canonical_mirror.map(str::to_owned);
    }
    for symbol in sch
        .symbols
        .iter_mut()
        .filter(|symbol| selected.contains(&symbol.uuid))
    {
        // The delta lands each unit at its own angle, so a unit already at
        // 270° asked to follow a +90° turn computes 360° — normalize into
        // eeschema's canonical `(rotation, mirror)` pair before writing, so
        // KiCad's next save leaves the pair alone and a later rotation computes
        // its delta from the stored angle (#568).
        let new_rotation = (symbol.at.rotation.unwrap_or(0.0).rem_euclid(360.0) + rotation_delta)
            .rem_euclid(360.0);
        symbol.set_rotation(new_rotation);
        symbol.normalize_orientation();
    }
    // A turn relocates pin endpoints exactly as a move does, so the markers on
    // those pins travel with them here too (#626).
    let carried = match carry_no_connects(&sch_path, &before_source, &mut sch) {
        Ok(carried) => carried,
        Err(error) => return Ok(error),
    };
    let candidate = sch.to_source();
    let (candidate, added, pruned) = reconcile_junctions_for_placement(&before_source, candidate);
    let (candidate, field_readback) = if let Some(authorization) = authorization {
        match rotation_fields(
            args,
            &before_source,
            authorization,
            candidate,
            &selected,
            rotation,
        )? {
            Ok(RotationFields::Preview(preview)) => return Ok(preview),
            Ok(RotationFields::Apply(candidate, observe)) => (candidate, Some(observe)),
            Err(error) => return Ok(error),
        }
    } else {
        (candidate, None)
    };
    match write_atomic_if_unchanged(&sch_path, &before_source, &candidate) {
        Ok(()) => {}
        Err(copperpour_sexp::SexpError::Conflict { path }) if autoplace => {
            return Ok(field_preview_conflict(&path));
        }
        Err(error) => return Err(error.into()),
    }
    let moved_markers =
        match observed_no_connect_moves(&sch_path, "rotate_schematic_component", &carried)? {
            Ok(moved) => moved,
            Err(error) => return Ok(error),
        };
    let (observed, fields) = if let Some(field_readback) = field_readback {
        let uncertain_turn = |reason| {
            crate::outcome::extend(
                crate::tools::mutation_outcome_uncertain(
                    &sch_path,
                    "rotate_schematic_component",
                    reason,
                ),
                json!({"observed_before_uncertainty": {"no_connects_moved": moved_markers}}),
            )
        };
        let (saved, committed) = match load_committed_component_snapshot(&sch_path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(uncertain_turn(format!(
                    "Could not read the saved turn: {error}"
                )))
            }
        };
        let observed = match verified_component_readback(&sch_path, &committed, &target) {
            Ok(observed) => observed,
            Err(error) => return Ok(uncertain_turn(tool_error_summary(&error))),
        };
        (
            observed,
            Some(field_readback.observe(&sch_path, &saved, &candidate)),
        )
    } else {
        match load_component_mutation_readback(&sch_path, &target)? {
            Ok(observed) => (observed, None),
            Err(error) => return Ok(error),
        }
    };
    let mut result = json!({
        "rotated": observed["reference"],
        "rotation": observed["rotation"],
        "rotated_units": observed["unit_count"],
        "placements": observed["units"],
        "junctions_added_count": added,
        "junctions_pruned_count": pruned,
        "no_connects_moved_count": moved_markers.len(),
        "no_connects_moved": moved_markers
    });
    copy_component_observation(&mut result, &observed);
    if let Some(fields) = fields {
        match fields {
            Ok((fields, withdrawn)) => {
                result["approved_field_placements"] = json!(fields);
                result["withdrawn_offers"] = json!(withdrawn);
            }
            Err(error) => {
                return Ok(crate::outcome::extend(
                    error,
                    json!({"observed_before_uncertainty": {
                        "placements": result["placements"],
                        "no_connects_moved": moved_markers
                    }}),
                ));
            }
        }
    }
    Ok(CallToolResult::json(&result))
}

/// Transforming fields and no-connects needs the old pin geometry as well as
/// the new angle. Do not use non-quarter-turn saved angles for that geometry.
fn check_saved_rotation<'a>(
    operation: &str,
    reference: &str,
    units: impl IntoIterator<Item = &'a super::readback::ComponentTargetUnit>,
) -> Result<(), CallToolResult> {
    for unit in units {
        if !is_symbol_quarter_turn(unit.rotation) {
            let reason = format!(
                "{reference} unit {} has a saved rotation of {} degrees. \
                 Open the sheet in KiCad, correct or resave its orientation, and retry.",
                unit.unit, unit.rotation
            );
            return Err(CallToolResult::error_kind(
                ToolErrorKind::PlanBlocked {
                    operation: operation.into(),
                    reasons: vec![reason.clone()],
                },
                reason,
            ));
        }
    }
    Ok(())
}

/// The UUIDs of the placements a `unit` argument selects, in unit order: the
/// one carrying that unit number, or every placement when it is omitted. A
/// unit the component has no placement for is an invalid argument that names
/// the placed ones.
fn select_units(
    target: &ComponentTarget,
    reference: &str,
    only_unit: Option<u32>,
) -> Result<Vec<String>, CallToolResult> {
    let selected = target
        .units
        .iter()
        .filter(|unit| only_unit.is_none_or(|wanted| unit.unit == wanted))
        .map(|unit| unit.uuid.clone())
        .collect::<Vec<_>>();
    if selected.is_empty() {
        let placed = target
            .units
            .iter()
            .map(|unit| unit.unit)
            .collect::<BTreeSet<_>>();
        return Err(crate::tools::invalid_arg(
            "unit",
            &format!(
                "'{reference}' has no placed unit {} on this sheet (placed: {})",
                only_unit.unwrap_or_default(),
                placed
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(selected)
}

pub(super) async fn handle_mirror_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    // Required here, unlike at placement: an omitted axis would otherwise
    // read as "none" and clear a mirror the caller never mentioned.
    if let Err(error) = require_str(args, "mirror") {
        return Ok(error);
    }
    let mirror = match mirror_arg(args, "mirror") {
        Ok(mirror) => mirror,
        Err(error) => return Ok(error),
    };
    // Unit 0 needs no check of its own: no unit 0 is ever placed, so it is
    // refused below with the units that are.
    let only_unit = match opt_u32(args, "unit") {
        Ok(unit) => unit,
        Err(error) => return Ok(error),
    };

    let before_source = read_consistent(&sch_path)?;
    let mut sch = cse::Schematic::from_source(&sch_path, before_source.clone())?;
    let mut target = match component_target_from_source(&sch_path, &before_source, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };
    // KiCad stores the mirror per placed unit, so the selection is by unit
    // number. Every unit left out keeps the mirror the file already gives it,
    // and the readback below holds it to that bound state.
    let selected = match select_units(&target, &reference, only_unit) {
        Ok(selected) => selected.into_iter().collect::<BTreeSet<_>>(),
        Err(error) => return Ok(error),
    };
    if let Err(error) = check_saved_rotation(
        "mirror_schematic_component",
        &reference,
        target
            .units
            .iter()
            .filter(|unit| selected.contains(&unit.uuid)),
    ) {
        return Ok(error);
    }
    // The intent is bound apart from the writer: the requested reflection on
    // each unit's own turn, in the pair eeschema writes for it. A unit whose
    // bound pair is unchanged is already as requested.
    let mut changed = BTreeSet::new();
    for unit in target
        .units
        .iter_mut()
        .filter(|unit| selected.contains(&unit.uuid))
    {
        let (rotation, mirror) = cse::kicad_orientation(unit.rotation, mirror);
        if (rotation, mirror) != (unit.rotation, unit.mirror.as_deref()) {
            changed.insert(unit.uuid.clone());
        }
        unit.rotation = rotation;
        unit.mirror = mirror.map(str::to_owned);
    }
    // Nothing to change leaves the file alone, timestamp included. Judged on
    // the bound pairs, not the bytes: re-serializing an untouched sheet still
    // re-wraps its `(pts …)` lists (#210).
    let (added, pruned, moved_markers) = if changed.is_empty() {
        (0, 0, Vec::new())
    } else {
        for symbol in sch
            .symbols
            .iter_mut()
            .filter(|symbol| changed.contains(&symbol.uuid))
        {
            // Reflects the field text with the body (#613), then stores the
            // orientation as eeschema would, so its next save changes nothing.
            symbol.set_mirror(mirror);
            symbol.normalize_orientation();
        }
        // A reflection relocates pin endpoints exactly as a turn does, so the
        // markers on those pins travel with them here too (#626).
        let carried = match carry_no_connects(&sch_path, &before_source, &mut sch) {
            Ok(carried) => carried,
            Err(error) => return Ok(error),
        };
        let candidate = sch.to_source();
        let (candidate, added, pruned) =
            reconcile_junctions_for_placement(&before_source, candidate);
        write_atomic_if_unchanged(&sch_path, &before_source, &candidate)?;
        let moved_markers =
            match observed_no_connect_moves(&sch_path, "mirror_schematic_component", &carried)? {
                Ok(moved) => moved,
                Err(error) => return Ok(error),
            };
        (added, pruned, moved_markers)
    };
    let observed = match load_component_mutation_readback(&sch_path, &target)? {
        Ok(observed) => observed,
        Err(error) => return Ok(error),
    };
    // Each selected unit as the committed file has it. The readback has just
    // held every unit to its bound pair, so `changed` is a fact about the
    // file, not an echo of the request. The top-level fields copied below
    // describe the lowest-numbered unit, as in every component tool, which is
    // not necessarily one that was selected.
    let mirrored_units = observed["units"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|unit| {
            let uuid = unit["uuid"].as_str()?;
            selected.contains(uuid).then(|| {
                json!({
                    "unit": unit["unit"],
                    "uuid": uuid,
                    "rotation": unit["rotation"],
                    "mirror_x": unit["mirror_x"],
                    "mirror_y": unit["mirror_y"],
                    "changed": changed.contains(uuid)
                })
            })
        })
        .collect::<Vec<_>>();
    let mut result = json!({
        "mirrored": observed["reference"],
        "mirrored_units": mirrored_units,
        "changed_unit_count": changed.len(),
        "placements": observed["units"],
        "junctions_added_count": added,
        "junctions_pruned_count": pruned,
        "no_connects_moved_count": moved_markers.len(),
        "no_connects_moved": moved_markers
    });
    copy_component_observation(&mut result, &observed);
    Ok(CallToolResult::json(&result))
}

pub(super) async fn handle_move_connected(
    _args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    // The old implementation moved only the symbol but reported that it
    // preserved connections. Refuse until the tool can move the wires too.
    Ok(CallToolResult::error_kind(
        ToolErrorKind::UnsupportedCapability {
            capability: "move_connected".to_owned(),
            kicad_version: None,
        },
        "move_connected is not implemented. Use move_schematic_component (moves the symbol \
         only), then re-route or use connect_pins for the affected nets.",
    ))
}

pub(crate) async fn handle_move_region(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let x1 = match require_f64(args, "x1") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y1 = match require_f64(args, "y1") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let x2 = match require_f64(args, "x2") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y2 = match require_f64(args, "y2") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let dx = match require_f64(args, "dx") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let dy = match require_f64(args, "dy") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let rigid = match opt_str(args, "include") {
        None | Some("symbols") => false,
        Some("all") => true,
        Some(_) => return Ok(invalid_arg("include", "must be 'symbols' or 'all'")),
    };

    let before_source = read_consistent(&sch_path)?;
    let mut sch = cse::Schematic::from_source(&sch_path, before_source.clone())?;

    // Select placements by UUID, not reference. A multi-unit reference may
    // have one unit inside the rectangle and another outside; resolving the
    // selected reference back through `by_reference_mut` moved unit 1 every
    // time, and could move it twice when both units were selected (#182).
    let uuids_to_move: std::collections::HashSet<String> = sch
        .symbols
        .within_rectangle(x1, y1, x2, y2)
        .iter()
        .map(|symbol| symbol.uuid.clone())
        .collect();

    let mut moved_uuids = Vec::new();
    let mut expected_positions = BTreeMap::new();
    for symbol in sch.symbols.iter_mut() {
        if uuids_to_move.contains(&symbol.uuid) {
            let (old_x, old_y) = symbol.position();
            // A rigid block keeps its own geometry, on grid or not: snapping
            // each symbol alone would pull its pins off the wires that moved
            // with it.
            let (new_x, new_y) = if rigid {
                (old_x + dx, old_y + dy)
            } else {
                snap_point(old_x + dx, old_y + dy, 1.27)
            };
            symbol.move_to(new_x, new_y);
            moved_uuids.push(symbol.uuid.clone());
            expected_positions.insert(symbol.uuid.clone(), (new_x, new_y));
        }
    }
    // The sheet as loaded, beside everything else the rigid move translated.
    let block = if rigid {
        let before_tree = parse_sexp(&before_source)?;
        let Some(placed) = crate::tools::resolved_placed_pins_by_reference(&before_tree) else {
            return Ok(move_region_blocked(vec![
                crate::tools::UNRESOLVED_PIN_GEOMETRY.to_owned(),
            ]));
        };
        let region = Region::new(x1, y1, x2, y2);
        match translate_block(&mut sch, region, dx, dy, &placed) {
            Ok(block) => Some((before_tree, block)),
            Err(reasons) => return Ok(move_region_blocked(reasons)),
        }
    } else {
        None
    };

    // Region movement is one placement change, not a series of independent
    // moves. Carry marker intent and reconcile the whole-sheet symmetric pin
    // difference once so pins that swap coordinates do not manufacture or
    // remove a junction between the per-symbol steps (#623, #626).
    let mut carried = match carry_no_connects(&sch_path, &before_source, &mut sch) {
        Ok(carried) => carried,
        Err(error) => return Ok(error),
    };
    let candidate = sch.to_source();
    let (candidate, added, pruned) = reconcile_junctions_for_placement(&before_source, candidate);
    // Checked after the junction pass, so a dot it adds where a moved pin lands
    // on a wire outside the box counts as the net change it is.
    if let Some((before_tree, _)) = &block {
        let changes = move_net_changes(before_tree, &parse_sexp(&candidate)?);
        if !changes.is_empty() {
            return Ok(move_region_blocked(changes));
        }
    }
    write_atomic_if_unchanged(&sch_path, &before_source, &candidate)?;

    let items_moved = if let Some((_, block)) = block {
        carried.extend(block.free_no_connects);
        let written = read_consistent(&sch_path)?;
        if let Some(reason) = misplaced_item(&written, &block.anchors) {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "move_region",
                reason,
            ));
        }
        Some(block.counts)
    } else {
        None
    };
    let moved_markers = match observed_no_connect_moves(&sch_path, "move_region", &carried)? {
        Ok(moved) => moved,
        Err(error) => return Ok(error),
    };

    // Planned coordinates are not proof that the committed file contains the
    // move. Bind the response back to the selected UUIDs and reject an
    // incomplete or divergent readback as uncertain after mutation.
    let committed = match cse::Schematic::load(&sch_path) {
        Ok(committed) => committed,
        Err(error) => {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "move_region",
                format!("the written schematic cannot be read back: {error}"),
            ));
        }
    };
    let mut moved_references = Vec::new();
    let mut placements = Vec::new();
    for uuid in &moved_uuids {
        let Some(symbol) = committed.symbols.iter().find(|symbol| symbol.uuid == *uuid) else {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "move_region",
                format!("selected symbol UUID {uuid} is missing from the written schematic"),
            ));
        };
        let (observed_x, observed_y) = symbol.position();
        let &(expected_x, expected_y) = expected_positions
            .get(uuid)
            .expect("every selected UUID has a planned position");
        if !copperpour_sexp::geometry::points_coincident(
            observed_x,
            observed_y,
            expected_x,
            expected_y,
            crate::tools::sch_connectivity::COINCIDENT_TOLERANCE,
        ) {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "move_region",
                format!(
                    "selected symbol UUID {uuid} was written at ({observed_x}, {observed_y}), expected ({expected_x}, {expected_y})"
                ),
            ));
        }
        let reference = symbol.reference().unwrap_or("?").to_string();
        if !moved_references.contains(&reference) {
            moved_references.push(reference.clone());
        }
        placements.push(json!({
            "reference": reference,
            "unit": symbol.unit,
            "x": observed_x,
            "y": observed_y
        }));
    }

    let mut response = json!({
        "moved_count": moved_references.len(),
        "moved": moved_references,
        "moved_unit_count": placements.len(),
        "placements": placements,
        "junctions_added_count": added,
        "junctions_pruned_count": pruned,
        "no_connects_moved_count": moved_markers.len(),
        "no_connects_moved": moved_markers
    });
    if let Some(items_moved) = items_moved {
        response["items_moved"] = json!(items_moved);
    }
    Ok(CallToolResult::json(&response))
}

/// Why a rigid `move_region` cannot move its block, before anything is written.
fn move_region_blocked(reasons: Vec<String>) -> CallToolResult {
    let message = format!(
        "move_region cannot move the region as one block: {}",
        reasons.join("; ")
    );
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: "move_region".to_owned(),
            reasons,
        },
        message,
    )
}
