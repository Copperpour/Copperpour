//! Native pad and via teardrop settings on a live board (#555).
//!
//! KiCad 10.0.7 added `PadTeardropSettings` to the `Pad` and `Via` messages.
//! Earlier releases neither send nor read the field: an update carrying it is
//! accepted and silently dropped, which is why the tool layer refuses before
//! any write unless the bound editor reports 10.0.7 or newer, and why the
//! reads here refuse a pad or via that comes back without the field instead
//! of reporting it as disabled.
//!
//! What KiCad does with a settings change, observed live on 10.0.7 and
//! 10.0.8 (`tests/live_teardrop_test.rs`):
//!
//! - The commit itself rebuilds the teardrops of every pad of a touched
//!   footprint and of a touched via (`BOARD_COMMIT::Push` runs
//!   `TEARDROP_MANAGER::RemoveTeardrops`/`UpdateTeardrops` for them), in the
//!   same undo step. Settings reach copper without a refill.
//! - A refill (`RefillZones`, KiCad's fill-all) rebuilds every teardrop on
//!   the board and refills every zone. It keeps the commit's teardrop
//!   outlines but refills them, and restores teardrops nothing else rebuilt.
//!
//! Pads are sent back inside their whole `FootprintInstance`, not one by one.
//! An `UpdateItems` of a single pad (container: its footprint) applies once,
//! but leaves KiCad's item-by-ID cache answering for that footprint's pads
//! with the commit's undo copy: `GetItemsById` then returns the pad as it
//! was, and the next pad update is applied to the copy and silently lost.
//! A whole-footprint update replaces the footprint and does not have that
//! problem, but it drops a footprint-owned group, which KiCad 10 does not
//! serialize in the footprint. Such footprints are refused, and the board's
//! own serialization is compared before and after every commit: anything
//! besides teardrop settings and generated teardrop zones that changed makes
//! the outcome uncertain.
//!
//! Every read here enumerates with `GetItems`, never `GetItemsById`, for the
//! same cache reason.

use super::*;
use crate::gen::kiapi::board::types as board;
use crate::gen::kiapi::common::types as common;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PAD: &str = "kiapi.board.types.Pad";
const VIA: &str = "kiapi.board.types.Via";
const ZONE: &str = "kiapi.board.types.Zone";
const FOOTPRINT_INSTANCE: &str = "kiapi.board.types.FootprintInstance";

/// The first KiCad release whose IPC API carries pad and via teardrop
/// settings, as `(major, minor, patch)`.
pub const TEARDROP_SETTINGS_MIN_KICAD_VERSION: (u32, u32, u32) = (10, 0, 7);

/// Whether `version` is known and at least
/// [`TEARDROP_SETTINGS_MIN_KICAD_VERSION`]. A `0.0.0` answer is a KiCad that
/// did not say, not an old one, and is refused the same way.
pub fn supports_teardrop_settings(version: &IpcKiCadVersion) -> bool {
    let observed = (version.major, version.minor, version.patch);
    observed != (0, 0, 0) && observed >= TEARDROP_SETTINGS_MIN_KICAD_VERSION
}

/// A length limit as KiCad holds it: `TEARDROP_PARAMETERS` treats a value
/// `<= 0` as no constraint, and 10.0.7's `UnpackTeardropSettings` reads an
/// absent distance as 0. Both are `None` here.
fn constraint(distance: Option<&common::Distance>) -> Option<i64> {
    distance
        .map(|distance| distance.value_nm)
        .filter(|value_nm| *value_nm > 0)
}

/// One pad's or via's native teardrop settings, as `PadTeardropSettings`
/// carries them. Lengths are nanometres; `None` is KiCad's "no constraint".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcTeardropSettings {
    pub enabled: bool,
    pub curved_edges: bool,
    pub allow_multiple_track_segments: bool,
    pub prefer_zone_connection: bool,
    pub max_length_nm: Option<i64>,
    pub max_width_nm: Option<i64>,
    pub best_length_ratio: f64,
    pub best_width_ratio: f64,
    pub max_track_width_ratio: f64,
}

impl IpcTeardropSettings {
    /// Read KiCad's message. `None` for a mode this build does not know,
    /// rather than guessing enabled or disabled.
    pub fn from_proto(settings: &board::PadTeardropSettings) -> Option<Self> {
        let enabled = match board::PadTeardropMode::try_from(settings.mode).ok()? {
            board::PadTeardropMode::PtmEnabled => true,
            board::PadTeardropMode::PtmDisabled => false,
            board::PadTeardropMode::PtmUnknown => return None,
        };
        Some(Self {
            enabled,
            curved_edges: settings.curved_edges,
            allow_multiple_track_segments: settings.allow_multiple_track_segments,
            prefer_zone_connection: settings.prefer_zone_connection,
            max_length_nm: constraint(settings.max_length.as_ref()),
            max_width_nm: constraint(settings.max_width.as_ref()),
            best_length_ratio: settings.best_length_ratio,
            best_width_ratio: settings.best_width_ratio,
            max_track_width_ratio: settings.max_track_width_ratio,
        })
    }

    /// The fields where `self` differs from `other`, as `(field, self, other)`.
    pub fn differing_fields(&self, other: &Self) -> Vec<(&'static str, String, String)> {
        let mut out = Vec::new();
        let mut check = |field: &'static str, a: String, b: String| {
            if a != b {
                out.push((field, a, b));
            }
        };
        check(
            "enabled",
            self.enabled.to_string(),
            other.enabled.to_string(),
        );
        check(
            "curved_edges",
            self.curved_edges.to_string(),
            other.curved_edges.to_string(),
        );
        check(
            "allow_multiple_track_segments",
            self.allow_multiple_track_segments.to_string(),
            other.allow_multiple_track_segments.to_string(),
        );
        check(
            "prefer_zone_connection",
            self.prefer_zone_connection.to_string(),
            other.prefer_zone_connection.to_string(),
        );
        check(
            "max_length_nm",
            format!("{:?}", self.max_length_nm),
            format!("{:?}", other.max_length_nm),
        );
        check(
            "max_width_nm",
            format!("{:?}", self.max_width_nm),
            format!("{:?}", other.max_width_nm),
        );
        check(
            "best_length_ratio",
            self.best_length_ratio.to_string(),
            other.best_length_ratio.to_string(),
        );
        check(
            "best_width_ratio",
            self.best_width_ratio.to_string(),
            other.best_width_ratio.to_string(),
        );
        check(
            "max_track_width_ratio",
            self.max_track_width_ratio.to_string(),
            other.max_track_width_ratio.to_string(),
        );
        out
    }

    /// Write these settings onto a message KiCad sent, leaving any field this
    /// build does not know untouched.
    pub fn write_to(&self, settings: &mut board::PadTeardropSettings) {
        settings.mode = if self.enabled {
            board::PadTeardropMode::PtmEnabled
        } else {
            board::PadTeardropMode::PtmDisabled
        } as i32;
        settings.curved_edges = self.curved_edges;
        settings.allow_multiple_track_segments = self.allow_multiple_track_segments;
        settings.prefer_zone_connection = self.prefer_zone_connection;
        // KiCad reads an absent distance as 0, which is its "no constraint";
        // say so explicitly rather than rely on the default.
        settings.max_length = Some(common::Distance {
            value_nm: self.max_length_nm.unwrap_or(0),
        });
        settings.max_width = Some(common::Distance {
            value_nm: self.max_width_nm.unwrap_or(0),
        });
        settings.best_length_ratio = self.best_length_ratio;
        settings.best_width_ratio = self.best_width_ratio;
        settings.max_track_width_ratio = self.max_track_width_ratio;
    }
}

/// Pad or via.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcTeardropTargetKind {
    Pad,
    Via,
}

/// One pad or via that can carry native teardrops, read from KiCad.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcTeardropTarget {
    pub kind: IpcTeardropTargetKind,
    pub uuid: String,
    /// The owning footprint's KIID, for a pad.
    pub footprint_uuid: Option<String>,
    /// The owning footprint's reference, for a pad, when KiCad names one.
    pub reference: Option<String>,
    /// The pad number, for a pad.
    pub pad_number: Option<String>,
    /// Net name, empty when the item carries no net.
    pub net: String,
    /// Board-absolute position in millimetres.
    pub position: IpcVector2,
    pub settings: IpcTeardropSettings,
}

/// One teardrop zone KiCad generated, as observed copper evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcTeardropZone {
    pub uuid: String,
    pub net: String,
    pub layers: Vec<String>,
    /// `via_pad`, `track_end`, `unspecified`, or `unknown`.
    pub teardrop_type: String,
    pub filled: bool,
    /// FNV-1a digest of the zone's outline as KiCad serialized it.
    pub outline_digest: String,
    /// FNV-1a digest of the zone's filled polygons as KiCad serialized them:
    /// equal digests are the same copper.
    pub fill_digest: String,
}

/// What a teardrop read refused, before anything was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeardropReadError {
    /// KiCad sent a pad or via without `PadTeardropSettings`, or with a mode
    /// this build does not know. The editor is not serving the field, so its
    /// settings are unknown, not disabled.
    SettingsMissing {
        kind: IpcTeardropTargetKind,
        uuid: String,
    },
}

impl std::fmt::Display for TeardropReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SettingsMissing { kind, uuid } => write!(
                f,
                "KiCad sent {} {uuid} without readable teardrop settings",
                match kind {
                    IpcTeardropTargetKind::Pad => "pad",
                    IpcTeardropTargetKind::Via => "via",
                }
            ),
        }
    }
}

impl std::error::Error for TeardropReadError {}

/// One requested settings change: the target's identity as read, and the
/// complete settings it should hold.
#[derive(Debug, Clone, PartialEq)]
pub struct IpcTeardropEdit {
    pub kind: IpcTeardropTargetKind,
    pub uuid: String,
    pub footprint_uuid: Option<String>,
    /// The settings the caller read. A target holding others by the time it
    /// is written was changed meanwhile, and the batch is refused as stale.
    pub current: IpcTeardropSettings,
    pub settings: IpcTeardropSettings,
}

/// Why a settings change was refused with nothing written.
#[derive(Debug, Clone, PartialEq)]
pub enum IpcTeardropRefusal {
    /// A target is gone, or is no longer the kind or in the footprint the
    /// caller read.
    Stale { reason: String },
    /// The board is in KiCad's legacy teardrop mode, where no rebuild
    /// creates teardrops from these settings.
    LegacyTeardrops,
    /// Pads of these footprints are targeted, and each owns groups that a
    /// whole-footprint update would drop: `(footprint KIID, reference,
    /// owned group count)`.
    FootprintOwnsGroups {
        footprints: Vec<(String, String, usize)>,
    },
    /// KiCad refused the commit, and a fresh read showed every sent item as
    /// it was.
    Rejected { reason: String },
}

/// A confirmed settings change.
#[derive(Debug, Clone, PartialEq)]
pub struct IpcTeardropApplied {
    /// The targets as a fresh enumeration shows them after the commit.
    pub readback: Vec<IpcTeardropTarget>,
    /// Teardrop zones before and after the commit.
    pub zones_before: Vec<IpcTeardropZone>,
    pub zones_after: Vec<IpcTeardropZone>,
    /// Pads carried along in a whole-footprint update without being targeted;
    /// the post-commit comparison shows them unchanged.
    pub carried_pads: usize,
    /// What the read-back differs from before in, field by field (`footprint
    /// <kiid>: pad <kiid> teardrop`, `via <kiid> teardrop`): only teardrop
    /// fields, or the outcome would be uncertain.
    pub changed_fields: Vec<String>,
}

/// The result of [`KiCadIpcClient::set_teardrop_settings_in`].
#[derive(Debug, Clone, PartialEq)]
pub enum IpcTeardropApplyOutcome {
    /// The commit was published; a fresh read shows every target holding its
    /// requested settings and nothing but teardrops changed.
    Applied(IpcTeardropApplied),
    /// Nothing was written.
    Refused(IpcTeardropRefusal),
    /// The commit was published, but what was read after it does not prove
    /// the requested state. The board may hold some or all of the change.
    Uncertain { reason: String },
}

impl KiCadIpcClient {
    /// KiCad's `GetItems` answer for every pad and via on one board, as
    /// KiCad sent it and not decoded. The checked-in fixture capture is taken
    /// from this.
    pub fn get_teardrop_items_response_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<prost_types::Any> {
        let command = kiapi::common::commands::GetItems {
            header: Some(header_for(document)),
            types: vec![
                common::KiCadObjectType::KotPcbPad as i32,
                common::KiCadObjectType::KotPcbVia as i32,
            ],
        };
        let Some(any) = self.send_command(&command, "kiapi.common.commands.GetItems")? else {
            anyhow::bail!("KiCad returned no GetItems answer for the board's pads and vias");
        };
        if !crate::builders::any_is(&any, "kiapi.common.commands.GetItemsResponse") {
            anyhow::bail!(
                "KiCad answered GetItems with '{}', not a GetItemsResponse",
                any.type_url
            );
        }
        let response: kiapi::common::commands::GetItemsResponse = unpack_any(&any)?;
        ensure_item_request_ok(response.status, "pad and via retrieval")?;
        Ok(any)
    }

    /// Every pad and via on one board with its native teardrop settings.
    ///
    /// Refuses with [`TeardropReadError::SettingsMissing`] when any item
    /// lacks the settings: on a KiCad before 10.0.7 every item does.
    pub fn get_teardrop_targets_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<Vec<IpcTeardropTarget>> {
        let response = self.get_teardrop_items_response_in(document.clone())?;
        let response: kiapi::common::commands::GetItemsResponse = unpack_any(&response)?;
        let references = footprint_references(&self.footprints_in(document)?);
        teardrop_targets_from_items(&response.items, &references)
    }

    /// Every placed footprint, decoded, by KIID.
    fn footprints_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<BTreeMap<String, board::FootprintInstance>> {
        let items = self.get_items_in(document, common::KiCadObjectType::KotPcbFootprint)?;
        let mut footprints = BTreeMap::new();
        for item in &items {
            if !crate::builders::any_is(item, FOOTPRINT_INSTANCE) {
                continue;
            }
            let footprint = board::FootprintInstance::decode(item.value.as_slice())
                .context("KiCad returned an unreadable footprint instance")?;
            let kiid = kiid_value(footprint.id.clone());
            anyhow::ensure!(
                !kiid.is_empty(),
                "KiCad returned a footprint without a KIID"
            );
            anyhow::ensure!(
                footprints.insert(kiid.clone(), footprint).is_none(),
                "KiCad listed footprint {kiid} twice"
            );
        }
        Ok(footprints)
    }

    /// Every teardrop zone KiCad has generated on one board.
    pub fn get_teardrop_zones_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<Vec<IpcTeardropZone>> {
        let items = self.get_items_in(document, common::KiCadObjectType::KotPcbZone)?;
        teardrop_zones_from_items(&items)
    }

    /// Whether the board is in KiCad's legacy teardrop mode, read from
    /// KiCad's own serialization of the live board. In that mode no rebuild
    /// creates teardrops (`TEARDROP_MANAGER::UpdateTeardrops` returns at
    /// once), so the settings would not reach copper.
    pub fn board_uses_legacy_teardrops_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<bool> {
        let text = self.save_document_to_string_in(document)?;
        legacy_teardrops_in_board_text(&text)
    }

    /// Apply `edits` in one KiCad commit, then prove the result.
    ///
    /// Pads go through the shared footprint update path
    /// ([`Self::update_footprint_pads_in`]): their whole footprint is sent
    /// back as read with only the targeted pads' `teardrop` replaced, and the
    /// read-back must match it field for field. Vias are sent back as read
    /// with only their settings replaced, in the same commit, and compared the
    /// same way. Refuses with nothing written for a stale target, a
    /// legacy-mode board, or a footprint that owns groups (KiCad 10's
    /// footprint message does not carry them, so the write would drop them).
    /// Last, KiCad's serialization of the board must differ from the one
    /// before only in teardrop settings and generated teardrop zones.
    pub fn set_teardrop_settings_in(
        &self,
        document: common::DocumentSpecifier,
        edits: &[IpcTeardropEdit],
    ) -> Result<IpcTeardropApplyOutcome> {
        let before_text = self.save_document_to_string_in(document.clone())?;
        if legacy_teardrops_in_board_text(&before_text)? {
            return Ok(IpcTeardropApplyOutcome::Refused(
                IpcTeardropRefusal::LegacyTeardrops,
            ));
        }
        let zones_before = self.get_teardrop_zones_in(document.clone())?;
        if edits.is_empty() {
            return Ok(IpcTeardropApplyOutcome::Applied(IpcTeardropApplied {
                readback: Vec::new(),
                zones_after: zones_before.clone(),
                zones_before,
                carried_pads: 0,
                changed_fields: Vec::new(),
            }));
        }
        let stale = |reason: String| {
            Ok(IpcTeardropApplyOutcome::Refused(
                IpcTeardropRefusal::Stale { reason },
            ))
        };
        let items = self.pad_and_via_items_in(document.clone())?;
        let mut via_updates = Vec::new();
        let mut vias_before = Vec::new();
        let mut pad_edits: BTreeMap<&str, &IpcTeardropEdit> = BTreeMap::new();
        let mut footprints: BTreeSet<String> = BTreeSet::new();
        for edit in edits {
            let Some(item) = items.get(&edit.uuid) else {
                return stale(format!("{} is no longer on the board", edit.uuid));
            };
            match edit.kind {
                IpcTeardropTargetKind::Via => {
                    if !crate::builders::any_is(item, VIA) {
                        return stale(format!("{} is no longer a via", edit.uuid));
                    }
                    let original = board::Via::decode(item.value.as_slice())
                        .context("KiCad returned an unreadable via")?;
                    if original
                        .teardrop
                        .as_ref()
                        .and_then(IpcTeardropSettings::from_proto)
                        != Some(edit.current.clone())
                    {
                        return stale(format!("via {} changed since it was read", edit.uuid));
                    }
                    let mut via = original.clone();
                    edit.settings
                        .write_to(via.teardrop.get_or_insert_with(Default::default));
                    via_updates.push(crate::builders::pack_any(&via, VIA));
                    vias_before.push((original, via));
                }
                IpcTeardropTargetKind::Pad => {
                    if !crate::builders::any_is(item, PAD) {
                        return stale(format!("{} is no longer a pad", edit.uuid));
                    }
                    let pad = board::Pad::decode(item.value.as_slice())
                        .context("KiCad returned an unreadable pad")?;
                    let parent = kiid_value(pad.parent.clone());
                    if parent.is_empty() || Some(&parent) != edit.footprint_uuid.as_ref() {
                        return stale(format!(
                            "pad {} no longer belongs to footprint {}",
                            edit.uuid,
                            edit.footprint_uuid.as_deref().unwrap_or("<none>")
                        ));
                    }
                    if pad
                        .teardrop
                        .as_ref()
                        .and_then(IpcTeardropSettings::from_proto)
                        != Some(edit.current.clone())
                    {
                        return stale(format!("pad {} changed since it was read", edit.uuid));
                    }
                    pad_edits.insert(&edit.uuid, edit);
                    footprints.insert(parent);
                }
            }
        }

        let owned = footprint_owned_groups(&before_text)?;
        let grouped: Vec<_> = footprints
            .iter()
            .filter_map(|footprint| {
                let count = owned.get(footprint).copied().unwrap_or(0);
                (count > 0).then(|| (footprint.clone(), count))
            })
            .collect();
        if !grouped.is_empty() {
            let references = footprint_references(&self.footprints_in(document.clone())?);
            return Ok(IpcTeardropApplyOutcome::Refused(
                IpcTeardropRefusal::FootprintOwnsGroups {
                    footprints: grouped
                        .into_iter()
                        .map(|(id, count)| {
                            let reference = references.get(&id).cloned().unwrap_or_default();
                            (id, reference, count)
                        })
                        .collect(),
                },
            ));
        }

        let footprints: Vec<String> = footprints.into_iter().collect();
        let required: BTreeSet<String> = pad_edits.keys().map(|id| id.to_string()).collect();
        let mut carried_pads = 0;
        // The vias go in the same commit; their read-back is part of the
        // evidence the shared path settles on.
        let via_evidence = |client: &Self| -> Result<(Vec<String>, Vec<String>)> {
            let items = client.pad_and_via_items_in(document.clone())?;
            let mut to_expected = Vec::new();
            let mut to_before = Vec::new();
            for (original, sent) in &vias_before {
                let id = kiid_value(original.id.clone());
                let Some(Ok(observed)) = items
                    .get(&id)
                    .map(|item| board::Via::decode(item.value.as_slice()))
                else {
                    to_expected.push(format!("via {id} missing"));
                    to_before.push(format!("via {id} missing"));
                    continue;
                };
                let prefixed = |fields: Vec<String>| {
                    fields
                        .into_iter()
                        .map(|field| format!("via {id} {field}"))
                        .collect::<Vec<_>>()
                };
                to_expected.extend(prefixed(via_differences(sent, &observed)));
                to_before.extend(prefixed(via_differences(original, &observed)));
            }
            Ok((to_expected, to_before))
        };
        let update = self.update_footprint_pads_in(
            document.clone(),
            "Set teardrop settings",
            &footprints,
            &required,
            via_updates,
            via_evidence,
            |_, pad| {
                let id = kiid_value(pad.id.clone());
                let Some(edit) = pad_edits.get(id.as_str()) else {
                    carried_pads += 1;
                    return Ok(false);
                };
                edit.settings
                    .write_to(pad.teardrop.get_or_insert_with(Default::default));
                Ok(true)
            },
        );
        let uncertain = |reason: String| Ok(IpcTeardropApplyOutcome::Uncertain { reason });
        let (pads_before, pads_after) = match update {
            Ok(footprint_pads::FootprintPadUpdate::Applied { before, after }) => (before, after),
            // Every edit changes its target, so something is always sent.
            Ok(footprint_pads::FootprintPadUpdate::Unchanged) => {
                return uncertain("the footprint update path sent nothing".to_string())
            }
            Ok(footprint_pads::FootprintPadUpdate::Refused(error)) => {
                return Ok(IpcTeardropApplyOutcome::Refused(
                    IpcTeardropRefusal::Rejected {
                        reason: format!("{error:#}"),
                    },
                ))
            }
            Ok(footprint_pads::FootprintPadUpdate::Uncertain { mismatches, reason }) => {
                return uncertain(if mismatches.is_empty() {
                    reason
                } else {
                    format!("{reason}: {}", mismatches.join("; "))
                })
            }
            Err(error) if error.downcast_ref::<FootprintEditTargetError>().is_some() => {
                return stale(format!("{error:#}"))
            }
            Err(error) => return Err(error),
        };

        let after = (|| -> Result<_> {
            let items = self.pad_and_via_items_in(document.clone())?;
            let zones = self.get_teardrop_zones_in(document.clone())?;
            let text = self.save_document_to_string_in(document.clone())?;
            Ok((items, zones, text))
        })();
        let (items_after, zones_after, after_text) = match after {
            Ok(after) => after,
            Err(error) => return uncertain(format!("the read after the commit failed: {error:#}")),
        };
        // The shared path proved every sent footprint and via holds exactly
        // what was sent, which differs from before only in the targets'
        // teardrop settings. Report those differences as read back.
        let mut changed_fields = Vec::new();
        for (original, _) in &vias_before {
            let id = kiid_value(original.id.clone());
            if let Some(Ok(observed)) = items_after
                .get(&id)
                .map(|item| board::Via::decode(item.value.as_slice()))
            {
                changed_fields.extend(
                    via_differences(original, &observed)
                        .into_iter()
                        .map(|field| format!("via {id} {field}")),
                );
            }
        }
        for (original, observed) in pads_before.iter().zip(&pads_after) {
            changed_fields.extend(footprint_differences(original, observed));
        }

        let references = footprint_references(&self.footprints_in(document)?);
        let targets: Vec<prost_types::Any> = edits
            .iter()
            .filter_map(|edit| items_after.get(&edit.uuid).cloned())
            .collect();
        let readback = match teardrop_targets_from_items(&targets, &references) {
            Ok(readback) => readback,
            Err(error) => return uncertain(format!("the read after the commit failed: {error:#}")),
        };
        for edit in edits {
            let Some(held) = readback.iter().find(|target| target.uuid == edit.uuid) else {
                return uncertain(format!(
                    "{} is missing from the read after the commit",
                    edit.uuid
                ));
            };
            let differing = held.settings.differing_fields(&edit.settings);
            if !differing.is_empty() {
                return uncertain(format!(
                    "{} reads back with {} other than requested",
                    edit.uuid,
                    differing
                        .iter()
                        .map(|(field, held, asked)| format!("{field} {held} (asked {asked})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        let changes = changes_besides_teardrops(&before_text, &after_text, edits)?;
        if !changes.is_empty() {
            return uncertain(format!(
                "the commit changed more than teardrops: {}",
                changes.join("; ")
            ));
        }
        Ok(IpcTeardropApplyOutcome::Applied(IpcTeardropApplied {
            readback,
            zones_before,
            zones_after,
            carried_pads,
            changed_fields,
        }))
    }

    /// Every pad and via on the board by KIID, enumerated with `GetItems`.
    fn pad_and_via_items_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<BTreeMap<String, prost_types::Any>> {
        let response = self.get_teardrop_items_response_in(document)?;
        let response: kiapi::common::commands::GetItemsResponse = unpack_any(&response)?;
        let mut by_id = BTreeMap::new();
        for item in response.items {
            if !(crate::builders::any_is(&item, PAD) || crate::builders::any_is(&item, VIA)) {
                continue;
            }
            let id = item_kiid(&item)?;
            anyhow::ensure!(
                by_id.insert(id.clone(), item).is_none(),
                "KiCad listed {id} twice"
            );
        }
        Ok(by_id)
    }
}

fn footprint_references(
    footprints: &BTreeMap<String, board::FootprintInstance>,
) -> BTreeMap<String, String> {
    footprints
        .iter()
        .map(|(id, footprint)| {
            (
                id.clone(),
                crate::builders::footprint_reference(footprint).to_string(),
            )
        })
        .collect()
}

/// Every difference between two forms of one via, by field name. The net is
/// compared by name and teardrop settings as KiCad means them, as for pads
/// in [`footprint_differences`]; `zone_layer_overrides`, a KiCad-derived
/// implementation detail, is not compared.
pub fn via_differences(expected: &board::Via, observed: &board::Via) -> Vec<String> {
    let mut out = Vec::new();
    macro_rules! compare {
        ($($field:ident),*) => {
            $(
                if expected.$field != observed.$field {
                    out.push(stringify!($field).to_string());
                }
            )*
        };
    }
    compare!(id, position, pad_stack, locked, r#type, parent, is_free);
    let net = |via: &board::Via| {
        via.net
            .as_ref()
            .map(|net| net.name.clone())
            .unwrap_or_default()
    };
    if net(expected) != net(observed) {
        out.push("net".to_string());
    }
    let teardrop = |via: &board::Via| {
        via.teardrop
            .as_ref()
            .map(|t| IpcTeardropSettings::from_proto(t).ok_or(*t))
    };
    if teardrop(expected) != teardrop(observed) {
        out.push("teardrop".to_string());
    }
    out
}

/// Groups each footprint owns directly, by footprint KIID, from KiCad's
/// board serialization: KiCad 10 leaves them out of `FootprintInstance`.
/// A footprint listed twice refuses rather than guessing which one.
pub fn footprint_owned_groups(board_text: &str) -> Result<BTreeMap<String, usize>> {
    let root =
        copperpour_sexp::parse_sexp(board_text).context("KiCad's board text did not parse")?;
    let mut owned = BTreeMap::new();
    for footprint in root.find_all("footprint") {
        let id = footprint
            .find_str("uuid")
            .context("KiCad's board text has a footprint without a uuid")?;
        anyhow::ensure!(
            owned
                .insert(id.to_string(), footprint.find_all("group").len())
                .is_none(),
            "KiCad's board text lists footprint {id} twice"
        );
    }
    Ok(owned)
}

/// Whether a node is a KiCad-generated teardrop zone: `(zone … (attr
/// (teardrop …)))`.
fn is_teardrop_zone(node: &copperpour_sexp::SexpNode) -> bool {
    node.head() == Some("zone")
        && node
            .find("attr")
            .is_some_and(|attr| attr.find("teardrop").is_some())
}

/// A board with every `(teardrops …)` settings list and generated teardrop
/// zone removed, at any depth.
fn without_teardrops(node: &copperpour_sexp::SexpNode) -> copperpour_sexp::SexpNode {
    use copperpour_sexp::SexpNode;
    match node {
        SexpNode::List(children) => SexpNode::List(
            children
                .iter()
                .filter(|child| child.head() != Some("teardrops") && !is_teardrop_zone(child))
                .map(without_teardrops)
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What differs between two serializations of one board besides teardrops,
/// as readable paths; and any pad or via not among `edits` whose
/// `(teardrops …)` settings changed. Empty when the change was teardrops
/// only.
pub fn changes_besides_teardrops(
    before: &str,
    after: &str,
    edits: &[IpcTeardropEdit],
) -> Result<Vec<String>> {
    use copperpour_sexp::SexpNode;
    let before = copperpour_sexp::parse_sexp(before).context("board text before did not parse")?;
    let after = copperpour_sexp::parse_sexp(after).context("board text after did not parse")?;
    let mut changes = Vec::new();
    diff_nodes(
        "kicad_pcb",
        &without_teardrops(&before),
        &without_teardrops(&after),
        &mut changes,
    );
    let settings_of = |tree: &SexpNode| -> BTreeMap<String, Option<SexpNode>> {
        let mut out = BTreeMap::new();
        let items = tree.find_all("via").into_iter().chain(
            tree.find_all("footprint")
                .into_iter()
                .flat_map(|footprint| footprint.find_all("pad")),
        );
        for item in items {
            if let Some(id) = item.find_str("uuid") {
                out.insert(id.to_string(), item.find("teardrops").cloned());
            }
        }
        out
    };
    let (held_before, held_after) = (settings_of(&before), settings_of(&after));
    for (id, settings) in &held_before {
        if edits.iter().any(|edit| &edit.uuid == id) {
            continue;
        }
        if held_after.get(id) != Some(settings) {
            changes.push(format!("teardrop settings of untargeted item {id}"));
        }
    }
    Ok(changes)
}

/// The innermost differing nodes between two trees, as paths. Bounded: the
/// first few are enough to say what changed.
fn diff_nodes(
    path: &str,
    before: &copperpour_sexp::SexpNode,
    after: &copperpour_sexp::SexpNode,
    out: &mut Vec<String>,
) {
    use copperpour_sexp::SexpNode;
    const LIMIT: usize = 8;
    if before == after || out.len() >= LIMIT {
        return;
    }
    let label = |node: &SexpNode| -> String {
        let head = node.head().unwrap_or("?");
        match node.find_str("uuid") {
            Some(id) => format!("{head}[{id}]"),
            None => head.to_string(),
        }
    };
    match (before, after) {
        (SexpNode::List(a), SexpNode::List(b)) if a.len() == b.len() && a.first() == b.first() => {
            let here = format!("{path}/{}", label(before));
            for (x, y) in a.iter().zip(b) {
                diff_nodes(&here, x, y, out);
            }
        }
        (SexpNode::List(_), SexpNode::List(_)) => {
            out.push(format!("{path}/{} changed shape", label(before)))
        }
        _ => out.push(format!("{path}: value changed")),
    }
}

fn item_kiid(item: &prost_types::Any) -> Result<String> {
    let id = if crate::builders::any_is(item, PAD) {
        board::Pad::decode(item.value.as_slice())
            .context("KiCad returned an unreadable pad")?
            .id
    } else if crate::builders::any_is(item, VIA) {
        board::Via::decode(item.value.as_slice())
            .context("KiCad returned an unreadable via")?
            .id
    } else {
        anyhow::bail!(
            "KiCad answered a pad or via lookup with '{}'",
            item.type_url
        );
    };
    let id = kiid_value(id);
    anyhow::ensure!(!id.is_empty(), "KiCad returned a pad or via without a KIID");
    Ok(id)
}

/// Decode pads and vias from a `GetItems` answer. Other item types (KiCad
/// may add tracks when vias are asked for) are skipped.
pub fn teardrop_targets_from_items(
    items: &[prost_types::Any],
    references: &BTreeMap<String, String>,
) -> Result<Vec<IpcTeardropTarget>> {
    let mut targets = Vec::new();
    for item in items {
        if crate::builders::any_is(item, PAD) {
            let pad = board::Pad::decode(item.value.as_slice())
                .context("KiCad returned an unreadable pad")?;
            let uuid = kiid_value(pad.id.clone());
            anyhow::ensure!(!uuid.is_empty(), "KiCad returned a pad without a KIID");
            let settings = pad
                .teardrop
                .as_ref()
                .and_then(IpcTeardropSettings::from_proto)
                .ok_or_else(|| TeardropReadError::SettingsMissing {
                    kind: IpcTeardropTargetKind::Pad,
                    uuid: uuid.clone(),
                })?;
            let footprint = kiid_value(pad.parent.clone());
            targets.push(IpcTeardropTarget {
                kind: IpcTeardropTargetKind::Pad,
                reference: references.get(&footprint).cloned(),
                footprint_uuid: (!footprint.is_empty()).then_some(footprint),
                pad_number: Some(pad.number.clone()),
                net: pad.net.map(|net| net.name).unwrap_or_default(),
                position: pad
                    .position
                    .map(point_in_mm)
                    .unwrap_or(IpcVector2 { x: 0.0, y: 0.0 }),
                uuid,
                settings,
            });
        } else if crate::builders::any_is(item, VIA) {
            let via = board::Via::decode(item.value.as_slice())
                .context("KiCad returned an unreadable via")?;
            let uuid = kiid_value(via.id.clone());
            anyhow::ensure!(!uuid.is_empty(), "KiCad returned a via without a KIID");
            let settings = via
                .teardrop
                .as_ref()
                .and_then(IpcTeardropSettings::from_proto)
                .ok_or_else(|| TeardropReadError::SettingsMissing {
                    kind: IpcTeardropTargetKind::Via,
                    uuid: uuid.clone(),
                })?;
            targets.push(IpcTeardropTarget {
                kind: IpcTeardropTargetKind::Via,
                footprint_uuid: None,
                reference: None,
                pad_number: None,
                net: via.net.map(|net| net.name).unwrap_or_default(),
                position: via
                    .position
                    .map(point_in_mm)
                    .unwrap_or(IpcVector2 { x: 0.0, y: 0.0 }),
                uuid,
                settings,
            });
        }
    }
    Ok(targets)
}

/// Decode the teardrop zones from a `GetItems` answer for zones.
pub fn teardrop_zones_from_items(items: &[prost_types::Any]) -> Result<Vec<IpcTeardropZone>> {
    let mut zones = Vec::new();
    for item in items {
        if !crate::builders::any_is(item, ZONE) {
            continue;
        }
        let zone = board::Zone::decode(item.value.as_slice())
            .context("KiCad returned an unreadable zone")?;
        if board::ZoneType::try_from(zone.r#type).ok() != Some(board::ZoneType::ZtTeardrop) {
            continue;
        }
        let (net, teardrop_type) = match zone.settings.as_ref() {
            Some(board::zone::Settings::CopperSettings(copper)) => (
                copper
                    .net
                    .as_ref()
                    .map(|net| net.name.clone())
                    .unwrap_or_default(),
                copper
                    .teardrop
                    .as_ref()
                    .and_then(|t| board::ZoneTeardropType::try_from(t.r#type).ok())
                    .map(|t| match t {
                        board::ZoneTeardropType::ZtdtViaPad => "via_pad",
                        board::ZoneTeardropType::ZtdtTrackEnd => "track_end",
                        board::ZoneTeardropType::ZtdtUnspecified => "unspecified",
                        board::ZoneTeardropType::ZtdtNone => "none",
                        board::ZoneTeardropType::ZtdtUnknown => "unknown",
                    })
                    .unwrap_or("unknown"),
            ),
            _ => (String::new(), "unknown"),
        };
        let outline = zone
            .outline
            .as_ref()
            .map(|outline| outline.encode_to_vec())
            .unwrap_or_default();
        let fill: Vec<u8> = zone
            .filled_polygons
            .iter()
            .flat_map(|filled| filled.encode_to_vec())
            .collect();
        zones.push(IpcTeardropZone {
            uuid: kiid_value(zone.id.clone()),
            net,
            layers: zone
                .layers
                .iter()
                .map(|layer| {
                    board::BoardLayer::try_from(*layer)
                        .ok()
                        .and_then(crate::builders::layer_name)
                        .unwrap_or("unknown")
                        .to_string()
                })
                .collect(),
            teardrop_type: teardrop_type.to_string(),
            filled: zone.filled,
            outline_digest: format!("{:016x}", fnv1a64(&outline)),
            fill_digest: format!("{:016x}", fnv1a64(&fill)),
        });
    }
    zones.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    Ok(zones)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Whether a board serialization declares `(legacy_teardrops yes)` in its
/// `(general …)` block. KiCad 10 writes `no` on every board it saves; an
/// absent token is a board that predates it and is read as `no` too.
pub fn legacy_teardrops_in_board_text(text: &str) -> Result<bool> {
    let root = copperpour_sexp::parse_sexp(text).context("KiCad's board text did not parse")?;
    anyhow::ensure!(
        root.head() == Some("kicad_pcb"),
        "KiCad's board text is not a kicad_pcb document"
    );
    match root
        .find("general")
        .and_then(|general| general.find_str("legacy_teardrops"))
    {
        None | Some("no") => Ok(false),
        Some("yes") => Ok(true),
        Some(other) => anyhow::bail!("KiCad's board text declares legacy_teardrops '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(major: u32, minor: u32, patch: u32) -> IpcKiCadVersion {
        IpcKiCadVersion {
            major,
            minor,
            patch,
            full_version: format!("{major}.{minor}.{patch}"),
        }
    }

    #[test]
    fn teardrop_settings_need_kicad_10_0_7_and_a_known_version() {
        assert!(!supports_teardrop_settings(&version(10, 0, 6)));
        assert!(!supports_teardrop_settings(&version(9, 0, 9)));
        assert!(!supports_teardrop_settings(&version(0, 0, 0)));
        assert!(supports_teardrop_settings(&version(10, 0, 7)));
        assert!(supports_teardrop_settings(&version(10, 0, 8)));
        assert!(supports_teardrop_settings(&version(10, 99, 0)));
    }

    fn board_text(general: &str, body: &str) -> String {
        format!("(kicad_pcb\n\t(version 20260206)\n\t(general\n\t\t(thickness 1.6){general}\n\t)\n{body})\n")
    }

    #[test]
    fn legacy_teardrop_mode_is_read_from_the_general_block() {
        let read = |general: &str| legacy_teardrops_in_board_text(&board_text(general, ""));
        assert!(!read("\n\t\t(legacy_teardrops no)").unwrap());
        assert!(read("\n\t\t(legacy_teardrops yes)").unwrap());
        assert!(!read("").unwrap());
        assert!(read("\n\t\t(legacy_teardrops maybe)").is_err());
        assert!(legacy_teardrops_in_board_text("(not_a_board)").is_err());
    }

    #[test]
    fn owned_groups_count_per_footprint_and_board_groups_do_not() {
        let text = board_text(
            "",
            "\t(footprint \"A\" (uuid \"fa\") (group \"\" (uuid \"g1\") (members \"x\")))\n\
             \t(footprint \"B\" (uuid \"fb\"))\n\
             \t(group \"\" (uuid \"g2\") (members \"fa\" \"fb\"))\n",
        );
        let owned = footprint_owned_groups(&text).unwrap();
        assert_eq!(owned.get("fa"), Some(&1));
        assert_eq!(owned.get("fb"), Some(&0));
        let twice = board_text(
            "",
            "\t(footprint \"A\" (uuid \"fa\"))\n\t(footprint \"A\" (uuid \"fa\"))\n",
        );
        assert!(footprint_owned_groups(&twice).is_err());
    }

    fn edit(uuid: &str) -> IpcTeardropEdit {
        IpcTeardropEdit {
            kind: IpcTeardropTargetKind::Via,
            uuid: uuid.to_string(),
            footprint_uuid: None,
            current: IpcTeardropSettings {
                enabled: false,
                curved_edges: false,
                allow_multiple_track_segments: true,
                prefer_zone_connection: true,
                max_length_nm: None,
                max_width_nm: None,
                best_length_ratio: 0.5,
                best_width_ratio: 1.0,
                max_track_width_ratio: 0.9,
            },
            settings: IpcTeardropSettings {
                enabled: true,
                curved_edges: false,
                allow_multiple_track_segments: true,
                prefer_zone_connection: true,
                max_length_nm: None,
                max_width_nm: None,
                best_length_ratio: 0.5,
                best_width_ratio: 1.0,
                max_track_width_ratio: 0.9,
            },
        }
    }

    const VIA_A: &str = "\t(via (at 1 1) (size 0.6) (net \"GND\") (uuid \"va\"))\n";
    const VIA_A_ON: &str =
        "\t(via (at 1 1) (size 0.6) (net \"GND\") (teardrops (enabled yes)) (uuid \"va\"))\n";
    const VIA_B: &str = "\t(via (at 2 2) (size 0.6) (net \"GND\") (uuid \"vb\"))\n";
    const VIA_B_ON: &str =
        "\t(via (at 2 2) (size 0.6) (net \"GND\") (teardrops (enabled yes)) (uuid \"vb\"))\n";
    const TEARDROP_ZONE: &str =
        "\t(zone (net \"GND\") (layer \"F.Cu\") (attr (teardrop (type padvia))))\n";

    #[test]
    fn targeted_settings_and_generated_zones_are_the_only_allowed_changes() {
        let before = board_text("", &format!("{VIA_A}{VIA_B}"));
        let after = board_text("", &format!("{VIA_A_ON}{VIA_B}{TEARDROP_ZONE}"));
        assert!(changes_besides_teardrops(&before, &after, &[edit("va")])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_untargeted_settings_change_or_any_other_change_is_reported() {
        let before = board_text("", &format!("{VIA_A}{VIA_B}"));
        let untargeted = board_text("", &format!("{VIA_A_ON}{VIA_B_ON}"));
        let changes = changes_besides_teardrops(&before, &untargeted, &[edit("va")]).unwrap();
        assert_eq!(changes, ["teardrop settings of untargeted item vb"]);

        let moved = board_text(
            "",
            &format!("{VIA_A_ON}{}", VIA_B.replace("(at 2 2)", "(at 3 2)")),
        );
        let changes = changes_besides_teardrops(&before, &moved, &[edit("va")]).unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(changes[0].contains("via[vb]"), "{changes:?}");

        let removed = board_text("", VIA_A_ON);
        let changes = changes_besides_teardrops(&before, &removed, &[edit("va")]).unwrap();
        assert!(!changes.is_empty());
    }

    #[test]
    fn length_limits_round_trip_with_zero_as_no_constraint() {
        let mut message = board::PadTeardropSettings::default();
        let settings = IpcTeardropSettings {
            max_length_nm: Some(1_500_000),
            max_width_nm: None,
            ..edit("x").settings
        };
        settings.write_to(&mut message);
        assert_eq!(message.max_width.as_ref().map(|d| d.value_nm), Some(0));
        assert_eq!(message.mode, board::PadTeardropMode::PtmEnabled as i32);
        assert_eq!(IpcTeardropSettings::from_proto(&message), Some(settings));
        message.max_length = None;
        assert_eq!(
            IpcTeardropSettings::from_proto(&message)
                .unwrap()
                .max_length_nm,
            None
        );
        message.mode = board::PadTeardropMode::PtmUnknown as i32;
        assert_eq!(IpcTeardropSettings::from_proto(&message), None);
    }
}
