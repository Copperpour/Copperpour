//! Editing pads of placed footprints: the one update path pad nets and pad
//! teardrop settings share.
//!
//! KiCad 10 cannot update a pad on its own reliably. `UpdateItems` on a bare
//! `Pad` is refused with `ISC_INVALID_TYPE`; with its footprint as the
//! request's container it applies once, after which KiCad's item-by-ID cache
//! answers for that footprint's pads with the commit's undo copy and the next
//! pad update is silently lost (observed on 10.0.7 and 10.0.8). So a pad edit
//! sends its whole `FootprintInstance` back, as read, with only the pad fields
//! the caller changed.
//!
//! `UpdateItems` has no field mask: KiCad replaces the whole footprint. The
//! proof that nothing else changed is the read-back: every sent footprint is
//! read again by enumeration (never `GetItemsById`, for the cache reason
//! above) and compared field by field with what was sent, and with what was
//! there before. [`settle`] turns that into applied, refused or uncertain.

use super::text_edit::{settle, Settled};
use super::*;
use crate::gen::kiapi::board::types as board;
use std::collections::{BTreeMap, BTreeSet};

const PAD: &str = "kiapi.board.types.Pad";
const FOOTPRINT_INSTANCE: &str = "kiapi.board.types.FootprintInstance";

/// A pad edit's target is not what the caller read: a footprint is gone or
/// listed twice, or a required pad is not in its footprint. Raised before
/// anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintEditTargetError {
    pub reason: String,
}

impl std::fmt::Display for FootprintEditTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; nothing was written", self.reason)
    }
}

impl std::error::Error for FootprintEditTargetError {}

/// The result of [`KiCadIpcClient::update_footprint_pads_in`].
#[derive(Debug)]
pub(crate) enum FootprintPadUpdate {
    /// No pad changed and nothing else was given: nothing was sent.
    Unchanged,
    /// The commit landed and a fresh read of every sent footprint holds
    /// exactly what was sent. `after` holds every named footprint as read
    /// afterwards (unsent ones as read before).
    Applied {
        before: Vec<board::FootprintInstance>,
        after: Vec<board::FootprintInstance>,
    },
    /// KiCad refused the commit, and a fresh read showed every sent
    /// footprint, and every extra item by `extra_evidence`, as it was.
    Refused(anyhow::Error),
    /// Neither the request nor the old state could be proved.
    Uncertain {
        mismatches: Vec<String>,
        reason: String,
    },
}

impl KiCadIpcClient {
    /// Edit pads of the footprints `footprints` (KIIDs) and send the changed
    /// footprints back whole, with `extra` top-level items, in one KiCad
    /// commit named `description`.
    ///
    /// `edit` sees each pad of each named footprint, decoded from what KiCad
    /// sent, and returns whether it changed it. A footprint none of whose
    /// pads changed is not sent. Every KIID in `required_pads` must be among
    /// the changed pads. `extra` items (vias, say) go in the same
    /// `UpdateItems`; `extra_evidence` reads them back after the commit and
    /// returns their differences from what was sent and from what was there,
    /// as the footprints' are.
    ///
    /// Afterwards every sent footprint is read again and compared with what
    /// was sent and with what was there (see [`footprint_differences`]), and
    /// [`settle`] decides from both and the extra evidence. A footprint that
    /// is missing or listed twice, or a required pad not edited, refuses the
    /// whole edit before anything is sent, with a [`FootprintEditTargetError`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_footprint_pads_in(
        &self,
        document: kiapi::common::types::DocumentSpecifier,
        description: &str,
        footprints: &[String],
        required_pads: &BTreeSet<String>,
        extra: Vec<prost_types::Any>,
        extra_evidence: impl FnOnce(&Self) -> Result<(Vec<String>, Vec<String>)>,
        mut edit: impl FnMut(&str, &mut board::Pad) -> Result<bool>,
    ) -> Result<FootprintPadUpdate> {
        let placed = self.placed_footprints_by_kiid_in(document.clone())?;
        let mut before = Vec::new();
        for kiid in footprints {
            match placed.get(kiid).map(Vec::as_slice) {
                Some([footprint]) => before.push(footprint.clone()),
                found => {
                    return Err(anyhow::Error::new(FootprintEditTargetError {
                        reason: match found.map_or(0, <[_]>::len) {
                            0 => format!("footprint {kiid} is no longer on the board"),
                            n => format!("KiCad listed {n} footprints with KIID {kiid}"),
                        },
                    }))
                }
            }
        }

        let mut sent = Vec::new();
        let mut edited = BTreeSet::new();
        for footprint in &before {
            let kiid = kiid_value(footprint.id.clone());
            let mut expected = footprint.clone();
            let definition = expected
                .definition
                .as_mut()
                .with_context(|| format!("footprint {kiid} carries no definition"))?;
            let mut changed = false;
            for child in definition.items.iter_mut() {
                if !crate::builders::any_is(child, PAD) {
                    continue;
                }
                let mut pad = board::Pad::decode(child.value.as_slice())
                    .with_context(|| format!("footprint {kiid} has an unreadable pad"))?;
                if edit(&kiid, &mut pad)? {
                    edited.insert(kiid_value(pad.id.clone()));
                    *child = crate::builders::pack_any(&pad, PAD);
                    changed = true;
                }
            }
            if changed {
                sent.push((footprint.clone(), expected));
            }
        }
        if let Some(missing) = required_pads.iter().find(|id| !edited.contains(*id)) {
            return Err(anyhow::Error::new(FootprintEditTargetError {
                reason: format!("pad {missing} is no longer in its footprint"),
            }));
        }
        if sent.is_empty() && extra.is_empty() {
            return Ok(FootprintPadUpdate::Unchanged);
        }

        let mut updates = extra;
        updates.extend(
            sent.iter()
                .map(|(_, expected)| crate::builders::pack_any(expected, FOOTPRINT_INSTANCE)),
        );
        let update_document = document.clone();
        let mutation = self.run_commit(description, move |client| {
            client.update_items_in(update_document, updates)
        });

        let read = self.placed_footprints_by_kiid_in(document);
        let extra_read = extra_evidence(self).map_err(|error| format!("{error:#}"));
        let evidence = read
            .as_ref()
            .map_err(|error| format!("{error:#}"))
            .and_then(|placed| {
                let mut to_expected = Vec::new();
                let mut to_before = Vec::new();
                for (original, expected) in &sent {
                    let kiid = kiid_value(original.id.clone());
                    let Some([observed]) = placed.get(&kiid).map(Vec::as_slice) else {
                        let gone = format!("footprint {kiid}: not exactly one with its KIID");
                        to_expected.push(gone.clone());
                        to_before.push(gone);
                        continue;
                    };
                    to_expected.extend(footprint_differences(expected, observed));
                    to_before.extend(footprint_differences(original, observed));
                }
                let (extra_expected, extra_before) = extra_read?;
                to_expected.extend(extra_expected);
                to_before.extend(extra_before);
                Ok((to_expected, to_before))
            });
        Ok(match settle(mutation, evidence) {
            Settled::Applied => {
                let placed = read?;
                let after = before
                    .iter()
                    .map(|footprint| {
                        let kiid = kiid_value(footprint.id.clone());
                        placed
                            .get(&kiid)
                            .and_then(|found| found.first())
                            .cloned()
                            .unwrap_or_else(|| footprint.clone())
                    })
                    .collect();
                FootprintPadUpdate::Applied { before, after }
            }
            Settled::Refused(error) => FootprintPadUpdate::Refused(error),
            Settled::Uncertain { mismatches, reason } => {
                FootprintPadUpdate::Uncertain { mismatches, reason }
            }
        })
    }

    /// Every placed footprint by KIID, enumerated with `GetItems`. A KIID
    /// KiCad lists twice keeps both, so a caller can refuse it.
    fn placed_footprints_by_kiid_in(
        &self,
        document: kiapi::common::types::DocumentSpecifier,
    ) -> Result<BTreeMap<String, Vec<board::FootprintInstance>>> {
        let mut by_kiid: BTreeMap<String, Vec<board::FootprintInstance>> = BTreeMap::new();
        for footprint in self.placed_footprints_in(document)? {
            by_kiid
                .entry(kiid_value(footprint.id.clone()))
                .or_default()
                .push(footprint);
        }
        Ok(by_kiid)
    }
}

/// Every difference between two forms of one placed footprint, as readable
/// `footprint <kiid>: …` lines naming the field: the footprint's own fields,
/// its definition's, each pad's (pads keyed by KIID) and its other children
/// (compared as a multiset of messages, since they carry no stable order).
///
/// Two things are compared as KiCad means them rather than byte for byte: a
/// pad's net by name (KiCad 10's API deprecates net codes and reads the
/// name), and teardrop settings through [`IpcTeardropSettings`], where an
/// absent or zero length limit is the same "no limit". A pad's
/// `zone_layer_overrides` is not compared: the proto marks it an unstable
/// KiCad implementation detail, and KiCad derives it from zone fills.
pub(super) fn footprint_differences(
    expected: &board::FootprintInstance,
    observed: &board::FootprintInstance,
) -> Vec<String> {
    let kiid = kiid_value(expected.id.clone());
    let mut out = Vec::new();
    let mut note = |what: String| out.push(format!("footprint {kiid}: {what}"));

    macro_rules! compare {
        ($a:expr, $b:expr, $prefix:expr, [$($field:ident),* $(,)?]) => {
            $(
                if $a.$field != $b.$field {
                    note(format!("{}{}", $prefix, stringify!($field)));
                }
            )*
        };
    }
    compare!(
        expected,
        observed,
        "",
        [
            id,
            position,
            orientation,
            layer,
            locked,
            reference_field,
            value_field,
            datasheet_field,
            description_field,
            attributes,
            overrides,
            symbol_path,
            symbol_sheet_name,
            symbol_sheet_filename,
            symbol_footprint_filters,
            parent,
            embedded_files,
            variants,
            symbol_units,
        ]
    );
    let (Some(a), Some(b)) = (expected.definition.as_ref(), observed.definition.as_ref()) else {
        if expected.definition.is_some() != observed.definition.is_some() {
            note("definition".to_string());
        }
        return out;
    };
    compare!(
        a,
        b,
        "definition.",
        [
            id,
            anchor,
            attributes,
            overrides,
            net_ties,
            private_layers,
            reference_field,
            value_field,
            datasheet_field,
            description_field,
            jumpers,
        ]
    );

    let pads = |definition: &board::Footprint| -> BTreeMap<String, Vec<board::Pad>> {
        let mut pads: BTreeMap<String, Vec<board::Pad>> = BTreeMap::new();
        for item in &definition.items {
            if crate::builders::any_is(item, PAD) {
                match board::Pad::decode(item.value.as_slice()) {
                    Ok(pad) => pads
                        .entry(kiid_value(pad.id.clone()))
                        .or_default()
                        .push(pad),
                    // Listed under its own key twice so it never matches.
                    Err(_) => pads
                        .entry("<unreadable pad>".into())
                        .or_default()
                        .extend([board::Pad::default(), board::Pad::default()]),
                }
            }
        }
        pads
    };
    let (pads_a, pads_b) = (pads(a), pads(b));
    for (id, listed) in &pads_a {
        match (listed.as_slice(), pads_b.get(id).map(Vec::as_slice)) {
            ([pad_a], Some([pad_b])) => {
                let prefix = format!("pad {id} ");
                compare!(
                    pad_a,
                    pad_b,
                    prefix,
                    [
                        locked,
                        number,
                        r#type,
                        pad_stack,
                        position,
                        copper_clearance_override,
                        pad_to_die_length,
                        symbol_pin,
                        pad_to_die_delay,
                        parent,
                        fab_property,
                    ]
                );
                // An absent net and an empty one are both "unconnected".
                let net = |pad: &board::Pad| {
                    pad.net
                        .as_ref()
                        .map(|net| net.name.clone())
                        .unwrap_or_default()
                };
                if net(pad_a) != net(pad_b) {
                    note(format!("pad {id} net"));
                }
                let teardrop = |pad: &board::Pad| {
                    pad.teardrop
                        .as_ref()
                        .map(|t| IpcTeardropSettings::from_proto(t).ok_or(*t))
                };
                if teardrop(pad_a) != teardrop(pad_b) {
                    note(format!("pad {id} teardrop"));
                }
            }
            (_, None) => note(format!("pad {id} missing")),
            _ => note(format!("pad {id} listed more than once")),
        }
    }
    for id in pads_b.keys().filter(|id| !pads_a.contains_key(*id)) {
        note(format!("pad {id} added"));
    }

    let others = |definition: &board::Footprint| -> Vec<(String, Vec<u8>)> {
        let mut others: Vec<_> = definition
            .items
            .iter()
            .filter(|item| !crate::builders::any_is(item, PAD))
            .map(|item| (item.type_url.clone(), item.value.clone()))
            .collect();
        others.sort();
        others
    };
    let (others_a, others_b) = (others(a), others(b));
    if others_a != others_b {
        let differing = others_a.iter().filter(|o| !others_b.contains(o)).count()
            + others_b.iter().filter(|o| !others_a.contains(o)).count();
        note(format!(
            "{differing} non-pad child item(s) differ ({} before, {} after)",
            others_a.len(),
            others_b.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(id: &str, number: &str, net: &str) -> prost_types::Any {
        crate::builders::pack_any(
            &board::Pad {
                id: Some(kiapi::common::types::Kiid { value: id.into() }),
                number: number.into(),
                net: Some(kiapi::board::types::Net {
                    code: None,
                    name: net.into(),
                }),
                ..Default::default()
            },
            PAD,
        )
    }

    fn footprint(items: Vec<prost_types::Any>) -> board::FootprintInstance {
        board::FootprintInstance {
            id: Some(kiapi::common::types::Kiid { value: "fp".into() }),
            definition: Some(board::Footprint {
                items,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn identical_footprints_have_no_differences() {
        let a = footprint(vec![pad("p1", "1", "GND"), pad("p2", "2", "VCC")]);
        assert!(footprint_differences(&a, &a.clone()).is_empty());
    }

    /// Pads are matched by KIID, other children as a multiset: KiCad's own
    /// order is not a change.
    #[test]
    fn child_order_is_not_a_difference() {
        let field = crate::builders::pack_any(&board::Field::default(), "kiapi.board.types.Field");
        let a = footprint(vec![
            pad("p1", "1", "GND"),
            field.clone(),
            pad("p2", "2", "VCC"),
        ]);
        let b = footprint(vec![pad("p2", "2", "VCC"), pad("p1", "1", "GND"), field]);
        assert!(footprint_differences(&a, &b).is_empty());
    }

    #[test]
    fn each_changed_pad_field_is_named() {
        let a = footprint(vec![pad("p1", "1", "GND"), pad("p2", "2", "VCC")]);
        let b = footprint(vec![pad("p1", "1", "VCC"), pad("p3", "3", "VCC")]);
        assert_eq!(
            footprint_differences(&a, &b),
            [
                "footprint fp: pad p1 net",
                "footprint fp: pad p2 missing",
                "footprint fp: pad p3 added",
            ]
        );
    }

    /// A net is its name: KiCad 10 answers with a code the request left out.
    #[test]
    fn a_net_code_kicad_adds_is_not_a_difference() {
        let a = footprint(vec![pad("p1", "1", "GND")]);
        let mut b = a.clone();
        let definition = b.definition.as_mut().unwrap();
        let mut held = board::Pad::decode(definition.items[0].value.as_slice()).unwrap();
        held.net.as_mut().unwrap().code = Some(kiapi::board::types::NetCode { value: 3 });
        definition.items[0] = crate::builders::pack_any(&held, PAD);
        assert!(footprint_differences(&a, &b).is_empty());
    }

    /// `set_pad_net` sends an empty name to unconnect; KiCad may answer with
    /// no net at all. Both are unconnected.
    #[test]
    fn an_absent_net_and_an_empty_one_are_the_same() {
        let a = footprint(vec![pad("p1", "1", "")]);
        let mut b = a.clone();
        let definition = b.definition.as_mut().unwrap();
        let mut held = board::Pad::decode(definition.items[0].value.as_slice()).unwrap();
        held.net = None;
        definition.items[0] = crate::builders::pack_any(&held, PAD);
        assert!(footprint_differences(&a, &b).is_empty());
    }

    /// An absent length limit and a zero one are KiCad's same "no limit".
    #[test]
    fn teardrop_settings_compare_as_kicad_means_them() {
        let with = |max_width: Option<i64>| {
            let mut pad = board::Pad::decode(pad("p1", "1", "GND").value.as_slice()).unwrap();
            pad.teardrop = Some(board::PadTeardropSettings {
                mode: board::PadTeardropMode::PtmEnabled as i32,
                max_width: max_width.map(|value_nm| kiapi::common::types::Distance { value_nm }),
                ..Default::default()
            });
            footprint(vec![crate::builders::pack_any(&pad, PAD)])
        };
        assert!(footprint_differences(&with(None), &with(Some(0))).is_empty());
        assert_eq!(
            footprint_differences(&with(None), &with(Some(5))),
            ["footprint fp: pad p1 teardrop"]
        );
    }

    #[test]
    fn a_dropped_non_pad_child_is_a_difference() {
        let field = crate::builders::pack_any(&board::Field::default(), "kiapi.board.types.Field");
        let a = footprint(vec![pad("p1", "1", "GND"), field]);
        let b = footprint(vec![pad("p1", "1", "GND")]);
        assert_eq!(
            footprint_differences(&a, &b),
            ["footprint fp: 1 non-pad child item(s) differ (1 before, 0 after)"]
        );
    }
}
