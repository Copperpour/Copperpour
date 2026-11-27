//! Plan and revise schematic-to-board changes without writing to KiCad.

use super::netlist::{netlist_identity, normalize_symbol_path};
use super::{
    AcknowledgedNetChange, BoardState, Bounds, CopperItem, CopperKind, CopperNetChange,
    ExportedDesign, FootprintWithoutSymbol, NetChange, PlanStatus, PlannedChange, Point,
    PreparedFootprint, PreservedBoardState, ReplacedFootprint, SyncCounts, SyncDiagnostic,
    SyncPlan, UnassignedBoardState, UnassignedFootprint, WithoutSymbolReason,
};
use crate::hex::encode_hex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// [`plan_sync_acknowledging`] with no routed pad net change acknowledged.
#[cfg(test)]
pub(super) fn plan_sync(
    netlist_source: &str,
    design: &ExportedDesign,
    board: &BoardState,
) -> SyncPlan {
    plan_sync_acknowledging(netlist_source, design, board, &BTreeSet::new(), false)
}

/// The plan. A pad net change on a net that routed copper uses is a
/// `routed_pad_net_change` conflict unless `acknowledged` names it exactly;
/// then it is planned as KiCad's Update PCB makes it and noted; the vias and
/// zones of a net that leaves the schematic follow (`plan_copper_nets`). An acknowledgement the plan has no such change for is a
/// conflict, so a stale or widened one refuses (#38). With
/// `delete_without_symbols`, a plan with no conflict deletes each unlocked
/// footprint no symbol matched that lacks the board-only attribute (#861);
/// the footprint of a symbol marked Exclude from board is one of them (#870).
pub(super) fn plan_sync_acknowledging(
    netlist_source: &str,
    design: &ExportedDesign,
    board: &BoardState,
    acknowledged: &BTreeSet<AcknowledgedNetChange>,
    delete_without_symbols: bool,
) -> SyncPlan {
    let mut diagnostics = Vec::new();
    let mut acknowledgements_used = BTreeSet::new();
    // Reported beside the plan without refusing it, as KiCad's Update PCB
    // reports a warning.
    let mut notes = Vec::new();
    // Parts skipped rather than refusing the plan (`SyncPlan::refused_updates`).
    let mut refused_updates = Vec::new();
    let mut counts = SyncCounts::default();
    let mut changes = Vec::new();
    let mut board_by_path = HashMap::new();
    let mut board_by_reference = HashMap::new();

    // Every reference the schematic export names, on either side of the
    // `on_board` flag. A duplicate board reference only matters when this set
    // contains it: `board_by_reference` is consulted at four sites below, and
    // each one looks up a reference that came from the export —
    // `skipped.reference` once and `component.reference` three times. A
    // reference the export never names is therefore never looked up, so which
    // of the duplicates `insert` happened to keep is unobservable.
    //
    // The map is still built from every footprint, duplicates included. Only
    // the diagnostic is scoped; suppressing the insert would change which
    // footprint an unrelated adoption resolves to.
    let exported_references = design
        .components
        .iter()
        .map(|component| component.reference.as_str())
        .chain(
            design
                .skipped
                .iter()
                .map(|skipped| skipped.reference.as_str()),
        )
        .chain(
            design
                .unassigned
                .iter()
                .map(|unassigned| unassigned.reference.as_str()),
        )
        .collect::<HashSet<_>>();

    let root_uuid = design.root_sheet_uuid.as_deref();
    for (index, footprint) in board.footprints.iter().enumerate() {
        let duplicate_reference = board_by_reference
            .insert(footprint.reference.as_str(), index)
            .is_some();
        if duplicate_reference && exported_references.contains(footprint.reference.as_str()) {
            diagnostics.push(conflict(
                "duplicate_board_reference",
                format!("board contains duplicate reference {}", footprint.reference),
                Some(&footprint.reference),
            ));
        }
        if let Some(path) = footprint.symbol_path.as_deref() {
            let normalized = normalize_symbol_path(path, root_uuid);
            if board_by_path.insert(normalized, index).is_some() {
                diagnostics.push(conflict(
                    "duplicate_board_identity",
                    format!("board contains duplicate schematic identity {normalized}"),
                    Some(&footprint.reference),
                ));
            }
        }
    }

    let mut matched = std::collections::HashSet::new();
    let mut design_references = std::collections::HashSet::new();
    let mut design_paths = std::collections::HashSet::new();
    let staging_x = board.bounds.max_x + 10.0;
    let mut add_index = 0usize;

    let mut skipped_references = HashSet::new();
    let mut skipped_paths = HashSet::new();
    // The footprints of `on_board=no` instances. KiCad's Update PCB skips a
    // symbol marked Exclude from board, and its pass over unmatched
    // footprints counts no match for that symbol's footprint, so it is one
    // with no symbol: kept, or deleted with "Delete footprints with no
    // symbols" (`BOARD_NETLIST_UPDATER::UpdateNetlist`,
    // board_netlist_updater.cpp:2092 and :2350, KiCad 10.0.8; #870). They
    // stay out of `matched` for that pass below, and out of the rename
    // candidates, since their symbol still claims them.
    let mut excluded = HashSet::new();
    for skipped in &design.skipped {
        let skipped_path = normalize_symbol_path(&skipped.symbol_path, root_uuid);
        if !skipped_references.insert(skipped.reference.as_str())
            || !skipped_paths.insert(skipped_path)
        {
            diagnostics.push(conflict(
                "duplicate_skipped_identity",
                format!(
                    "on_board=no instance {} has a duplicate reference or identity",
                    skipped.reference
                ),
                Some(&skipped.reference),
            ));
            continue;
        }
        counts.skipped_by_flag.planned += 1;
        // Matched as a component is: by identity, or by reference when the
        // footprint has none.
        let existing = board_by_path.get(skipped_path).copied().or_else(|| {
            board_by_reference
                .get(skipped.reference.as_str())
                .copied()
                .filter(|index| board.footprints[*index].symbol_path.is_none())
        });
        if let Some(index) = existing {
            excluded.insert(index);
        }
    }

    // No footprint assigned: nothing can be placed, so the part is reported
    // with what the board holds for it and the sync goes on for everything
    // else — eeschema's own Update PCB behaviour. Never a diagnostic: a
    // diagnostic clears the whole plan, which is exactly what #507 fixes.
    let mut unassigned = Vec::new();
    for component in &design.unassigned {
        counts.unassigned_footprint.planned += 1;
        let unassigned_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let existing = board_by_path.get(unassigned_path).copied().or_else(|| {
            board_by_reference
                .get(component.reference.as_str())
                .copied()
        });
        let board_state = match existing {
            Some(index) => {
                // Left untouched, as KiCad leaves it; counted as matched so
                // it is not reported as a board-only footprint.
                matched.insert(index);
                UnassignedBoardState::Kept
            }
            None => UnassignedBoardState::Absent,
        };
        unassigned.push(UnassignedFootprint {
            reference: component.reference.clone(),
            value: component.value.clone(),
            lib_id: component.lib_id.clone(),
            symbol_path: component.symbol_path.clone(),
            board_state,
        });
    }

    for component in &design.components {
        let normalized_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        if !design_references.insert(component.reference.as_str()) {
            diagnostics.push(conflict(
                "duplicate_schematic_reference",
                format!(
                    "schematic export contains duplicate reference {}",
                    component.reference
                ),
                Some(&component.reference),
            ));
            continue;
        }
        if !design_paths.insert(normalized_path) {
            diagnostics.push(conflict(
                "duplicate_schematic_identity",
                format!(
                    "schematic export contains duplicate identity {}",
                    component.symbol_path
                ),
                Some(&component.reference),
            ));
            continue;
        }

        // A footprint linked to any unit of the symbol is its footprint, as
        // KiCad's Update PCB matches it against the sheet path and each of
        // the component's unit UUIDs (`BOARD_NETLIST_UPDATER::UpdateNetlist`,
        // KiCad 10.0.8). KiCad then relinks it to the first unit; the sync
        // keeps its link, as it keeps every matched footprint's.
        let unit_matches = component
            .unit_paths()
            .filter_map(|path| {
                board_by_path
                    .get(normalize_symbol_path(path, root_uuid))
                    .copied()
            })
            .collect::<BTreeSet<_>>();
        if unit_matches.len() > 1 {
            matched.extend(unit_matches.iter().copied());
            diagnostics.push(conflict(
                "duplicate_board_identity",
                format!(
                    "board footprints {} are each linked to a unit of {}",
                    unit_matches
                        .iter()
                        .map(|index| board.footprints[*index].reference.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    component.reference
                ),
                Some(&component.reference),
            ));
            continue;
        }
        let matched_index = unit_matches.first().copied().or_else(|| {
            board_by_reference
                .get(component.reference.as_str())
                .copied()
                .filter(|index| board.footprints[*index].symbol_path.is_none())
        });

        let Some(index) = matched_index else {
            if let Some(index) = board_by_reference
                .get(component.reference.as_str())
                .copied()
            {
                diagnostics.push(conflict(
                    "reference_identity_conflict",
                    format!(
                        "reference {} belongs to a different schematic identity on the board",
                        component.reference
                    ),
                    Some(&board.footprints[index].reference),
                ));
                continue;
            }
            let possible_renames = board
                .footprints
                .iter()
                .enumerate()
                .filter(|(index, footprint)| {
                    !matched.contains(index)
                        && !excluded.contains(index)
                        && footprint.symbol_path.is_none()
                        && !footprint.not_in_schematic
                        && footprint.footprint_id == component.footprint_id
                        && footprint.value == component.value
                })
                .collect::<Vec<_>>();
            if !possible_renames.is_empty() {
                diagnostics.push(conflict(
                    "reference_only_rename_ambiguous",
                    format!(
                        "{} has no stable board identity and could be a rename of {}; link or resolve the identity in KiCad",
                        component.reference,
                        possible_renames
                            .iter()
                            .map(|(_, footprint)| footprint.reference.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some(&component.reference),
                ));
                continue;
            }
            let position = Point {
                x: staging_x,
                y: board.bounds.min_y + add_index as f64 * 10.0,
            };
            add_index += 1;
            changes.push(PlannedChange::Add {
                reference: component.reference.clone(),
                value: component.value.clone(),
                footprint_id: component.footprint_id.clone(),
                symbol_path: component.symbol_path.clone(),
                dnp: component.dnp,
                exclude_from_bom: component.exclude_from_bom,
                pad_nets: component.pad_nets.clone(),
                unconnected_pins: component.unconnected_pins.clone(),
                position,
                fields: component.fields.clone(),
                sheet: component.sheet.clone(),
            });
            counts.added.planned += 1;
            continue;
        };

        matched.insert(index);
        let footprint = &board.footprints[index];
        if footprint.reference != component.reference {
            if let Some(other_index) = board_by_reference
                .get(component.reference.as_str())
                .copied()
                .filter(|other_index| *other_index != index)
            {
                diagnostics.push(conflict(
                    "reference_rename_collision",
                    format!(
                        "cannot rename {} to {} because that reference belongs to board footprint {}",
                        footprint.reference,
                        component.reference,
                        board.footprints[other_index].kiid
                    ),
                    Some(&component.reference),
                ));
                continue;
            }
        }
        // A different footprint in the schematic is replaced, as KiCad's
        // Update PCB replaces it (`BOARD_NETLIST_UPDATER::replaceFootprint`).
        // Whether the new footprint can be built, and has every pad the
        // schematic connects, is checked against its library in the
        // handler's preflight (`prepare_replacements`). KiCad leaves a locked
        // footprint as it is, with a warning, and still updates the rest of
        // it; so does this.
        let replace_footprint = if footprint.footprint_id == component.footprint_id {
            None
        } else if footprint.locked {
            notes.push(conflict(
                "footprint_locked_not_replaced",
                format!(
                    "{} is locked, so it keeps {} though the schematic names {}; unlock it and \
                     sync again to replace it",
                    component.reference, footprint.footprint_id, component.footprint_id
                ),
                Some(&component.reference),
            ));
            None
        } else {
            Some(ReplacedFootprint {
                from: footprint.footprint_id.clone(),
                to: component.footprint_id.clone(),
            })
        };
        // A pin KiCad left unconnected needs no pad on the footprint kept;
        // a replacement is given its pads in the handler, from its library
        // footprint (`leave_out_unconnected_pins`).
        let pad_nets = if replace_footprint.is_none() {
            nets_for_pads(
                &component.pad_nets,
                &component.unconnected_pins,
                &footprint.pad_numbers,
            )
        } else {
            component.pad_nets.clone()
        };
        // A pad the schematic connects and the live footprint lacks used to
        // reach `apply_footprint_fields` and fail the apply, after a dry run
        // that said `ready` (#657). The part is skipped instead, as an added
        // part is that lacks one, and the rest is still synced. A replacement
        // is checked against its new footprint.
        let missing = missing_pads(&pad_nets, &footprint.pad_numbers);
        if replace_footprint.is_none() && !missing.is_empty() {
            refused_updates.push(pad_missing_conflict(
                &component.reference,
                &footprint.footprint_id,
                &missing,
            ));
            continue;
        }

        let mut changed_pads = 0usize;
        let pad_numbers = pad_nets
            .keys()
            .chain(footprint.pad_nets.keys())
            .collect::<std::collections::BTreeSet<_>>();
        for number in pad_numbers {
            let new_net = pad_nets.get(number).map(String::as_str).unwrap_or("");
            let old_net = footprint
                .pad_nets
                .get(number)
                .map(String::as_str)
                .unwrap_or("");
            if old_net == new_net {
                continue;
            }
            if board.routed_nets.contains_key(old_net) || board.routed_nets.contains_key(new_net) {
                let change = NetChange {
                    pad: number.clone(),
                    from: old_net.to_string(),
                    to: new_net.to_string(),
                };
                let acknowledgement = AcknowledgedNetChange {
                    reference: component.reference.clone(),
                    pad: change.pad.clone(),
                    from: change.from.clone(),
                    to: change.to.clone(),
                };
                if acknowledged.contains(&acknowledgement) {
                    changed_pads += 1;
                    let mut note = conflict(
                        "routed_pad_net_change_acknowledged",
                        format!(
                            "{} pad {} changes from '{}' to '{}' as acknowledged; KiCad's commit \
                             carries the tracks joined to it, the vias and zones of a net that \
                             leaves the schematic are moved as copper_net_changes, and the apply \
                             reports the routed copper on both nets and runs DRC",
                            component.reference, number, old_net, new_net
                        ),
                        Some(&component.reference),
                    );
                    note.net_change = Some(change);
                    notes.push(note);
                    acknowledgements_used.insert(acknowledgement);
                } else {
                    let mut diagnostic = conflict(
                        "routed_pad_net_change",
                        format!(
                            "{} pad {} would change from '{}' to '{}' while routed copper uses that net",
                            component.reference, number, old_net, new_net
                        ),
                        Some(&component.reference),
                    );
                    diagnostic.net_change = Some(change);
                    diagnostics.push(diagnostic);
                }
            } else {
                changed_pads += 1;
            }
        }

        // KiCad's Update PCB updates a footprint whose fields differ from the
        // symbol's and leaves fields the symbol lacks in place (#788). A
        // replacement takes the new footprint's fields, so it gets every one
        // of the symbol's again.
        let changed_fields = component
            .fields
            .iter()
            .filter(|field| {
                replace_footprint.is_some()
                    || footprint.fields.get(&field.name) != Some(&field.value)
            })
            .cloned()
            .collect::<Vec<_>>();
        let preserved_symbol_path = footprint
            .symbol_path
            .as_deref()
            .unwrap_or(component.symbol_path.as_str());
        let differs_beyond_sheet = replace_footprint.is_some()
            || footprint.reference != component.reference
            || footprint.value != component.value
            || footprint.symbol_path.is_none()
            || footprint.dnp != component.dnp
            || footprint.exclude_from_bom != component.exclude_from_bom
            || changed_pads > 0
            || !changed_fields.is_empty();
        if differs_beyond_sheet || footprint.sheet != component.sheet {
            changes.push(PlannedChange::Update {
                kiid: footprint.kiid.clone(),
                reference: component.reference.clone(),
                value: component.value.clone(),
                symbol_path: preserved_symbol_path.to_string(),
                dnp: component.dnp,
                exclude_from_bom: component.exclude_from_bom,
                pad_nets,
                unconnected_pins: component.unconnected_pins.clone(),
                preserve: PreservedBoardState {
                    position: footprint.position,
                    rotation: footprint.rotation,
                    layer: footprint.layer.clone(),
                    locked: footprint.locked,
                },
                fields: changed_fields,
                sheet: component.sheet.clone(),
                sheet_only: !differs_beyond_sheet,
                replace_footprint,
            });
            counts.updated.planned += 1;
            counts.pads_reassigned.planned += changed_pads;
        }
    }

    for unused in acknowledged.difference(&acknowledgements_used) {
        diagnostics.push(conflict(
            "net_change_acknowledgement_unmatched",
            format!(
                "{} pad {} from '{}' to '{}' is acknowledged, but this plan makes no such routed \
                 pad net change; dry run again and acknowledge what it reports",
                unused.reference, unused.pad, unused.from, unused.to
            ),
            Some(&unused.reference),
        ));
    }

    counts.conflicts.planned = diagnostics.len();
    if !diagnostics.is_empty() {
        changes.clear();
        counts.added.planned = 0;
        counts.updated.planned = 0;
        counts.pads_reassigned.planned = 0;
    }
    // The footprints no symbol matched. KiCad's Update PCB with "Delete
    // footprints with no symbols" removes each of them in the same commit,
    // never one with the board-only attribute (`FP_BOARD_ONLY`), and a locked
    // one only when locks are overridden, which the sync does not offer: it
    // keeps it with a warning (`BOARD_NETLIST_UPDATER::UpdateNetlist`, KiCad
    // 10.0.8). A plan with a conflict deletes nothing (#861). The footprint of
    // a symbol marked Exclude from board is handled the same way (#870).
    let deleting = delete_without_symbols && diagnostics.is_empty();
    let mut without_symbols = Vec::new();
    let mut unmatched = 0usize;
    for (index, footprint) in board.footprints.iter().enumerate() {
        if matched.contains(&index) {
            continue;
        }
        unmatched += 1;
        if footprint.not_in_schematic {
            continue;
        }
        let reason = if excluded.contains(&index) {
            WithoutSymbolReason::ExcludedFromBoard
        } else {
            WithoutSymbolReason::SymbolRemoved
        };
        if deleting && !footprint.locked {
            changes.push(PlannedChange::Delete {
                kiid: footprint.kiid.clone(),
                reference: footprint.reference.clone(),
                footprint_id: footprint.footprint_id.clone(),
                reason,
            });
            counts.deleted.planned += 1;
            continue;
        }
        if deleting {
            notes.push(conflict(
                "footprint_locked_not_deleted",
                format!(
                    "{} {}, but it is locked, so it is kept; unlock it and sync again to \
                     delete it",
                    footprint.reference,
                    reason.describe()
                ),
                Some(&footprint.reference),
            ));
        }
        without_symbols.push(FootprintWithoutSymbol {
            reference: footprint.reference.clone(),
            kiid: footprint.kiid.clone(),
            footprint_id: footprint.footprint_id.clone(),
            locked: footprint.locked,
            reason,
        });
    }
    counts.board_only_preserved.planned = unmatched - counts.deleted.planned;
    let status = if !diagnostics.is_empty() {
        PlanStatus::Conflict
    } else if changes.is_empty() {
        PlanStatus::Noop
    } else {
        PlanStatus::Ready
    };
    diagnostics.extend(notes);
    let plan_revision = plan_revision(netlist_source, board);
    SyncPlan {
        status,
        plan_revision,
        counts,
        changes,
        diagnostics,
        unassigned,
        // Set by the handler's preflight, which reads the library.
        skipped: Vec::new(),
        refused_updates,
        without_symbols,
        // Set by the handler, from the vias and zones the snapshot read.
        copper_net_changes: Vec::new(),
        // Set by the handler from the snapshot this plan was made against.
        staging: None,
        acknowledged_apply: None,
    }
}

pub(super) fn conflict(code: &str, message: String, reference: Option<&str>) -> SyncDiagnostic {
    SyncDiagnostic {
        code: code.to_string(),
        message,
        reference: reference.map(str::to_string),
        references: reference.map(str::to_string).into_iter().collect(),
        footprint_id: None,
        net_change: None,
        kiid: None,
    }
}

/// A diagnostic about one library footprint and every part that uses it.
/// `reference` keeps its single-part meaning, so it is set only when exactly
/// one part is concerned.
pub(super) fn footprint_conflict(
    code: &str,
    message: String,
    footprint_id: &str,
    references: Vec<String>,
) -> SyncDiagnostic {
    SyncDiagnostic {
        code: code.to_string(),
        message,
        reference: match references.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
        references,
        footprint_id: Some(footprint_id.to_string()),
        net_change: None,
        kiid: None,
    }
}

/// References for a message, bounded: a common footprint can block hundreds
/// of parts, and the complete list is in the diagnostic's `references`.
pub(super) fn listed(references: &[String]) -> String {
    const SHOWN: usize = 8;
    let shown = references
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match references.len().saturating_sub(SHOWN) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// The schematic connects a pad the footprint does not have. `missing` is
/// never empty.
pub(super) fn pad_missing_conflict(
    reference: &str,
    footprint_id: &str,
    missing: &[&str],
) -> SyncDiagnostic {
    let pads = if missing.len() == 1 { "pad" } else { "pads" };
    footprint_conflict(
        "footprint_pad_missing",
        format!(
            "the schematic connects {reference} {pads} {}, which footprint {footprint_id} does not have",
            missing.join(", ")
        ),
        footprint_id,
        vec![reference.to_string()],
    )
}

/// Pad numbers the schematic connects that `available` does not contain.
pub(super) fn missing_pads<'a>(
    pad_nets: &'a BTreeMap<String, String>,
    available: &BTreeSet<String>,
) -> Vec<&'a str> {
    pad_nets
        .keys()
        .filter(|number| !available.contains(*number))
        .map(String::as_str)
        .collect()
}

/// The pad nets a footprint with the pads `available` takes: every pin's but
/// those of the pins KiCad left unconnected that it has no pad for. Such a
/// pin connects nothing, so the footprint of a keyed header, which has no pad
/// at the key, is synced, and its pad nets name only pads it has. KiCad's
/// Update PCB likewise sets the nets of the pads a footprint has
/// (`updateComponentPadConnections`, KiCad 10.0.8).
pub(super) fn nets_for_pads(
    pad_nets: &BTreeMap<String, String>,
    unconnected_pins: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    pad_nets
        .iter()
        .filter(|(number, _)| available.contains(*number) || !unconnected_pins.contains(*number))
        .map(|(number, net)| (number.clone(), net.clone()))
        .collect()
}

pub(super) fn plan_revision(netlist_source: &str, board: &BoardState) -> String {
    let mut footprints = board.footprints.iter().collect::<Vec<_>>();
    footprints.sort_by(|a, b| a.kiid.cmp(&b.kiid));
    let mut hasher = Sha256::new();
    hasher.update(netlist_identity(netlist_source));
    hasher.update(serde_json::to_vec(&board.bounds).expect("bounds serialize"));
    for footprint in footprints {
        hasher.update(footprint.kiid.as_bytes());
        hasher.update(footprint.reference.as_bytes());
        hasher.update(footprint.footprint_id.as_bytes());
        hasher.update(footprint.symbol_path.as_deref().unwrap_or("").as_bytes());
        for (pad, net) in &footprint.pad_nets {
            hasher.update(pad.as_bytes());
            hasher.update(net.as_bytes());
        }
    }
    for (net, count) in &board.routed_nets {
        hasher.update(net.as_bytes());
        hasher.update(count.to_le_bytes());
    }
    encode_hex(&hasher.finalize())
}

pub(super) fn refresh_revision_with_staging(plan: &mut SyncPlan) {
    let mut hasher = Sha256::new();
    hasher.update(plan.plan_revision.as_bytes());
    hasher.update(serde_json::to_vec(&plan.changes).expect("planned changes serialize"));
    // What was left out is part of what was reviewed.
    hasher.update(serde_json::to_vec(&plan.skipped).expect("skipped parts serialize"));
    hasher.update(
        serde_json::to_vec(&plan.copper_net_changes).expect("copper net changes serialize"),
    );
    plan.plan_revision = encode_hex(&hasher.finalize());
}

/// Move the vias and copper zones whose net leaves the schematic as KiCad's
/// Update PCB moves them, and note each one it leaves (#862).
///
/// KiCad 10.0.8, `BOARD_NETLIST_UPDATER` (`board_netlist_updater.cpp`):
/// `updateComponentPadConnections` records each pad's old net and new net in
/// `m_oldToNewNets` when both are non-empty, the last pad winning (`:1202`).
/// `updateCopperZoneNets` (`:1676`) then looks at every via and copper zone
/// whose net no schematic pin carries. A via takes the new net the map gives
/// its old one (`:1703`), else KiCad warns "Via connected to unknown net". A
/// zone first takes the new net of a pad connected to it before the update
/// whose net changes (`cacheCopperZoneConnections`, `:1664`), then the map
/// (`:1796`), else KiCad warns that it "has no pads connected". A zone's net
/// counts as in the schematic when it starts with a schematic net's name, as
/// KiCad's `isInNetlist` reads pseudo net names.
///
/// The map is built from the pad changes still planned, so a part the
/// preflight skipped moves nothing. `zone_pads` holds the pads connected to
/// each zone, as `(footprint kiid, pad number)`, in the order to try them.
/// An item on no net is left alone without a note: there is nothing to move.
/// A plan with a conflict moves nothing.
pub(super) fn plan_copper_nets(
    plan: &mut SyncPlan,
    board: &BoardState,
    copper: &[CopperItem],
    net_names: &BTreeSet<String>,
    zone_pads: &BTreeMap<String, Vec<(String, String)>>,
) {
    if plan.status == PlanStatus::Conflict {
        return;
    }
    let mut old_to_new = BTreeMap::<&str, &str>::new();
    let mut new_nets = BTreeMap::<(&str, &str), &str>::new();
    for change in &plan.changes {
        let PlannedChange::Update { kiid, pad_nets, .. } = change else {
            continue;
        };
        let Some(footprint) = board
            .footprints
            .iter()
            .find(|footprint| &footprint.kiid == kiid)
        else {
            continue;
        };
        for number in &footprint.pad_numbers {
            let old = footprint.pad_nets.get(number).map_or("", String::as_str);
            let new = pad_nets.get(number).map_or("", String::as_str);
            new_nets.insert((kiid.as_str(), number.as_str()), new);
            // `old != new` as KiCad's `if( pad->GetNetname() != netName )`
            // (`:1173`); a schematic net in the map would move nothing anyway,
            // as copper on schematic nets is skipped.
            if !old.is_empty() && !new.is_empty() && old != new {
                old_to_new.insert(old, new);
            }
        }
    }
    let net_after = |footprint_kiid: &str, number: &str| -> String {
        if let Some(net) = new_nets.get(&(footprint_kiid, number)) {
            return net.to_string();
        }
        board
            .footprints
            .iter()
            .find(|footprint| footprint.kiid == footprint_kiid)
            .and_then(|footprint| footprint.pad_nets.get(number))
            .cloned()
            .unwrap_or_default()
    };
    for item in copper {
        if item.net.is_empty() {
            continue;
        }
        let in_schematic = match item.kind {
            CopperKind::Via => net_names.contains(&item.net),
            CopperKind::Zone => net_names
                .iter()
                .any(|name| !name.is_empty() && item.net.starts_with(name.as_str())),
        };
        if in_schematic {
            continue;
        }
        let mut to = String::new();
        if item.kind == CopperKind::Zone {
            if let Some(net) = zone_pads
                .get(&item.kiid)
                .into_iter()
                .flatten()
                .map(|(footprint, number)| net_after(footprint, number))
                .find(|net| *net != item.net)
            {
                to = net;
            }
        }
        if to.is_empty() {
            if let Some(net) = old_to_new.get(item.net.as_str()) {
                to = net.to_string();
            }
        }
        if !to.is_empty() {
            plan.copper_net_changes.push(CopperNetChange {
                kind: item.kind,
                kiid: item.kiid.clone(),
                name: item.name.clone(),
                from: item.net.clone(),
                to,
            });
            plan.counts.copper_net_changes.planned += 1;
            continue;
        }
        let (code, message) = match item.kind {
            CopperKind::Via => (
                "via_net_not_in_schematic",
                format!(
                    "via {} is on '{}', which is not in the schematic, and no planned pad moves \
                     off it, so it stays; KiCad's Update PCB warns \"Via connected to unknown \
                     net\"",
                    item.kiid, item.net
                ),
            ),
            CopperKind::Zone => (
                "zone_net_not_in_schematic",
                format!(
                    "copper zone {}({}) is on '{}', which is not in the schematic, and no pad \
                     connected to it or moving off its net takes another, so it stays; KiCad's \
                     Update PCB warns that it has no pads connected",
                    if item.name.is_empty() {
                        String::new()
                    } else {
                        format!("'{}' ", item.name)
                    },
                    item.kiid,
                    item.net
                ),
            ),
        };
        let mut note = conflict(code, message, None);
        note.kiid = Some(item.kiid.clone());
        plan.diagnostics.push(note);
    }
}

/// Remove the updates identified by
/// [`updates_not_carried`](super::live_board::updates_not_carried) and keep their
/// diagnostics. A plan left with no change is a no-op.
pub(super) fn leave_out_updates(plan: &mut SyncPlan, left_out: Vec<(String, SyncDiagnostic)>) {
    for (kiid, diagnostic) in left_out {
        let planned = plan.changes.len();
        plan.changes.retain(
            |change| !matches!(change, PlannedChange::Update { kiid: planned, .. } if *planned == kiid),
        );
        // `skip_parts` may have taken it off already.
        if plan.changes.len() < planned {
            plan.counts.updated.planned -= 1;
            plan.diagnostics.push(diagnostic);
        }
    }
    if plan.changes.is_empty() && plan.status == PlanStatus::Ready {
        plan.status = PlanStatus::Noop;
    }
}

/// Take the parts these preflight diagnostics name off the plan and report
/// them in `skipped`, so one footprint that cannot be built or updated no
/// longer stops the sync of every other part. KiCad's Update PCB does the
/// same: a part it cannot add or replace is reported as an error and the rest
/// is committed (`BOARD_NETLIST_UPDATER`, KiCad 10.0.8).
///
/// Only the named parts are left out: a net a skipped part shares still
/// reaches every other part's pads. The plan is refused whole instead, as
/// before, and `false` returned, when a diagnostic names no part, or when
/// nothing but the skipped parts and the updates in `left_out` (which
/// `leave_out_updates` takes off next) would be applied: a no-op would hide
/// that the schematic asked for changes.
pub(super) fn skip_parts(
    plan: &mut SyncPlan,
    diagnostics: Vec<SyncDiagnostic>,
    board: &BoardState,
    left_out: &BTreeSet<&str>,
) -> bool {
    let skipped: BTreeSet<String> = diagnostics
        .iter()
        .flat_map(|diagnostic| diagnostic.references.iter().cloned())
        .collect();
    let applies_something = plan.changes.iter().any(|change| match change {
        PlannedChange::Add { reference, .. } => !skipped.contains(reference),
        PlannedChange::Update {
            kiid, reference, ..
        } => !skipped.contains(reference) && !left_out.contains(kiid.as_str()),
        // A footprint with no symbol is no part the preflight skips (#861).
        PlannedChange::Delete { .. } => true,
    });
    let names_its_parts = diagnostics
        .iter()
        .all(|diagnostic| !diagnostic.references.is_empty());
    if !applies_something || !names_its_parts {
        refuse_plan(plan, diagnostics);
        return false;
    }
    plan.changes.retain(|change| match change {
        PlannedChange::Add { reference, .. } | PlannedChange::Update { reference, .. } => {
            !skipped.contains(reference)
        }
        PlannedChange::Delete { .. } => true,
    });
    // A note about a skipped part (a locked footprint kept, say) no longer
    // describes anything this sync does.
    plan.diagnostics.retain(|note| {
        note.references.is_empty()
            || !note
                .references
                .iter()
                .all(|reference| skipped.contains(reference))
    });
    plan.counts.added.planned = 0;
    plan.counts.updated.planned = 0;
    plan.counts.pads_reassigned.planned = 0;
    for change in &plan.changes {
        match change {
            PlannedChange::Add { .. } => plan.counts.added.planned += 1,
            PlannedChange::Update { kiid, pad_nets, .. } => {
                plan.counts.updated.planned += 1;
                plan.counts.pads_reassigned.planned += board
                    .footprints
                    .iter()
                    .find(|footprint| &footprint.kiid == kiid)
                    .map_or(0, |footprint| {
                        changed_pad_count(pad_nets, &footprint.pad_nets)
                    });
            }
            PlannedChange::Delete { .. } => {}
        }
    }
    plan.counts.skipped_footprint.planned += skipped.len();
    plan.skipped.extend(diagnostics);
    true
}

/// How many pads change net, as `plan_sync` counts them for an update,
/// including a routed change the caller acknowledged (#38).
pub(super) fn changed_pad_count(
    new: &BTreeMap<String, String>,
    old: &BTreeMap<String, String>,
) -> usize {
    new.keys()
        .chain(old.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|number| {
            new.get(*number).map_or("", String::as_str)
                != old.get(*number).map_or("", String::as_str)
        })
        .count()
}

/// Turn a plan into a conflict the way `plan_sync` does for its own
/// diagnostics: nothing stays planned, so nothing can be applied.
pub(super) fn refuse_plan(plan: &mut SyncPlan, diagnostics: Vec<SyncDiagnostic>) {
    plan.status = PlanStatus::Conflict;
    plan.counts.added.planned = 0;
    plan.counts.updated.planned = 0;
    plan.counts.pads_reassigned.planned = 0;
    plan.counts.conflicts.planned += diagnostics.len();
    plan.diagnostics.extend(diagnostics);
    clear_changes(plan);
}

/// Take every change off a refused plan. A footprint it would have deleted
/// stays on the board, so it is listed and counted with those kept (#861).
pub(super) fn clear_changes(plan: &mut SyncPlan) {
    for change in plan.changes.drain(..) {
        if let PlannedChange::Delete {
            kiid,
            reference,
            footprint_id,
            reason,
        } = change
        {
            plan.without_symbols.push(FootprintWithoutSymbol {
                reference,
                kiid,
                footprint_id,
                // Only an unlocked footprint is planned for deletion.
                locked: false,
                reason,
            });
            plan.counts.board_only_preserved.planned += 1;
        }
    }
    plan.counts.deleted.planned = 0;
    plan.copper_net_changes.clear();
    plan.counts.copper_net_changes.planned = 0;
}

/// The footprints the plan deletes, as the list of kept footprints would name
/// them: only an unlocked footprint is planned for deletion.
pub(super) fn planned_deletions(plan: &SyncPlan) -> Vec<FootprintWithoutSymbol> {
    plan.changes
        .iter()
        .filter_map(|change| match change {
            PlannedChange::Delete {
                kiid,
                reference,
                footprint_id,
                reason,
            } => Some(FootprintWithoutSymbol {
                reference: reference.clone(),
                kiid: kiid.clone(),
                footprint_id: footprint_id.clone(),
                locked: false,
                reason: *reason,
            }),
            _ => None,
        })
        .collect()
}

pub(super) fn restage_additions(
    plan: &mut SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    bounds: Bounds,
) {
    let mut next_y = bounds.min_y;
    for change in &mut plan.changes {
        let PlannedChange::Add {
            footprint_id,
            position,
            ..
        } = change
        else {
            continue;
        };
        let dimensions = prepared.get(footprint_id);
        let width = dimensions.map(|part| part.width).unwrap_or(10.0);
        let height = dimensions.map(|part| part.height).unwrap_or(10.0);
        *position = Point {
            x: bounds.max_x + 5.0 + width / 2.0,
            y: next_y + height / 2.0,
        };
        next_y += height + 5.0;
    }
}
