//! In-place edit of a placed footprint's value and assembly flags: do not
//! populate, exclude from BOM and exclude from position files.
//!
//! The contract is the one [`super::text_edit`] follows for fields: read the
//! exact footprint, change only the requested properties on the message KiCad
//! sent, send it in one KiCad undo step, then prove the result with a fresh
//! read rather than echoing the request.

use super::text_edit::{footprint_kiid, footprint_mismatches, settle, Settled};
use super::*;
use crate::gen::kiapi::board::types as board;
use crate::gen::kiapi::common::types as common;

const FOOTPRINT_INSTANCE: &str = "kiapi.board.types.FootprintInstance";

/// The value and assembly flags of `footprint`, from its instance attributes,
/// which are the ones KiCad reads.
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
        linked_to_schematic: footprint
            .symbol_path
            .as_ref()
            .is_some_and(|path| !path.path.is_empty()),
    }
}

/// Apply `edit` to `footprint`. The flags go on the instance attributes:
/// `FOOTPRINT::Deserialize` reads them there, and the definition's attributes
/// carry only the description and keywords. The value goes on the instance's
/// value field, with the definition's copy kept in step.
fn apply_footprint_edit(
    footprint: &mut board::FootprintInstance,
    edit: &IpcFootprintEdit,
) -> Result<()> {
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
    Ok(())
}

/// Every difference between two states of the same footprint that this edit
/// could plausibly touch: the value and the flags, the pose and every
/// mandatory field.
fn edit_mismatches(
    expected: &board::FootprintInstance,
    observed: &board::FootprintInstance,
) -> Vec<String> {
    let (want, got) = (footprint_state(expected), footprint_state(observed));
    let mut out = Vec::new();
    // Compared here as well as in `footprint_mismatches`, which describes a
    // field only when KiCad sent its text attributes.
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
    ] {
        if want != got {
            out.push(format!("{name}: expected {want}, observed {got}"));
        }
    }
    out.extend(footprint_mismatches(expected, observed));
    out
}

impl KiCadIpcClient {
    /// The value and assembly flags of the one placed footprint with
    /// `reference` on `document`.
    pub fn footprint_state_in(
        &self,
        document: common::DocumentSpecifier,
        reference: &str,
    ) -> Result<IpcFootprintState> {
        Ok(footprint_state(
            &self.find_footprint_instance_in(document, reference)?,
        ))
    }

    /// Change the value and the assembly flags of the one placed footprint
    /// with `reference` on `document`, in one KiCad undo step.
    ///
    /// The whole footprint is sent back, as KiCad has no narrower update,
    /// with only the requested properties changed. Success requires a fresh
    /// read in which the footprint holds every requested value, and its pose
    /// and mandatory fields are otherwise unchanged. A footprint that already
    /// holds the request is not sent. This method never saves the board.
    pub fn edit_footprint_in(
        &self,
        document: common::DocumentSpecifier,
        reference: &str,
        edit: &IpcFootprintEdit,
    ) -> Result<IpcTextEditOutcome<IpcFootprintState>> {
        anyhow::ensure!(!edit.is_empty(), "the edit names no property to change");
        let footprint = self.find_footprint_instance_in(document.clone(), reference)?;
        let kiid = footprint_kiid(&footprint);
        if kiid.is_empty() {
            return Err(PreflightRefusal::missing_footprint_kiid(reference).into());
        }
        let before = footprint_state(&footprint);

        let mut edited = footprint.clone();
        apply_footprint_edit(&mut edited, edit)?;
        let expected = footprint_state(&edited);
        if expected == before {
            return Ok(IpcTextEditOutcome::Unchanged { current: before });
        }

        let packed = crate::builders::pack_any(&edited, FOOTPRINT_INSTANCE);
        let mutation = self.run_commit("Edit footprint", |client| {
            client.update_items_in(document.clone(), vec![packed])
        });
        let readback = self.footprint_by_kiid_in(document, &kiid);
        let observed = readback.as_ref().ok().map(footprint_state);
        let evidence = readback
            .as_ref()
            .map(|after| {
                (
                    edit_mismatches(&edited, after),
                    edit_mismatches(&footprint, after),
                )
            })
            .map_err(|error| format!("{error:#}"));
        Ok(match settle(mutation, evidence) {
            Settled::Applied => IpcTextEditOutcome::Applied {
                before,
                after: observed.context("readback proved the edit but lost the footprint")?,
            },
            Settled::Refused(error) => {
                return Err(error.context(
                    "KiCad rejected the footprint edit; a fresh read confirmed the footprint \
                     is unchanged",
                ))
            }
            Settled::Uncertain { mismatches, reason } => IpcTextEditOutcome::Uncertain {
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
    fn a_value_kicad_did_not_keep_is_a_mismatch() {
        let mismatches = edit_mismatches(&footprint("4k7", false), &footprint("10k", false));
        assert!(!mismatches.is_empty());
        assert!(
            mismatches.iter().any(|m| m.contains("value")),
            "{mismatches:?}"
        );
    }
}
