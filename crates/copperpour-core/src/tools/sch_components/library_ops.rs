//! Component library updates, replacement, field resets, annotations, and grouping.

use super::deletion::ComponentDeleteTargetError;
use super::properties::{
    closing_quote, dnp_property_refusal, escape_property_text, is_dnp_property,
    is_reserved_property, set_property_value,
};
use super::readback::{
    commit_verified_component_mutation, component_target_from_source, copy_component_observation,
    load_component_mutation_readback, tool_error_summary, ComponentMutationIntent,
};
use crate::mcp::protocol::CallToolResult;
use crate::tools::reference_hint::component_not_found;
use crate::tools::sch_wiring::{
    apply_no_connect_carries, no_connect_carry_unwritable, observed_no_connect_moves,
    plan_no_connect_carry_for_replacement, reconcile_junctions_for_placement,
};
use crate::tools::{
    find_all_symbol_instance_blocks, get_path, opt_u32, reembed_lib_symbols, require_array,
    require_str, ReembedOutcome, ToolContext,
};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    parse_sexp,
    schematic::{extract_symbol_instances, read_schematic},
    writer::{apply_edits, read_consistent, write_atomic_if_unchanged, SexpEdit},
    SchematicCommand,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn handle_add_component_annotation(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    let key = match require_str(args, "key") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let value = match require_str(args, "value") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    // Reference/Value/Footprint/Datasheet have dedicated parameters on
    // edit_schematic_component with their own side effects — a Reference
    // rename must also rewrite the instances path (#157) — so annotating
    // them here would bypass those. DNP is not a property at all.
    if is_reserved_property(&key) {
        if is_dnp_property(&key) {
            return Ok(crate::tools::invalid_arg(
                "key",
                &format!(
                    "{} on edit_schematic_component, not as an annotation",
                    dnp_property_refusal(&key)
                ),
            ));
        }
        return Ok(CallToolResult::error(format!(
            "'{key}' is a built-in field — set it through edit_schematic_component's \
             dedicated parameter, not as an annotation."
        )));
    }

    let content = read_consistent(&sch_path)?;
    let expected = content.clone();
    let target = match component_target_from_source(&sch_path, &expected, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };

    // An existing key is updated in place; appending a second `(property
    // "KEY" …)` gives eeschema two fields with one name — it shows both,
    // edits the wrong one, and the duplicate survives save/reload (#203).
    // A new key is anchored separately at every unit's own position and uses
    // that block's indentation. A partially populated legacy component is
    // repaired by updating existing copies and adding only the missing ones.
    let (new_content, _) = match set_property_value(&content, &reference, &key, &value, true) {
        Ok(updated) => updated,
        Err(why) => return Ok(CallToolResult::error(format!("{key}: {why}"))),
    };
    let item_ids = match target.item_ids() {
        Ok(item_ids) => item_ids,
        Err(error) => return Ok(error.into_result()),
    };
    let command = SchematicCommand::replace_items_from_document(
        &expected,
        &new_content,
        item_ids,
        format!("Add {key} property to {reference}"),
    )?;
    let intent = ComponentMutationIntent::new(
        target.with_fields(&BTreeMap::from([(key.clone(), value.clone())])),
    );
    let observed = match commit_verified_component_mutation(
        &sch_path,
        &expected,
        &command,
        std::slice::from_ref(&intent),
        "add_component_annotation",
    )? {
        Ok(mut observed) => observed.remove(0),
        Err(error) => return Ok(error),
    };
    let updated_units = target
        .units
        .iter()
        .filter(|unit| unit.fields.contains_key(&key))
        .count();
    let added_units = target.units.len() - updated_units;
    let mut result = json!({
        "reference": observed["reference"],
        "added_property": key,
        "value": observed["fields"][key.as_str()],
        "updated_existing": updated_units > 0,
        "updated_units": updated_units,
        "added_units": added_units
    });
    copy_component_observation(&mut result, &observed);
    result["component_value"] = observed["value"].clone();
    result["value"] = observed["fields"][key.as_str()].clone();
    Ok(CallToolResult::json(&result))
}

pub(super) async fn handle_group_components(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let group_name = match require_str(args, "group_name") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let refs = match require_array(args, "references") {
        Ok(values) => values,
        Err(error) => return Ok(error),
    };

    if refs.is_empty() {
        return Ok(CallToolResult::error("No references provided"));
    }

    let mut content = read_consistent(&sch_path)?;
    let expected = content.clone();
    let mut item_ids = Vec::new();
    let mut targets = Vec::new();
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();

    for value in refs {
        let Some(reference) = value.as_str() else {
            errors.push("Component reference must be a string".to_owned());
            continue;
        };
        if !seen.insert(reference.to_owned()) {
            continue;
        }
        let target = match component_target_from_source(&sch_path, &expected, reference) {
            Ok(target) => target,
            Err(ComponentDeleteTargetError::Stale { reason, .. })
                if reason.contains("is not present") =>
            {
                errors.push(format!("Component '{reference}' not found"));
                continue;
            }
            Err(error) => return Ok(error.into_result()),
        };
        match set_property_value(&content, reference, "Group", &group_name, true) {
            Ok((updated, _)) => {
                content = updated;
                item_ids.extend(match target.item_ids() {
                    Ok(item_ids) => item_ids,
                    Err(error) => return Ok(error.into_result()),
                });
                targets.push(target);
            }
            Err(error) => errors.push(format!("{reference}: {error}")),
        }
    }

    if item_ids.is_empty() {
        return Ok(ComponentDeleteTargetError::stale(
            &sch_path,
            if errors.is_empty() {
                "no components were selected".to_owned()
            } else {
                errors.join("; ")
            },
        )
        .into_result());
    }
    let command = SchematicCommand::replace_items_from_document(
        &expected,
        &content,
        item_ids,
        format!("Group components as {group_name}"),
    )?;
    let intents = targets
        .iter()
        .map(|target| {
            ComponentMutationIntent::new(
                target.with_fields(&BTreeMap::from([("Group".to_owned(), group_name.clone())])),
            )
        })
        .collect::<Vec<_>>();
    let observations = match commit_verified_component_mutation(
        &sch_path,
        &expected,
        &command,
        &intents,
        "group_components",
    )? {
        Ok(observations) => observations,
        Err(error) => return Ok(error),
    };

    let mut grouped = Vec::new();
    let mut components = Vec::new();
    for observed in observations {
        grouped.push(
            observed["reference"]
                .as_str()
                .expect("readback reference is a string")
                .to_owned(),
        );
        components.push(observed);
    }
    let observed_group_name = components[0]["fields"]["Group"].clone();
    let observed_schematic = components[0]["schematic"].clone();

    Ok(CallToolResult::json(&json!({
        "group_name": observed_group_name,
        "grouped_count": grouped.len(),
        "grouped": grouped,
        "components": components,
        "schematic": observed_schematic,
        "errors": errors
    })))
}

pub(super) async fn handle_update_symbols_from_library(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let only: Option<Vec<String>> = args["references"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    let dry_run = args["dry_run"].as_bool().unwrap_or(false);
    let allow_pin_moves = args["allow_pin_moves"].as_bool().unwrap_or(false);

    let (mut content, tree) = read_schematic(&sch_path)?;
    let expected = content.clone();

    let instances = extract_symbol_instances(&tree);
    if let Some(refs) = &only {
        if let Some(missing) = refs
            .iter()
            .find(|r| !instances.iter().any(|i| &i.reference == *r))
        {
            return Ok(CallToolResult::error(format!(
                "Component '{}' not found in {}",
                missing,
                sch_path.display()
            )));
        }
    }

    // One definition serves every instance of a lib_id, so refresh each once.
    let mut lib_ids: Vec<String> = Vec::new();
    // An instance with a (lib_name …) draws from that sheet-local copy, so a
    // missing lib_id copy is not missing for it. An embedded definition serves
    // every instance that draws from it directly, selected or not, so the
    // highest such unit is tracked across the whole sheet.
    let mut selected_direct: Vec<String> = Vec::new();
    let mut highest_direct_unit: Vec<(String, u32)> = Vec::new();
    for inst in instances {
        let direct = inst.lib_symbol_name() == inst.lib_id;
        if direct {
            match highest_direct_unit
                .iter_mut()
                .find(|(id, _)| *id == inst.lib_id)
            {
                Some((_, unit)) => *unit = (*unit).max(inst.unit),
                None => highest_direct_unit.push((inst.lib_id.clone(), inst.unit)),
            }
        }
        if only.as_ref().is_some_and(|r| !r.contains(&inst.reference)) {
            continue;
        }
        if direct && !selected_direct.contains(&inst.lib_id) {
            selected_direct.push(inst.lib_id.clone());
        }
        if !lib_ids.contains(&inst.lib_id) {
            lib_ids.push(inst.lib_id);
        }
    }

    let mut updated = Vec::new();
    let mut added = Vec::new();
    let mut unchanged = Vec::new();
    let mut pins_moved = Vec::new();
    let mut errors = Vec::new();
    let src = match crate::tools::library::KiCadSymbolSource::for_file(&sch_path) {
        Ok(source) => source,
        Err(error) => return Ok(error.into_tool_result()),
    };
    let outcomes = reembed_lib_symbols(&mut content, &lib_ids, allow_pin_moves, &src);
    for (lib_id, outcome) in lib_ids.iter().zip(outcomes) {
        match outcome {
            ReembedOutcome::Updated => updated.push(lib_id.clone()),
            ReembedOutcome::Unchanged => unchanged.push(lib_id.clone()),
            ReembedOutcome::PinsMoved(pins) => pins_moved.push(json!({
                "lib_id": lib_id,
                "pins": pins,
            })),
            ReembedOutcome::Unresolved => errors.push(format!(
                "'{}' no longer resolves in any registered library — the \
                 embedded copy is left as it is",
                lib_id
            )),
            // The pin guard has no old copy to compare against here. KiCad
            // draws such an instance without pins, so embedding orphans
            // nothing, but the new pins may land on wires drawn for another
            // symbol: the hint below says so.
            ReembedOutcome::NotEmbedded => {
                if !selected_direct.contains(lib_id) {
                    errors.push(format!(
                        "'{}' is drawn only from sheet-local copies named by lib_name, \
                         which this tool does not refresh",
                        lib_id
                    ));
                    continue;
                }
                let placed_unit = highest_direct_unit
                    .iter()
                    .find(|(id, _)| id == lib_id)
                    .map_or(1, |&(_, unit)| unit);
                let unit_count = cse::library::symbol_unit_count(lib_id, &src);
                let missing_unit = unit_count.filter(|&unit_count| placed_unit > unit_count);
                if let Some(unit_count) = missing_unit {
                    errors.push(format!(
                        "'{}' was not embedded: placed unit {} does not exist in the \
                         {}-unit library symbol",
                        lib_id, placed_unit, unit_count
                    ));
                } else if crate::tools::ensure_lib_symbol_in_schematic(&mut content, lib_id, &src) {
                    added.push(lib_id.clone());
                } else if unit_count.is_some() {
                    // symbol_unit_count resolves through the same lookup as
                    // the embed, so a known unit count means it resolved.
                    errors.push(format!(
                        "'{}' resolves in its library but could not be placed in the \
                         sheet's lib_symbols; nothing was embedded",
                        lib_id
                    ));
                } else {
                    errors.push(format!(
                        "'{}' has no embedded definition, and nothing was embedded: {}",
                        lib_id,
                        crate::tools::lib_symbol_not_found_message(lib_id, &src)
                    ));
                }
            }
        }
    }

    if (!updated.is_empty() || !added.is_empty()) && !dry_run {
        write_atomic_if_unchanged(&sch_path, &expected, &content)?;
    }

    let mut body = json!({
        "updated": updated,
        "updated_count": updated.len(),
        "added": added,
        "added_count": added.len(),
        "unchanged": unchanged,
        "pins_moved": pins_moved,
        "errors": errors,
        "dry_run": dry_run
    });
    let mut hints = Vec::new();
    if !pins_moved.is_empty() {
        hints.push(
            "Symbols listed in pins_moved were left untouched: the library moved or \
             removed pins, and wires and labels attach at pin coordinates. Pass \
             allow_pin_moves: true to update them anyway, then reconnect.",
        );
    }
    if !added.is_empty() {
        hints.push(
            "Symbols listed in added had no definition on the sheet, so their pins \
             were not checked against the wires and labels already placed around \
             them. Run ERC or check their connections.",
        );
    }
    if !hints.is_empty() {
        body["hint"] = json!(hints.join(" "));
    }
    Ok(CallToolResult::json(&body))
}

/// Put every instance field back on its library anchor.
///
/// `add_schematic_component` places new symbols there already; this repairs
/// sheets written before it did, where every field sat at a fixed ±3.81mm
/// offset regardless of what the symbol's definition asked for (#101).
pub(super) async fn handle_reset_schematic_field_positions(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let only: Option<std::collections::HashSet<String>> = args["references"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    let dry_run = args["dry_run"].as_bool().unwrap_or(false);

    let mut sch = cse::Schematic::load(&sch_path)?;

    // Anchors first: reading them borrows the schematic, mutating the symbols
    // borrows it again, so the lookup cannot be inlined into the loop.
    let lib_ids: Vec<String> = {
        let mut ids: Vec<String> = Vec::new();
        for sym in sch.symbols.iter() {
            if !ids.contains(&sym.lib_id) {
                ids.push(sym.lib_id.clone());
            }
        }
        ids
    };
    let anchors: std::collections::HashMap<String, cse::library::FieldAnchors> = lib_ids
        .into_iter()
        .map(|id| {
            let a = cse::library::field_anchors(&sch, &id);
            (id, a)
        })
        .collect();

    let mut moved = Vec::new();
    let mut unchanged = Vec::new();
    let mut no_anchor = Vec::new();
    let mut no_property = Vec::new();
    let mut missing: Vec<String> = only
        .clone()
        .map(|r| r.into_iter().collect())
        .unwrap_or_default();

    for sym in sch.symbols.iter_mut() {
        let Some(reference) = sym.reference().map(String::from) else {
            continue;
        };
        if only.as_ref().is_some_and(|r| !r.contains(&reference)) {
            continue;
        }
        missing.retain(|r| r != &reference);

        let anchor = anchors.get(&sym.lib_id).copied().unwrap_or_default();
        let mirror = sym.mirror.as_deref().unwrap_or("");
        let t = copperpour_sexp::geometry::PinTransform {
            comp_x: sym.at.x,
            comp_y: sym.at.y,
            rotation_deg: sym.at.rotation.unwrap_or(0.0),
            mirror_x: mirror.contains('x'),
            mirror_y: mirror.contains('y'),
        };

        for (name, anchor) in [
            ("Reference", anchor.reference_at),
            ("Value", anchor.value_at),
        ] {
            let Some(anchor) = anchor else {
                no_anchor.push(format!("{}.{}", reference, name));
                continue;
            };
            let (x, y, rot) = crate::tools::field_at(Some(anchor), (0.0, 0.0, 0.0), t);
            // The library anchors this field but the placed symbol carries no
            // such property. Report it rather than dropping it in silence —
            // an unreported skip reads as "reset" to the caller.
            let Some(prop) = sym.properties.iter_mut().find(|p| p.name == name) else {
                no_property.push(format!("{}.{}", reference, name));
                continue;
            };
            if set_property_at(prop, x, y, rot) {
                moved.push(format!("{}.{}", reference, name));
            } else {
                unchanged.push(format!("{}.{}", reference, name));
            }
        }
    }

    if !moved.is_empty() && !dry_run {
        sch.overwrite()?;
    }
    // `missing` starts life as a HashSet, whose iteration order varies run to
    // run; a caller asking about several unknown references would get them
    // back in a different order each time.
    missing.sort_unstable();

    Ok(CallToolResult::json(&json!({
        "moved": moved,
        "moved_count": moved.len(),
        "unchanged": unchanged,
        "no_library_anchor": no_anchor,
        "no_property": no_property,
        "not_found": missing,
        "dry_run": dry_run
    })))
}

/// Rewrite a property's `(at …)` in place. Returns whether anything changed,
/// so an already-correct field is not reported as moved.
fn set_property_at(prop: &mut cse::types::Property, x: f64, y: f64, rotation: f64) -> bool {
    use cse::sexp::{atom, SexpNode};
    use cse::types::fmt_f64;

    let at = SexpNode::List(vec![
        atom("at"),
        atom(fmt_f64(x)),
        atom(fmt_f64(y)),
        atom(fmt_f64(rotation)),
    ]);
    match prop.sub_nodes.iter_mut().find(|n| n.tag() == Some("at")) {
        Some(existing) => {
            if *existing == at {
                return false;
            }
            *existing = at;
        }
        // A field with no (at) is drawn at the sheet origin — always a move.
        None => prop.sub_nodes.insert(0, at),
    }
    true
}

pub(super) async fn handle_replace_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    let new_lib_id = match require_str(args, "new_lib_id") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let new_unit = match opt_u32(args, "unit") {
        Ok(unit) => unit,
        Err(error) => return Ok(error),
    };

    let mut content = read_consistent(&sch_path)?;
    let expected = content.clone();

    let blocks = find_all_symbol_instance_blocks(&content, &reference);
    if blocks.is_empty() {
        return Ok(CallToolResult::error(component_not_found(
            &sch_path, &reference,
        )));
    }
    if blocks.len() > 1 && new_unit.is_some() {
        return Ok(CallToolResult::error(format!(
            "Component '{}' has {} placed units; the 'unit' override is only \
             unambiguous for a single placement. Omit it to preserve each unit.",
            reference,
            blocks.len()
        )));
    }
    let parsed = parse_sexp(&content)?;
    let current_units: Vec<u32> = extract_symbol_instances(&parsed)
        .into_iter()
        .filter(|instance| instance.reference == reference)
        .map(|instance| instance.unit)
        .collect();

    let src = match crate::tools::library::KiCadSymbolSource::for_file(&sch_path) {
        Ok(source) => source,
        Err(error) => return Ok(error.into_tool_result()),
    };
    // Preserve the ownership preflight as the first semantic gate. Binding the
    // selected instances is still performed before any in-memory mutation,
    // but an ambiguously owned sheet must report that conflict rather than a
    // secondary instance-metadata diagnosis.
    let mut target = match component_target_from_source(&sch_path, &content, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };
    let embedded_unit_count = parsed
        .find("lib_symbols")
        .and_then(|libraries| {
            libraries.find_all("symbol").into_iter().find(|symbol| {
                symbol.get(1).and_then(|value| value.as_str()) == Some(new_lib_id.as_str())
            })
        })
        .map(|symbol| {
            symbol
                .find_all("symbol")
                .into_iter()
                .filter_map(|unit| {
                    unit.get(1)
                        .and_then(|value| value.as_str())
                        .and_then(copperpour_sexp::schematic::parse_subsymbol_unit)
                })
                .max()
                .unwrap_or(1)
                .max(1)
        });
    let unit_count = embedded_unit_count
        .or_else(|| cse::library::symbol_unit_count(&new_lib_id, &src))
        .unwrap_or(1);
    if let Some(unit) = new_unit {
        if unit < 1 || unit > unit_count {
            return Ok(CallToolResult::error(format!(
                "Invalid unit {} for '{}': the symbol has {} unit(s) (valid: 1..={}).",
                unit, new_lib_id, unit_count, unit_count
            )));
        }
    } else if let Some(invalid) = current_units
        .iter()
        .find(|unit| **unit < 1 || **unit > unit_count)
    {
        return Ok(CallToolResult::error(format!(
            "Cannot replace '{}' with '{}': placed unit {} does not exist in the \
             new {}-unit symbol. Delete and re-place the component deliberately.",
            reference, new_lib_id, invalid, unit_count
        )));
    }

    // Replace the library id in every unit block. Shared component identity
    // must not leave one unit pointing at the old symbol (#182).
    let lib_id_pat = "(lib_id \"";
    let escaped_lib_id = escape_property_text(&new_lib_id);
    let mut edits = Vec::new();
    let mut old_lib_ids = Vec::new();
    for (start, end) in &blocks {
        let block = &content[*start..*end];
        let Some(relative) = block.find(lib_id_pat) else {
            return Ok(CallToolResult::error(format!(
                "A unit of '{}' has no lib_id",
                reference
            )));
        };
        let value_start = *start + relative + lib_id_pat.len();
        let Some(value_end) = closing_quote(&content, value_start) else {
            return Ok(CallToolResult::error("Malformed lib_id"));
        };
        old_lib_ids.push(content[value_start..value_end].to_string());
        edits.push(SexpEdit::replace(
            value_start,
            value_end,
            escaped_lib_id.clone(),
        ));
    }

    // Add the optional unit edits without a second source read. The multi-unit
    // guard above means this scan has at most one block.
    if let Some(unit) = new_unit {
        let (start, end) = blocks[0];
        let block = &content[start..end];
        let mut from = 0usize;
        while let Some(relative) = block[from..].find("(unit ") {
            let number_start = from + relative + "(unit ".len();
            let Some(close) = block[number_start..].find(')') else {
                break;
            };
            edits.push(SexpEdit::replace(
                start + number_start,
                start + number_start + close,
                unit.to_string(),
            ));
            from = number_start + close;
        }
    }

    old_lib_ids.sort();
    old_lib_ids.dedup();
    if old_lib_ids.len() != 1 {
        return Ok(CallToolResult::error(format!(
            "Component '{}' already has inconsistent library ids across its units: {}",
            reference,
            old_lib_ids.join(", ")
        )));
    }
    let old_lib_id = old_lib_ids.remove(0);
    content = apply_edits(content, edits);

    // Ensure the new library symbol definition is present. Bail BEFORE writing:
    // a replace that can't embed its definition would leave the component
    // netlist-invisible (#34).
    if !crate::tools::ensure_lib_symbol_in_schematic(&mut content, &new_lib_id, &src) {
        return Ok(crate::tools::lib_symbol_not_found_error(&new_lib_id, &src));
    }

    // A replacement changes pin geometry while preserving only symbol UUID,
    // unit and pin number. Carry a marker through that narrower, explicitly
    // one-to-one identity before the junction pass sees the new endpoints;
    // removed, renumbered or duplicated protected pins refuse before writing.
    let after_tree = parse_sexp(&content)?;
    let carries = match plan_no_connect_carry_for_replacement(&parsed, &after_tree) {
        Ok(carries) => carries,
        Err(error) => return Ok(error.into_result(&sch_path)),
    };
    content = match apply_no_connect_carries(content, &carries) {
        Some(content) => content,
        None => return Ok(no_connect_carry_unwritable(&sch_path)),
    };
    let (content, added, pruned) = reconcile_junctions_for_placement(&expected, content);
    write_atomic_if_unchanged(&sch_path, &expected, &content)?;

    let moved_markers = match observed_no_connect_moves(&sch_path, "replace_component", &carries)? {
        Ok(moved) => moved,
        Err(error) => return Ok(error),
    };
    for unit in &mut target.units {
        unit.lib_id.clone_from(&new_lib_id);
        if let Some(new_unit) = new_unit {
            unit.unit = new_unit;
        }
    }
    let observed = match load_component_mutation_readback(&sch_path, &target) {
        Ok(Ok(observed)) => observed,
        Ok(Err(error)) => {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "replace_component",
                tool_error_summary(&error),
            ));
        }
        Err(error) => {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "replace_component",
                format!("the written schematic cannot be read back: {error}"),
            ));
        }
    };
    let mut result = json!({
        "reference": observed["reference"],
        "old_lib_id": old_lib_id,
        "new_lib_id": observed["lib_id"],
        "unit": new_unit,
        "units_replaced": observed["unit_count"],
        "junctions_added_count": added,
        "junctions_pruned_count": pruned,
        "no_connects_moved_count": moved_markers.len(),
        "no_connects_moved": moved_markers
    });
    copy_component_observation(&mut result, &observed);
    Ok(CallToolResult::json(&result))
}

// Library symbol resolution moved to tools/mod.rs (shared with sch_wiring.rs)
