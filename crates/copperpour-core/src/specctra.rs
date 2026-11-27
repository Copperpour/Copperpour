//! Fail-closed KiCad PCB snapshot to Specctra DSN lowering.
//!
//! The input is the exact string returned by KiCad's IPC
//! `SaveDocumentToString`; this module never reads or edits the live board
//! file. The first supported profile is intentionally narrow and rejects
//! every feature it cannot represent without approximation.

mod board;
mod native;
mod outline;
mod writer;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) use board::{
    board_pin_identities, board_rule_area_inventory, board_zone_inventory, stack_layer_names,
};
#[cfg(test)]
pub(crate) use native::adopt_native_dsn;

use board::{
    board_zones_and_rule_areas, copper_layers, footprints, locked_routing, power_layers,
    top_level_net_table,
};
use outline::{outline_arc_error_um, simple_closed_outline};
use writer::{build_manifest, build_pcb, component_side, serialize_and_validate};

use crate::hex::sha256_hex;
use anyhow::{bail, Context, Result};
use copperpour_ipc::IpcEffectiveRoutingRules;
use copperpour_sexp::{parse_sexp, SexpNode};
use serde::{Deserialize, Serialize};
use specctra::structure as dsn;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const UM_PER_MM: f64 = 1_000.0;
const DSN_RESOLUTION: f32 = 10.0;
const DEFAULT_OUTLINE_ARC_ERROR_UM: f64 = 5.0;
const MAX_OUTLINE_ARC_SEGMENTS: usize = 1_000_000;
const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;

#[derive(Debug, Clone)]
pub(crate) struct ExportBundle {
    pub dsn: String,
    pub manifest: String,
    pub source_sha256: String,
    pub component_count: usize,
    pub pad_count: usize,
    pub net_count: usize,
    pub class_count: usize,
    /// Copper layers of the exported board, counted from its layer table.
    pub copper_layer_count: usize,
    /// Copper zones of the exported board, each written as one `plane` per
    /// copper layer it covers.
    pub copper_zone_count: usize,
    /// Rule areas of the exported board, each written as one keep-out per
    /// copper layer it covers.
    pub rule_area_count: usize,
    /// The sides the exported footprints sit on: `front`, `back` or `both`.
    pub component_side: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PadShape {
    Circle,
    Rect,
    #[serde(rename = "roundrect")]
    RoundRect,
    /// An obround: a stadium whose width is the pad's shorter side (#384).
    Oval,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PadstackKey {
    shape: PadShape,
    layers: Vec<String>,
    size_x_um: i64,
    size_y_um: i64,
    corner_radius_um: Option<i64>,
    drill_um: Option<i64>,
}

#[derive(Debug, Clone)]
struct PadModel {
    /// KiCad's pad number, which several pads of a footprint may share.
    number: String,
    /// The pad's own KiCad UUID: its physical identity (#338).
    uuid: String,
    /// Its pin name in the DSN image, unique in the footprint: see
    /// `board::dsn_pin_names`.
    pin: String,
    x_um: f64,
    y_um: f64,
    /// Relative to the footprint, like `x_um`/`y_um`.
    rotation_degrees: f64,
    net: Option<String>,
    padstack: PadstackKey,
}

#[derive(Debug, Clone)]
struct FootprintModel {
    reference: String,
    kiid: String,
    image_name: String,
    x_um: i64,
    y_um: i64,
    /// The `place` rotation: KiCad's for a front footprint, KiCad's plus 180
    /// for a bottom one, as KiCad's own Specctra export writes it.
    rotation_degrees: f64,
    side: Side,
    pads: Vec<PadModel>,
    /// Pads on no copper layer, such as the paste apertures of a QFN's
    /// exposed pad. KiCad's Specctra export leaves them out of the image, and
    /// so does this one; the manifest still lists them (#458).
    non_copper_pads: Vec<NonCopperPadModel>,
}

/// A pad on no copper layer: not routed, but part of the footprint the
/// manifest is checked against (#458).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct NonCopperPadModel {
    /// KiCad's pad number, often empty for a paste aperture.
    number: String,
    uuid: String,
    /// Its KiCad layers, all technical (`F.Paste`, `B.Mask`, ...).
    layers: Vec<String>,
}

/// The board side a footprint sits on, by its `F.Cu` or `B.Cu` layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Front,
    Back,
}

impl Side {
    fn name(self) -> &'static str {
        match self {
            Side::Front => "front",
            Side::Back => "back",
        }
    }
}

#[derive(Debug, Clone)]
struct LockedTrackModel {
    start_x_um: i64,
    start_y_um: i64,
    end_x_um: i64,
    end_y_um: i64,
    width_um: i64,
    layer: String,
    net: String,
}

#[derive(Debug, Clone)]
struct LockedViaModel {
    x_um: i64,
    y_um: i64,
    diameter_um: i64,
    drill_um: i64,
    net: String,
}

/// One copper zone of the board: its net, the copper layers it covers (top to
/// bottom) and its outline, in DSN micrometres. Its fill is not exported: like
/// KiCad's own exporter, the DSN gives Freerouting the outline as a `plane`.
#[derive(Debug, Clone)]
struct CopperZoneModel {
    net: String,
    layers: Vec<String>,
    outline: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleAreaKind {
    /// Forbids both tracks and vias (`keepout`).
    Keepout,
    /// Forbids tracks while allowing vias (`wire_keepout`).
    WireKeepout,
    /// Forbids vias while allowing tracks (`via_keepout`).
    ViaKeepout,
}

impl RuleAreaKind {
    pub(crate) fn keeps_out_tracks(self) -> bool {
        matches!(self, Self::Keepout | Self::WireKeepout)
    }

    pub(crate) fn keeps_out_vias(self) -> bool {
        matches!(self, Self::Keepout | Self::ViaKeepout)
    }

    fn dsn_kind(self) -> dsn::KeepoutKind {
        match self {
            Self::Keepout => dsn::KeepoutKind::Normal,
            Self::WireKeepout => dsn::KeepoutKind::Wire,
            Self::ViaKeepout => dsn::KeepoutKind::Via,
        }
    }
}

struct RuleAreaModel {
    kind: RuleAreaKind,
    layers: Vec<String>,
    outline: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, Default)]
struct LockedRouting {
    tracks: Vec<LockedTrackModel>,
    vias: Vec<LockedViaModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RuleKey {
    track_width_um: i64,
    clearance_um: i64,
    via_diameter_um: i64,
    via_drill_um: i64,
}

type RuleGroups = BTreeMap<RuleKey, Vec<String>>;
type NetClassNames = BTreeMap<String, String>;

/// 2 adds each pin's `pad_uuid` and lets pads share a number (#338).
/// 3 adds the copper-zone inventory, `zones` (#430).
/// 4 adds each component's pads on no copper layer, `non_copper_pads` (#458).
/// 5 adds the rule-area keep-out inventory, `rule_areas` (#81).
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    schema_version: u32,
    board_path: String,
    source_sha256: String,
    coordinate_unit: String,
    resolution: u32,
    supported_profile: SupportedProfile,
    layers: Vec<ManifestLayer>,
    components: Vec<ManifestComponent>,
    nets: Vec<ManifestNet>,
    padstacks: Vec<ManifestPadstack>,
    /// Absent before schema 3, which had no copper zones (#430).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    zones: Vec<ManifestZoneCount>,
    /// Absent before schema 5, which had no rule areas (#81).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rule_areas: Vec<ManifestRuleArea>,
}

/// How many copper zones of one net cover one copper layer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestZoneCount {
    pub layer: String,
    pub net: String,
    pub count: usize,
}

/// One DSN polygon vertex in micrometres (Specctra Y-up coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestPointUm {
    pub x_um: i64,
    pub y_um: i64,
}

/// One validated rule-area keep-out on the board, in canonical order (#81).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestRuleArea {
    pub kind: RuleAreaKind,
    pub layers: Vec<String>,
    pub outline_um: Vec<ManifestPointUm>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SupportedProfile {
    copper_layers: u32,
    component_side: String,
    pad_shapes: Vec<String>,
    existing_routing: bool,
    #[serde(default)]
    locked_track_count: usize,
    #[serde(default)]
    locked_via_count: usize,
    copper_zones: bool,
    /// Absent before schema 5, which had no rule areas (#81).
    #[serde(default)]
    rule_areas: bool,
    custom_rules: bool,
    outline: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestLayer {
    kicad_name: String,
    dsn_name: String,
    index: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestComponent {
    reference: String,
    kiid: String,
    image_name: String,
    x_um: i64,
    y_um: i64,
    rotation_degrees: f64,
    side: String,
    pads: Vec<ManifestPin>,
    /// Pads on no copper layer: not in the DSN, checked against the live
    /// board on import. Absent before schema 4 (#458).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    non_copper_pads: Vec<ManifestNonCopperPad>,
}

/// A pad on no copper layer, recorded but not routed (#458).
#[derive(Debug, Deserialize, Serialize)]
struct ManifestNonCopperPad {
    pad_number: String,
    pad_uuid: String,
    layers: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestPin {
    pad_number: String,
    /// Absent in a schema 1 manifest, written before repeated pad numbers
    /// were exported (#338).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pad_uuid: Option<String>,
    dsn_pin: String,
    net: Option<String>,
    padstack_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestNet {
    name: String,
    pins: Vec<String>,
    class_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestPadstack {
    name: String,
    purpose: String,
    shape: PadShape,
    layers: Vec<String>,
    size_x_um: i64,
    size_y_um: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corner_radius_um: Option<i64>,
    drill_um: Option<i64>,
}

/// One pad as the export names it, which the SES importer checks a
/// manifest's pins against on the live board (#338).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PinIdentity {
    pub dsn_pin: String,
    pub pad_number: String,
    pub pad_uuid: String,
    pub net: Option<String>,
}

/// A pad on no copper layer as the manifest records it (#458).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NonCopperPadIdentity {
    pub pad_number: String,
    pub pad_uuid: String,
    pub layers: Vec<String>,
}

/// A routed footprint of the live board: the side it sits on, its pins, and
/// its pads on no copper layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveFootprint {
    pub side: &'static str,
    pub pins: Vec<PinIdentity>,
    pub non_copper_pads: Vec<NonCopperPadIdentity>,
}

/// Import-owned snapshot model. No Rust boundary is generated or substituted.
pub(crate) struct RoutingSnapshot {
    board_path: std::path::PathBuf,
    tree: SexpNode,
    source_sha256: String,
    copper_layers: Vec<String>,
    power_layers: BTreeSet<String>,
    footprints: Vec<FootprintModel>,
    net_pins: BTreeMap<String, Vec<String>>,
    class_nets: RuleGroups,
    net_classes: NetClassNames,
    padstack_names: BTreeMap<PadstackKey, String>,
    via_names: BTreeMap<RuleKey, String>,
    locked_routing: LockedRouting,
    zones: Vec<CopperZoneModel>,
    rule_areas: Vec<RuleAreaModel>,
}

pub(crate) fn prepare_export(
    board_path: &Path,
    board_source: &str,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<RoutingSnapshot> {
    let tree = parse_sexp(board_source).context("parse KiCad IPC board snapshot")?;
    if tree.head() != Some("kicad_pcb") {
        bail!("KiCad IPC snapshot root is not 'kicad_pcb'");
    }

    let copper_layers = copper_layers(&tree)?;
    let power_layers = power_layers(&tree);
    let net_table = top_level_net_table(&tree);
    let locked_routing = locked_routing(&tree, &net_table, &copper_layers)?;
    let (zones, rule_areas) = board_zones_and_rule_areas(&tree, &net_table, &copper_layers)?;
    let footprints = footprints(&tree, &net_table, &copper_layers)?;
    if footprints.is_empty() {
        bail!("supported routing profile requires at least one footprint");
    }

    let mut net_pins: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for footprint in &footprints {
        for pad in &footprint.pads {
            if let Some(net) = &pad.net {
                net_pins
                    .entry(net.clone())
                    .or_default()
                    .push(format!("{}-{}", footprint.reference, pad.pin));
            }
        }
    }
    if net_pins.is_empty() {
        bail!("supported routing profile requires at least one connected pad");
    }
    for pins in net_pins.values_mut() {
        pins.sort();
        pins.dedup();
    }
    // A plane names a DSN net, and the DSN only has nets with pads.
    if let Some(zone) = zones.iter().find(|zone| !net_pins.contains_key(&zone.net)) {
        bail!(
            "unsupported routing profile: copper zone on net '{}', which has no pad",
            zone.net
        );
    }

    let (class_nets, net_classes) = normalize_rules(&net_pins, effective_rules)?;

    let mut padstack_keys = BTreeSet::new();
    for footprint in &footprints {
        for pad in &footprint.pads {
            padstack_keys.insert(pad.padstack.clone());
        }
    }
    let padstack_names: BTreeMap<PadstackKey, String> = padstack_keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, format!("copperpour_pad_{:04}", index + 1)))
        .collect();

    let via_keys: BTreeSet<RuleKey> = class_nets.keys().cloned().collect();
    let via_names: BTreeMap<RuleKey, String> = via_keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, format!("copperpour_via_{:04}", index + 1)))
        .collect();

    Ok(RoutingSnapshot {
        board_path: board_path.to_path_buf(),
        tree,
        source_sha256: sha256_hex(board_source.as_bytes()),
        copper_layers,
        power_layers,
        footprints,
        net_pins,
        class_nets,
        net_classes,
        padstack_names,
        via_names,
        locked_routing,
        zones,
        rule_areas,
    })
}

#[cfg(test)]
pub(crate) fn export_dsn(
    board_path: &Path,
    board_source: &str,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<ExportBundle> {
    prepare_export(board_path, board_source, effective_rules)?.export_rust()
}

impl RoutingSnapshot {
    pub(crate) fn export_rust(&self) -> Result<ExportBundle> {
        let outline = simple_closed_outline(
            &self.tree,
            outline_arc_error_um(&self.board_path, &self.tree)?,
        )?;
        let pcb = build_pcb(
            &self.board_path,
            &self.copper_layers,
            &self.power_layers,
            &outline,
            &self.footprints,
            &self.net_pins,
            &self.class_nets,
            &self.net_classes,
            &self.padstack_names,
            &self.via_names,
            &self.locked_routing,
            &self.zones,
            &self.rule_areas,
        )?;
        self.bundle(serialize_and_validate(pcb)?, RUST_OUTLINE)
    }

    fn bundle(&self, dsn: String, outline: &str) -> Result<ExportBundle> {
        let manifest = build_manifest(
            &self.board_path,
            &self.source_sha256,
            &self.copper_layers,
            &self.footprints,
            &self.net_pins,
            &self.net_classes,
            &self.padstack_names,
            &self.via_names,
            &self.locked_routing,
            &self.zones,
            &self.rule_areas,
            outline,
        )?;
        Ok(ExportBundle {
            dsn,
            manifest,
            source_sha256: self.source_sha256.clone(),
            component_count: self.footprints.len(),
            pad_count: self.footprints.iter().map(|f| f.pads.len()).sum(),
            net_count: self.net_pins.len(),
            class_count: self.class_nets.len(),
            copper_layer_count: self.copper_layers.len(),
            copper_zone_count: self.zones.len(),
            rule_area_count: self.rule_areas.len(),
            component_side: component_side(&self.footprints),
        })
    }
}

const RUST_OUTLINE: &str = "one closed loop of Edge.Cuts lines and circular arcs, with arcs flattened within board max_error";

/// The DSN name of the `index`th net class, shared with the manifest.
fn class_name(index: usize) -> String {
    format!("copperpour_class_{:04}", index + 1)
}

fn normalize_rules(
    net_pins: &BTreeMap<String, Vec<String>>,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<(RuleGroups, NetClassNames)> {
    let mut class_nets: BTreeMap<RuleKey, Vec<String>> = BTreeMap::new();
    for net in net_pins.keys() {
        let rules = effective_rules.get(net).with_context(|| {
            format!("KiCad returned no effective routing rules for net '{net}'")
        })?;
        let key = RuleKey {
            track_width_um: positive_um(rules.track_width_mm, "track width")?,
            clearance_um: non_negative_um(rules.clearance_mm, "clearance")?,
            via_diameter_um: positive_um(rules.via_diameter_mm, "via diameter")?,
            via_drill_um: positive_um(rules.via_drill_mm, "via drill")?,
        };
        if key.via_drill_um >= key.via_diameter_um {
            bail!(
                "net '{net}' has via drill {} um not smaller than diameter {} um",
                key.via_drill_um,
                key.via_diameter_um
            );
        }
        class_nets.entry(key).or_default().push(net.clone());
    }
    for nets in class_nets.values_mut() {
        nets.sort();
    }
    let mut net_classes = BTreeMap::new();
    for (index, nets) in class_nets.values().enumerate() {
        let name = class_name(index);
        for net in nets {
            net_classes.insert(net.clone(), name.clone());
        }
    }
    Ok((class_nets, net_classes))
}

fn point_um(node: &SexpNode, tag: &str) -> Result<(i64, i64)> {
    let point = node.find(tag).with_context(|| format!("missing '{tag}'"))?;
    let y = finite_um(point.get_f64(2), tag)?
        .checked_neg()
        .with_context(|| format!("{tag} is outside the supported coordinate range"))?;
    Ok((finite_um(point.get_f64(1), tag)?, y))
}

fn point_um_f64(node: &SexpNode, tag: &str) -> Result<(f64, f64)> {
    let point = node.find(tag).with_context(|| format!("missing '{tag}'"))?;
    let y = -finite_um_f64(point.get_f64(2), tag)?;
    if y >= I64_UPPER_EXCLUSIVE {
        bail!("{tag} is outside the supported coordinate range");
    }
    Ok((finite_um_f64(point.get_f64(1), tag)?, y))
}

fn finite_number(value: Option<f64>, label: &str) -> Result<f64> {
    let value = value.with_context(|| format!("missing {label}"))?;
    if !value.is_finite() {
        bail!("{label} is not finite");
    }
    Ok(value)
}

fn finite_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_number(value, label)? * UM_PER_MM;
    if value < i64::MIN as f64 || value >= I64_UPPER_EXCLUSIVE {
        bail!("{label} is outside the supported coordinate range");
    }
    Ok(value.round() as i64)
}

fn finite_um_f64(value: Option<f64>, label: &str) -> Result<f64> {
    let value_um = finite_number(value, label)? * UM_PER_MM;
    if !value_um.is_finite() || value_um < i64::MIN as f64 || value_um >= I64_UPPER_EXCLUSIVE {
        bail!("{label} is outside the supported coordinate range");
    }
    Ok(value_um)
}

/// Pin-coordinate tolerance (one DSN resolution step, 0.1 um, plus float
/// rounding margin) so KiCad's `%.6g` pin rounding matches Copperpour's
/// nanometre-accurate pad offsets.
const PIN_POSITION_TOLERANCE_UM: f64 = 1.0 / DSN_RESOLUTION as f64 + 1e-9;

fn snap_nm_um(value_um: f64) -> f64 {
    // `+ 0.0` turns `-0.0` into `0.0` so DSN writes `0` and `OrderedF64` matches.
    (value_um * 1000.0).round() / 1000.0 + 0.0
}

fn positive_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_um(value, label)?;
    if value <= 0 {
        bail!("{label} must be greater than zero");
    }
    Ok(value)
}

fn non_negative_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_um(value, label)?;
    if value < 0 {
        bail!("{label} must not be negative");
    }
    Ok(value)
}
