//! In-place edit of a placed footprint's reference (for board-owned
//! footprints), value, assembly flags (do not populate, exclude from BOM and
//! exclude from position files) and lock.
//!
//! The contract is the one [`super::text_edit`] follows for fields: read the
//! exact footprint, change only the requested properties on the message KiCad
//! sent, send it in one KiCad undo step, then prove the result with a fresh
//! read by KIID rather than echoing the request.

use super::text_edit::{footprint_kiid, footprint_mismatches, settle, Settled};
use super::*;
use crate::gen::kiapi::board::types as board;
use crate::gen::kiapi::common::types as common;

const FOOTPRINT_INSTANCE: &str = "kiapi.board.types.FootprintInstance";

/// The schematic symbol path of `footprint`, or `None` when it is not linked
/// to a schematic symbol.
///
/// KiCad's IPC layer sends a present-but-empty `SheetPath` (or all-empty
/// KIIDs) for a footprint placed directly on the board (a logo, fiducial or
/// mounting hole). Only a path with at least one non-empty KIID binds the
/// footprint to a schematic symbol.
fn linked_symbol_path(footprint: &board::FootprintInstance) -> Option<String> {
    let path = footprint.symbol_path.as_ref()?;
    if path.path.iter().all(|part| part.value.is_empty()) {
        return None;
    }
    Some(format!(
        "/{}",
        path.path
            .iter()
            .map(|part| part.value.as_str())
            .collect::<Vec<_>>()
            .join("/")
    ))
}

/// The identity, value, assembly flags and lock of `footprint`, from its
/// instance, which is where KiCad reads them.
pub(super) fn footprint_state(footprint: &board::FootprintInstance) -> IpcFootprintState {
    let attributes = footprint.attributes.clone().unwrap_or_default();
    let field_text = |field: &Option<board::Field>| {
        field
            .as_ref()
            .and_then(|field| field.text.as_ref())
            .and_then(|text| text.text.as_ref())
            .map(|text| text.text.clone())
            .unwrap_or_default()
    };
    IpcFootprintState {
        kiid: footprint_kiid(footprint),
        reference: field_text(&footprint.reference_field),
        value: field_text(&footprint.value_field),
        // As `list_footprints` writes it, which `edit_component` answered
        // with before.
        footprint: footprint
            .definition
            .as_ref()
            .and_then(|definition| definition.id.as_ref())
            .map(|id| format!("{}:{}", id.library_nickname, id.entry_name))
            .unwrap_or_default(),
        dnp: attributes.do_not_populate,
        exclude_from_bom: attributes.exclude_from_bill_of_materials,
        exclude_from_pos_files: attributes.exclude_from_position_files,
        locked: footprint.locked == common::LockedState::LsLocked as i32,
        linked_to_schematic: linked_symbol_path(footprint).is_some(),
    }
}

/// Select the one target footprint from `inventory`, pinning by `kiid` when
/// provided or requiring `reference` to be unique otherwise.
enum SelectedTarget<'a> {
    Found(&'a board::FootprintInstance),
    StaleTarget {
        target: String,
        reason: String,
    },
    Ambiguous {
        target: String,
        candidates: Vec<String>,
        reason: String,
    },
}

fn select_footprint_target<'a>(
    inventory: &'a [board::FootprintInstance],
    reference: &str,
    kiid: Option<&str>,
) -> Result<SelectedTarget<'a>> {
    if let Some(kiid) = kiid {
        let matches: Vec<_> = inventory
            .iter()
            .filter(|footprint| footprint_kiid(footprint) == kiid)
            .collect();
        return match matches.len() {
            0 => Ok(SelectedTarget::StaleTarget {
                target: kiid.to_string(),
                reason: format!(
                    "No placed footprint on the board has KIID '{kiid}' (expected reference \
                     '{reference}')"
                ),
            }),
            1 => {
                let footprint = matches[0];
                let observed_ref = crate::builders::footprint_reference(footprint);
                if observed_ref != reference {
                    Ok(SelectedTarget::StaleTarget {
                        target: kiid.to_string(),
                        reason: format!(
                            "Footprint KIID '{kiid}' currently has reference '{observed_ref}', \
                             not '{reference}'"
                        ),
                    })
                } else {
                    Ok(SelectedTarget::Found(footprint))
                }
            }
            n => Err(PreflightRefusal::check_failed(format!(
                "KiCad listed {n} footprints with KIID '{kiid}'"
            ))
            .into()),
        };
    }

    let matches: Vec<_> = inventory
        .iter()
        .filter(|footprint| crate::builders::footprint_reference(footprint) == reference)
        .collect();
    match matches.len() {
        0 => Err(FootprintTargetError::Missing {
            reference: reference.to_string(),
        }
        .into()),
        1 => Ok(SelectedTarget::Found(matches[0])),
        count => {
            // Distinct KIIDs let the caller pick one with `kiid`; a KIID
            // listed twice or missing is KiCad's inconsistency, not a choice.
            let mut candidates: Vec<_> = matches
                .iter()
                .map(|footprint| {
                    let at = footprint.position.unwrap_or_default();
                    (
                        footprint_kiid(footprint),
                        at.x_nm as f64 / 1e6,
                        at.y_nm as f64 / 1e6,
                    )
                })
                .collect();
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
            let distinct = candidates.windows(2).all(|pair| pair[0].0 != pair[1].0);
            if !distinct || candidates.iter().any(|(kiid, ..)| kiid.is_empty()) {
                return Err(FootprintTargetError::Ambiguous {
                    reference: reference.to_string(),
                    matches: count,
                }
                .into());
            }
            let listed: Vec<_> = candidates
                .iter()
                .map(|(kiid, x, y)| format!("{kiid} at ({x:.3}, {y:.3}) mm"))
                .collect();
            Ok(SelectedTarget::Ambiguous {
                target: reference.to_string(),
                reason: format!(
                    "Footprint reference '{reference}' matches {count} placed footprints; pass \
                     kiid to choose one: {}",
                    listed.join(", ")
                ),
                candidates: candidates.into_iter().map(|(kiid, ..)| kiid).collect(),
            })
        }
    }
}

/// Apply `edit` to `footprint`. The flags go on the instance attributes:
/// `FOOTPRINT::Deserialize` reads them there, and the definition's attributes
/// carry only the description and keywords. The reference and value go on the
/// instance's fields, with the definition's copies kept in step. The lock is
/// the instance's own `locked`, which `FOOTPRINT::Deserialize` reads.
fn apply_footprint_edit(
    footprint: &mut board::FootprintInstance,
    edit: &IpcFootprintEdit,
) -> Result<()> {
    if let Some(new_reference) = &edit.new_reference {
        let reference = footprint_state(footprint).reference;
        let text = footprint
            .reference_field
            .as_mut()
            .and_then(|field| field.text.as_mut())
            .and_then(|board_text| board_text.text.as_mut())
            .ok_or_else(|| {
                PreflightRefusal::check_failed(format!(
                    "Footprint '{reference}' has no editable reference field"
                ))
            })?;
        text.text = new_reference.clone();
        if let Some(text) = footprint
            .definition
            .as_mut()
            .and_then(|definition| definition.reference_field.as_mut())
            .and_then(|field| field.text.as_mut())
            .and_then(|board_text| board_text.text.as_mut())
        {
            text.text = new_reference.clone();
        }
    }
    if let Some(value) = &edit.value {
        let reference = footprint_state(footprint).reference;
        let text = footprint
            .value_field
            .as_mut()
            .and_then(|field| field.text.as_mut())
            .and_then(|board_text| board_text.text.as_mut())
            .ok_or_else(|| {
                PreflightRefusal::check_failed(format!(
                    "Footprint '{reference}' has no editable value field"
                ))
            })?;
        text.text = value.clone();
        if let Some(text) = footprint
            .definition
            .as_mut()
            .and_then(|definition| definition.value_field.as_mut())
            .and_then(|field| field.text.as_mut())
            .and_then(|board_text| board_text.text.as_mut())
        {
            text.text = value.clone();
        }
    }
    let attributes = footprint.attributes.get_or_insert_with(Default::default);
    if let Some(dnp) = edit.dnp {
        attributes.do_not_populate = dnp;
    }
    if let Some(exclude) = edit.exclude_from_bom {
        attributes.exclude_from_bill_of_materials = exclude;
    }
    if let Some(exclude) = edit.exclude_from_pos_files {
        attributes.exclude_from_position_files = exclude;
    }
    if let Some(locked) = edit.locked {
        footprint.locked = if locked {
            common::LockedState::LsLocked
        } else {
            common::LockedState::LsUnlocked
        } as i32;
    }
    Ok(())
}

/// Every difference between two states of the same footprint that this edit
/// could plausibly touch: the reference, value, flags, lock, schematic link,
/// pose and every mandatory field.
fn edit_mismatches(
    expected: &board::FootprintInstance,
    observed: &board::FootprintInstance,
) -> Vec<String> {
    let (want, got) = (footprint_state(expected), footprint_state(observed));
    let mut out = Vec::new();
    // Compared here as well as in `footprint_mismatches`, which describes a
    // field only when KiCad sent its text attributes.
    if want.reference != got.reference {
        out.push(format!(
            "reference: expected {:?}, observed {:?}",
            want.reference, got.reference
        ));
    }
    if want.value != got.value {
        out.push(format!(
            "value: expected {:?}, observed {:?}",
            want.value, got.value
        ));
    }
    for (name, want, got) in [
        ("dnp", want.dnp, got.dnp),
        (
            "exclude_from_bom",
            want.exclude_from_bom,
            got.exclude_from_bom,
        ),
        (
            "exclude_from_pos_files",
            want.exclude_from_pos_files,
            got.exclude_from_pos_files,
        ),
        ("locked", want.locked, got.locked),
        (
            "linked_to_schematic",
            want.linked_to_schematic,
            got.linked_to_schematic,
        ),
    ] {
        if want != got {
            out.push(format!("{name}: expected {want}, observed {got}"));
        }
    }
    out.extend(footprint_mismatches(expected, observed));
    out
}

impl KiCadIpcClient {
    /// The identity, value, assembly flags and lock of the one placed
    /// footprint with `reference` (and optional exact `kiid`) on `document`.
    pub fn footprint_state_in(
        &self,
        document: common::DocumentSpecifier,
        reference: &str,
        kiid: Option<&str>,
    ) -> Result<IpcFootprintLookupOutcome> {
        let inventory = self.footprint_inventory_in(document)?;
        Ok(
            match select_footprint_target(&inventory, reference, kiid)? {
                SelectedTarget::Found(footprint) => {
                    IpcFootprintLookupOutcome::Found(footprint_state(footprint))
                }
                SelectedTarget::StaleTarget { target, reason } => {
                    IpcFootprintLookupOutcome::StaleTarget { target, reason }
                }
                SelectedTarget::Ambiguous {
                    target,
                    candidates,
                    reason,
                } => IpcFootprintLookupOutcome::AmbiguousTarget {
                    target,
                    candidates,
                    reason,
                },
            },
        )
    }

    /// Change the reference (for board-owned footprints), value, assembly
    /// flags and lock of the one placed footprint with `reference` (and
    /// optional exact `kiid`) on `document`, in one KiCad undo step.
    ///
    /// The whole footprint is sent back, as KiCad has no narrower update,
    /// with only the requested properties changed. Success requires a fresh
    /// read by KIID in which the footprint holds every requested value, and
    /// its pose and mandatory fields are otherwise unchanged. A footprint that
    /// already holds the request is not sent. This method never saves the
    /// board.
    pub fn edit_footprint_in(
        &self,
        document: common::DocumentSpecifier,
        reference: &str,
        kiid: Option<&str>,
        edit: &IpcFootprintEdit,
    ) -> Result<IpcFootprintEditOutcome> {
        anyhow::ensure!(!edit.is_empty(), "the edit names no property to change");
        let inventory = self.footprint_inventory_in(document.clone())?;
        let footprint = match select_footprint_target(&inventory, reference, kiid)? {
            SelectedTarget::Found(footprint) => footprint,
            SelectedTarget::StaleTarget { target, reason } => {
                return Ok(IpcFootprintEditOutcome::StaleTarget { target, reason });
            }
            SelectedTarget::Ambiguous {
                target,
                candidates,
                reason,
            } => {
                return Ok(IpcFootprintEditOutcome::AmbiguousTarget {
                    target,
                    candidates,
                    reason,
                });
            }
        };
        let target_kiid = footprint_kiid(footprint);
        if target_kiid.is_empty() {
            return Err(PreflightRefusal::missing_footprint_kiid(reference).into());
        }
        let before = footprint_state(footprint);

        if let Some(new_reference) = &edit.new_reference {
            if let Some(symbol_path) = linked_symbol_path(footprint) {
                return Ok(IpcFootprintEditOutcome::Blocked {
                    reason: format!(
                        "Footprint '{reference}' (KIID '{target_kiid}') is linked to schematic \
                         symbol '{symbol_path}'. Direct board-side rename is refused so the board \
                         and schematic do not silently diverge; rename the symbol with \
                         edit_schematic_component and propagate with update_pcb_from_schematic"
                    ),
                });
            }
            if new_reference != &before.reference {
                if let Some(other) = inventory.iter().find(|other| {
                    footprint_kiid(other) != target_kiid
                        && crate::builders::footprint_reference(other) == new_reference.as_str()
                }) {
                    let other_kiid = footprint_kiid(other);
                    return Ok(IpcFootprintEditOutcome::Blocked {
                        reason: format!(
                            "Another footprint on the board (KIID '{other_kiid}') already has \
                             reference '{new_reference}'. Choose a unique reference"
                        ),
                    });
                }
            }
        }

        let mut edited = footprint.clone();
        apply_footprint_edit(&mut edited, edit)?;
        let expected = footprint_state(&edited);
        if expected == before {
            return Ok(IpcFootprintEditOutcome::Unchanged { current: before });
        }

        let packed = crate::builders::pack_any(&edited, FOOTPRINT_INSTANCE);
        let mutation = self.run_commit("Edit footprint", |client| {
            client.update_items_in(document.clone(), vec![packed])
        });
        let readback = self.footprint_by_kiid_in(document, &target_kiid);
        let observed = readback.as_ref().ok().map(footprint_state);
        let evidence = readback
            .as_ref()
            .map(|after| {
                (
                    edit_mismatches(&edited, after),
                    edit_mismatches(footprint, after),
                )
            })
            .map_err(|error| format!("{error:#}"));
        Ok(match settle(mutation, evidence) {
            Settled::Applied => IpcFootprintEditOutcome::Applied {
                before,
                after: observed.context("readback proved the edit but lost the footprint")?,
            },
            Settled::Refused(error) => {
                return Err(error.context(
                    "KiCad rejected the footprint edit; a fresh read confirmed the footprint \
                     is unchanged",
                ))
            }
            Settled::Uncertain { mismatches, reason } => IpcFootprintEditOutcome::Uncertain {
                before,
                expected,
                observed,
                mismatches,
                reason,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footprint(value: &str, dnp: bool) -> board::FootprintInstance {
        let field = |text: &str| board::Field {
            text: Some(board::BoardText {
                text: Some(common::Text {
                    text: text.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        board::FootprintInstance {
            id: Some(common::Kiid {
                value: "fp-1".to_string(),
            }),
            reference_field: Some(field("R1")),
            value_field: Some(field(value)),
            attributes: Some(board::FootprintAttributes {
                do_not_populate: dnp,
                ..Default::default()
            }),
            definition: Some(board::Footprint {
                value_field: Some(field(value)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn an_edit_sets_only_what_it_names_on_the_instance() {
        let mut edited = footprint("10k", false);
        apply_footprint_edit(
            &mut edited,
            &IpcFootprintEdit {
                value: Some("4k7".to_string()),
                exclude_from_bom: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let state = footprint_state(&edited);
        assert_eq!(state.value, "4k7");
        assert!(state.exclude_from_bom);
        assert!(!state.dnp, "dnp was not named");
        assert!(!state.exclude_from_pos_files, "not named");
        // The definition's copy of the value follows the instance.
        let definition_value = edited
            .definition
            .unwrap()
            .value_field
            .unwrap()
            .text
            .unwrap()
            .text
            .unwrap()
            .text;
        assert_eq!(definition_value, "4k7");
    }

    #[test]
    fn a_flag_kicad_did_not_keep_is_a_mismatch() {
        let expected = footprint("10k", true);
        let observed = footprint("10k", false);
        assert_eq!(
            edit_mismatches(&expected, &observed),
            vec!["dnp: expected true, observed false".to_string()]
        );
        assert!(edit_mismatches(&expected, &expected).is_empty());
    }

    #[test]
    fn the_lock_is_set_and_cleared_on_the_instance() {
        let mut edited = footprint("10k", false);
        let lock = |locked| IpcFootprintEdit {
            locked: Some(locked),
            ..Default::default()
        };
        apply_footprint_edit(&mut edited, &lock(true)).unwrap();
        assert_eq!(edited.locked, common::LockedState::LsLocked as i32);
        assert!(footprint_state(&edited).locked);
        assert_eq!(
            edited.attributes,
            footprint("10k", false).attributes,
            "the lock is not an attribute"
        );
        apply_footprint_edit(&mut edited, &lock(false)).unwrap();
        assert_eq!(edited.locked, common::LockedState::LsUnlocked as i32);
        assert!(!footprint_state(&edited).locked);
    }

    #[test]
    fn a_lock_kicad_did_not_keep_is_a_mismatch() {
        let mut expected = footprint("10k", false);
        expected.locked = common::LockedState::LsLocked as i32;
        let mut observed = footprint("10k", false);
        observed.locked = common::LockedState::LsUnlocked as i32;
        assert_eq!(
            edit_mismatches(&expected, &observed),
            vec!["locked: expected true, observed false".to_string()]
        );
    }

    #[test]
    fn a_value_kicad_did_not_keep_is_a_mismatch() {
        let mismatches = edit_mismatches(&footprint("4k7", false), &footprint("10k", false));
        assert!(!mismatches.is_empty());
        assert!(
            mismatches.iter().any(|m| m.contains("value")),
            "{mismatches:?}"
        );
    }

    #[test]
    fn a_rename_updates_instance_and_definition_reference_and_reports_mismatch() {
        let mut before = footprint("10k", false);
        before.definition.as_mut().unwrap().reference_field = before.reference_field.clone();
        let mut edited = before.clone();
        apply_footprint_edit(
            &mut edited,
            &IpcFootprintEdit {
                new_reference: Some("LOGO1".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(footprint_state(&edited).reference, "LOGO1");
        assert_eq!(footprint_state(&edited).kiid, "fp-1");
        let definition_ref = edited
            .definition
            .as_ref()
            .unwrap()
            .reference_field
            .as_ref()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .text
            .as_str();
        assert_eq!(definition_ref, "LOGO1");
        let mismatches = edit_mismatches(&edited, &before);
        assert!(
            mismatches
                .iter()
                .any(|m| m.contains("reference: expected \"LOGO1\", observed \"R1\"")),
            "{mismatches:?}"
        );
    }

    #[test]
    fn an_all_empty_sheet_path_is_not_linked_to_a_schematic() {
        let mut fp = footprint("10k", false);
        fp.symbol_path = Some(common::SheetPath {
            path: vec![common::Kiid {
                value: String::new(),
            }],
            path_human_readable: "/".to_string(),
        });
        assert!(!footprint_state(&fp).linked_to_schematic);
        assert_eq!(linked_symbol_path(&fp), None);

        fp.symbol_path = Some(common::SheetPath {
            path: vec![common::Kiid {
                value: "11111111-1111-1111-1111-111111111111".to_string(),
            }],
            path_human_readable: String::new(),
        });
        assert!(footprint_state(&fp).linked_to_schematic);
        assert_eq!(
            linked_symbol_path(&fp).as_deref(),
            Some("/11111111-1111-1111-1111-111111111111")
        );
    }
}
