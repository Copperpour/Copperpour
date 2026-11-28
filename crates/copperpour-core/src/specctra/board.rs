//! Read routing features and footprint identities from a KiCad board snapshot.

use super::{
    finite_number, finite_um, finite_um_f64, point_um, positive_um, snap_nm_um, CopperZoneModel,
    FootprintModel, LiveFootprint, LockedRouting, LockedTrackModel, LockedViaModel,
    ManifestPointUm, ManifestRuleArea, ManifestZoneCount, NonCopperPadIdentity, NonCopperPadModel,
    PadModel, PadShape, PadstackKey, PinIdentity, RuleAreaKind, RuleAreaModel, Side, UM_PER_MM,
};
use anyhow::{bail, Context, Result};
use copperpour_sexp::{parse_sexp, SexpNode};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn locked_routing(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
    copper_layers: &[String],
) -> Result<LockedRouting> {
    let arc_count = tree.find_all("arc").len();
    if arc_count > 0 {
        let locked = tree
            .find_all("arc")
            .iter()
            .filter(|arc| arc.find_str("locked") == Some("yes"))
            .count();
        if locked > 0 {
            bail!(
                "unsupported first routing profile: board contains {locked} locked routed arc(s), which cannot be represented without approximation"
            );
        }
        bail!(
            "unsupported first routing profile: board contains {arc_count} existing routed arc(s)"
        );
    }

    let mut routing = LockedRouting::default();
    for segment in tree.find_all("segment") {
        if segment.find_str("locked") != Some("yes") {
            bail!("unsupported first routing profile: board contains an existing track segment that is not locked");
        }
        let layer = segment
            .find_str("layer")
            .context("locked track segment has no layer")?
            .to_string();
        if !copper_layers.contains(&layer) {
            bail!("locked track segment uses unsupported layer '{layer}'");
        }
        let net = segment
            .find("net")
            .and_then(|node| resolve_net(node, net_table))
            .context("locked track segment has no connected net")?;
        let (start_x_um, start_y_um) = point_um(segment, "start")?;
        let (end_x_um, end_y_um) = point_um(segment, "end")?;
        if start_x_um == end_x_um && start_y_um == end_y_um {
            bail!("locked track segment has zero length");
        }
        routing.tracks.push(LockedTrackModel {
            start_x_um,
            start_y_um,
            end_x_um,
            end_y_um,
            width_um: positive_um(
                segment.find("width").and_then(|width| width.get_f64(1)),
                "locked track width",
            )?,
            layer,
            net,
        });
    }
    for via in tree.find_all("via") {
        if via.find_str("locked") != Some("yes") {
            bail!("unsupported first routing profile: board contains an existing via that is not locked");
        }
        if let Some(kind) = via.find_str("type") {
            bail!("unsupported locked via type '{kind}'; only through vias are supported");
        }
        let layers = via
            .find("layers")
            .and_then(SexpNode::children)
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .filter_map(SexpNode::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        // KiCad writes a through via's span as its two outer layers, on any
        // number of copper layers.
        let through = [
            copper_layers[0].clone(),
            copper_layers[copper_layers.len() - 1].clone(),
        ];
        if layers != through {
            bail!(
                "unsupported locked via layer span [{}]; expected [{}]",
                layers.join(", "),
                through.join(", ")
            );
        }
        let net = via
            .find("net")
            .and_then(|node| resolve_net(node, net_table))
            .context("locked via has no connected net")?;
        let (x_um, y_um) = point_um(via, "at")?;
        refuse_per_layer_padstack(
            via,
            &format!(
                "locked via at ({}, {}) mm",
                x_um as f64 / 1000.0,
                -y_um as f64 / 1000.0
            ),
        )?;
        let diameter_um = positive_um(
            via.find("size").and_then(|size| size.get_f64(1)),
            "locked via diameter",
        )?;
        let drill = via.find("drill").context("locked via has no drill")?;
        if drill.get(1).and_then(SexpNode::as_str) == Some("oval") {
            bail!("unsupported locked via with oval drill");
        }
        let drill_um = positive_um(drill.get_f64(1), "locked via drill")?;
        if drill_um >= diameter_um {
            bail!("locked via drill is not smaller than its diameter");
        }
        routing.vias.push(LockedViaModel {
            x_um,
            y_um,
            diameter_um,
            drill_um,
            net,
        });
    }
    routing.tracks.sort_by(|left, right| {
        (
            &left.net,
            &left.layer,
            left.start_x_um,
            left.start_y_um,
            left.end_x_um,
            left.end_y_um,
        )
            .cmp(&(
                &right.net,
                &right.layer,
                right.start_x_um,
                right.start_y_um,
                right.end_x_um,
                right.end_y_um,
            ))
    });
    routing.vias.sort_by(|left, right| {
        (&left.net, left.x_um, left.y_um).cmp(&(&right.net, right.x_um, right.y_um))
    });
    Ok(routing)
}

/// The board's copper zones (#430) and rule-area keep-outs (#81). Each zone is
/// exported as its outline, one `plane` per copper layer it covers, and each
/// representable rule area as one keep-out (`keepout`, `wire_keepout`, or
/// `via_keepout`) per copper layer it covers. A pour/pad/footprint-only rule
/// area is recorded as `FillOnly` and written to no DSN keep-out. Anything
/// those cannot express without approximation is refused: a zone inside a
/// footprint, a copper zone without a net, placement zones, an outline with a
/// cutout, an arc edge, or a layer off the copper stack.
pub(super) fn board_zones_and_rule_areas(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
    copper_layers: &[String],
) -> Result<(Vec<CopperZoneModel>, Vec<RuleAreaModel>)> {
    for footprint in tree.find_all("footprint") {
        if !footprint.find_all("zone").is_empty() {
            let reference = property_value(footprint, "Reference").unwrap_or("?");
            bail!("unsupported routing profile: footprint {reference} contains a zone");
        }
    }
    let mut zones = Vec::new();
    let mut rule_areas = Vec::new();
    for zone in tree.find_all("zone") {
        let name = zone.find_str("name").unwrap_or("");
        let is_rule_area = zone.find("keepout").is_some() || zone.find("placement").is_some();
        let prefix = if is_rule_area {
            "rule area"
        } else {
            "copper zone"
        };
        let label = if name.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix} '{name}'")
        };
        let covered = zone_copper_layers(zone, &label, copper_layers)?;
        if is_rule_area {
            if let Some(net) = zone
                .find("net")
                .and_then(|node| resolve_net(node, net_table))
            {
                bail!("unsupported routing profile: {label} cannot belong to net '{net}'");
            }
            let kind = rule_area_kind(zone, &label)?;
            let outline = zone_straight_outline(zone, &label, None, exact_rule_area_um)?;
            rule_areas.push(RuleAreaModel {
                label: label.clone(),
                kind,
                layers: covered,
                outline,
            });
        } else {
            let net = zone
                .find("net")
                .and_then(|node| resolve_net(node, net_table))
                .with_context(|| {
                    format!("unsupported routing profile: {label} has no net, which KiCad exports under an invented net name")
                })?;
            let outline = zone_straight_outline(zone, &label, Some(&net), finite_um)?;
            zones.push(CopperZoneModel {
                net,
                layers: covered,
                outline,
            });
        }
    }
    Ok((zones, rule_areas))
}

fn zone_copper_layers(
    zone: &SexpNode,
    label: &str,
    copper_layers: &[String],
) -> Result<Vec<String>> {
    let named = match (zone.find("layer"), zone.find("layers")) {
        (Some(layer), None) => layer
            .get(1)
            .and_then(SexpNode::as_str)
            .map(|name| vec![name.to_string()])
            .unwrap_or_default(),
        (None, Some(layers)) => layers
            .children()
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .filter_map(SexpNode::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    if named.is_empty() {
        bail!("{label} has no layer");
    }
    if named == ["*.Cu"] {
        return Ok(copper_layers.to_vec());
    }
    if let Some(layer) = named.iter().find(|name| !copper_layers.contains(name)) {
        bail!("unsupported routing profile: {label} is on '{layer}', which is not a copper layer of the board");
    }
    if named.iter().collect::<BTreeSet<_>>().len() != named.len() {
        bail!("{label} repeats a layer");
    }
    Ok(copper_layers
        .iter()
        .filter(|layer| named.contains(layer))
        .cloned()
        .collect())
}

fn zone_straight_outline(
    zone: &SexpNode,
    label: &str,
    net: Option<&str>,
    coordinate_um: fn(Option<f64>, &str) -> Result<i64>,
) -> Result<Vec<(i64, i64)>> {
    let polygons = zone.find_all("polygon");
    if polygons.len() != 1 {
        let on_net = net.map_or_else(String::new, |net| format!(" on net '{net}'"));
        bail!(
            "unsupported routing profile: {label}{on_net} has {} outlines; an outline with a cutout is not exported",
            polygons.len()
        );
    }
    let points = polygons[0]
        .find("pts")
        .and_then(SexpNode::children)
        .with_context(|| format!("{label} outline has no points"))?;
    let mut outline = Vec::with_capacity(points.len());
    for point in points.iter().skip(1) {
        if point.head() != Some("xy") {
            bail!(
                "unsupported routing profile: {label} outline has a '{}' point; only straight edges are exported",
                point.head().unwrap_or("?")
            );
        }
        let x = coordinate_um(point.get_f64(1), &format!("{label} x"))?;
        let y = coordinate_um(point.get_f64(2), &format!("{label} y"))?
            .checked_neg()
            .context("zone y is outside the supported coordinate range")?;
        outline.push((x, y));
    }
    if outline.iter().collect::<BTreeSet<_>>().len() < 3 {
        bail!("{label} outline has fewer than three distinct points");
    }
    Ok(outline)
}

/// Schema 5 and native keep-out correlation store whole micrometres. A rounded
/// boundary could shrink the forbidden area, so refuse anything not exactly on
/// that grid before either writer runs. Comparing the round trip in millimetres
/// accepts canonical KiCad decimals such as 0.009 without a floating multiply
/// spuriously making them fractional micrometres.
pub(super) fn exact_rule_area_um(value: Option<f64>, label: &str) -> Result<i64> {
    let mm = finite_number(value, label)?;
    let um = finite_um(Some(mm), label)?;
    if mm != um as f64 / UM_PER_MM {
        bail!("unsupported routing profile: {label} must lie on the whole-micrometre grid; rule-area vertices are never rounded");
    }
    Ok(um)
}

fn rule_area_kind(zone: &SexpNode, label: &str) -> Result<RuleAreaKind> {
    if let Some(placement) = zone.find("placement") {
        match placement.find_str("enabled") {
            Some("no") | None => {}
            Some("yes") => bail!(
                "unsupported routing profile: {label} enables room placement, which Specctra DSN keep-outs cannot represent"
            ),
            Some(other) => bail!(
                "unsupported routing profile: {label} has unsupported placement mode '{other}'"
            ),
        }
    }
    let keepout = zone
        .find("keepout")
        .with_context(|| format!("unsupported routing profile: {label} has no keepout rules"))?;
    let flag = |key: &str| -> Result<bool> {
        match keepout.find_str(key) {
            Some("not_allowed") => Ok(true),
            Some("allowed") => Ok(false),
            Some(other) => bail!(
                "unsupported routing profile: {label} has unsupported keepout setting ({key} {other})"
            ),
            None => {
                bail!("unsupported routing profile: {label} is missing keepout '{key}' rule")
            }
        }
    };
    let keepout_tracks = flag("tracks")?;
    let keepout_vias = flag("vias")?;
    for key in ["pads", "copperpour", "footprints"] {
        if let Some(value) = keepout.find_str(key) {
            if !matches!(value, "allowed" | "not_allowed") {
                bail!(
                    "unsupported routing profile: {label} has unsupported keepout setting ({key} {value})"
                );
            }
        }
    }
    match (keepout_tracks, keepout_vias) {
        (true, true) => Ok(RuleAreaKind::Keepout),
        (true, false) => Ok(RuleAreaKind::WireKeepout),
        (false, true) => Ok(RuleAreaKind::ViaKeepout),
        (false, false) => Ok(RuleAreaKind::FillOnly),
    }
}

/// How many copper zones of each net cover each copper layer of the board,
/// sorted by layer then net: the inventory the manifest records.
pub(super) fn zone_inventory(zones: &[CopperZoneModel]) -> Vec<ManifestZoneCount> {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for zone in zones {
        for layer in &zone.layers {
            *counts
                .entry((layer.as_str(), zone.net.as_str()))
                .or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|((layer, net), count)| ManifestZoneCount {
            layer: layer.to_string(),
            net: net.to_string(),
            count,
        })
        .collect()
}

/// All validated rule areas of the board, canonically sorted for the manifest (#81).
pub(super) fn rule_area_inventory(rule_areas: &[RuleAreaModel]) -> Vec<ManifestRuleArea> {
    let mut entries = rule_areas
        .iter()
        .map(|area| ManifestRuleArea {
            kind: area.kind,
            layers: area.layers.clone(),
            outline_um: area
                .outline
                .iter()
                .map(|&(x_um, y_um)| ManifestPointUm { x_um, y_um })
                .collect(),
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

/// The copper-zone inventory of a live board, read the way the export reads it.
pub(crate) fn board_zone_inventory(board_source: &str) -> Result<Vec<ManifestZoneCount>> {
    let tree = parse_sexp(board_source).context("parse KiCad board for its copper zones")?;
    let copper_layers = copper_layers(&tree)?;
    let (zones, _) =
        board_zones_and_rule_areas(&tree, &top_level_net_table(&tree), &copper_layers)?;
    Ok(zone_inventory(&zones))
}

/// The rule-area inventory of a live board, read the way the export reads it (#81).
pub(crate) fn board_rule_area_inventory(board_source: &str) -> Result<Vec<ManifestRuleArea>> {
    let tree = parse_sexp(board_source).context("parse KiCad board for its rule areas")?;
    let copper_layers = copper_layers(&tree)?;
    let (_, rule_areas) =
        board_zones_and_rule_areas(&tree, &top_level_net_table(&tree), &copper_layers)?;
    Ok(rule_area_inventory(&rule_areas))
}

/// The board's copper layers from top to bottom: `F.Cu`, `In1.Cu`..`In<N-2>.Cu`
/// and `B.Cu`, the order Specctra layer indices follow. KiCad 10 writes the
/// layer table top to bottom (`F.Cu` 0, `In1.Cu` 4, `In2.Cu` 6, `B.Cu` 2) but
/// writes a pad's layer list by ordinal (`F.Cu`, `B.Cu`, `In2.Cu`), so the
/// order is rebuilt from the names rather than taken from either list; a set
/// with a gap, a repeat or a missing outer layer is refused.
pub(super) fn copper_layers(tree: &SexpNode) -> Result<Vec<String>> {
    let table = tree.find("layers").context("board has no layers table")?;
    let copper = copperpour_sexp::layers::table_layers(table)
        .into_iter()
        .filter(copperpour_sexp::layers::Layer::is_copper)
        .map(|layer| layer.name)
        .collect::<Vec<_>>();
    let distinct = copper.iter().collect::<BTreeSet<_>>();
    let stack = copperpour_sexp::layers::CopperStack::from_layer_names(
        copper.iter().map(String::as_str),
    )
    .ok()
    .filter(|_| distinct.len() == copper.len())
    .with_context(|| {
        format!(
            "unsupported routing profile: copper layers must be F.Cu, In1.Cu..In<N-2>.Cu and B.Cu, got [{}]",
            copper.join(", ")
        )
    })?;
    Ok(stack_layer_names(stack.count()))
}

/// The copper layers the board's layer table marks `power`. KiCad's exporter
/// writes them as DSN `power` layers, where Freerouting lays no track, and
/// every other copper layer (`signal`, `mixed`, `jumper`) as `signal`.
pub(super) fn power_layers(tree: &SexpNode) -> BTreeSet<String> {
    tree.find("layers")
        .map(copperpour_sexp::layers::table_layers)
        .unwrap_or_default()
        .into_iter()
        .filter(|layer| layer.is_copper() && layer.kind == "power")
        .map(|layer| layer.name)
        .collect()
}

/// `F.Cu`, `In1.Cu`..`In<count-2>.Cu`, `B.Cu`.
pub(crate) fn stack_layer_names(count: u32) -> Vec<String> {
    std::iter::once("F.Cu".to_string())
        .chain((1..count.saturating_sub(1)).map(|k| format!("In{k}.Cu")))
        .chain(std::iter::once("B.Cu".to_string()))
        .collect()
}

pub(super) fn footprints(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
    copper_layers: &[String],
) -> Result<Vec<FootprintModel>> {
    // Across the board: a pad UUID names one physical pad.
    let mut pad_uuids = BTreeSet::new();
    let mut output = Vec::new();
    let mut references = BTreeSet::new();
    for footprint in tree.find_all("footprint") {
        let layer = footprint
            .find_str("layer")
            .context("footprint has no layer")?;
        let side = match layer {
            "F.Cu" => Side::Front,
            "B.Cu" => Side::Back,
            _ => bail!("unsupported first routing profile: footprint on '{layer}'"),
        };
        if footprint.find("clearance").is_some() {
            bail!("unsupported first routing profile: footprint has a local clearance override");
        }
        let reference = property_value(footprint, "Reference")
            .filter(|value| !value.is_empty())
            .context("footprint has no Reference property")?
            .to_string();
        if !references.insert(reference.clone()) {
            bail!("duplicate footprint reference '{reference}'");
        }
        let kiid = footprint
            .find_str("uuid")
            .context("footprint has no UUID")?
            .to_string();
        let at = footprint.find("at").context("footprint has no position")?;
        let x_um = finite_um(at.get_f64(1), "footprint x")?;
        let y_um = -finite_um(at.get_f64(2), "footprint y")?;
        let rotation_degrees = finite_number(at.get_f64(3).or(Some(0.0)), "footprint rotation")?;
        // KiCad keeps a bottom footprint's pads mirrored about its local X
        // axis, and its Specctra export unflips them: the image is the front
        // one and the `place` is `back` at KiCad's rotation plus 180, which
        // the router mirrors back about the image's Y axis.
        let place_degrees = match side {
            Side::Front => rotation_degrees,
            Side::Back => pin_rotation(rotation_degrees + 180.0, 0.0),
        };
        let image_name = format!("copperpour_image_{reference}");

        let mut pads = Vec::new();
        let mut non_copper_pads = Vec::new();
        for pad in footprint.find_all("pad") {
            // An SMD pad on no copper layer (a paste aperture) is not routed:
            // KiCad's export skips it, and this one only records it (#458).
            let pad_layers = pad
                .find("layers")
                .and_then(SexpNode::children)
                .unwrap_or(&[])
                .iter()
                .skip(1)
                .filter_map(SexpNode::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            if pad.get(2).and_then(SexpNode::as_str) == Some("smd")
                && !pad_layers.is_empty()
                && !pad_layers.iter().any(|layer| layer.ends_with(".Cu"))
            {
                let number = pad.get(1).and_then(SexpNode::as_str).unwrap_or("");
                let uuid = pad
                    .find_str("uuid")
                    .filter(|uuid| !uuid.is_empty())
                    .with_context(|| format!("pad on no copper layer of {reference} has no UUID"))?
                    .to_string();
                if !pad_uuids.insert(uuid.clone()) {
                    bail!(
                        "pad on no copper layer of {reference} repeats the UUID {uuid} of another pad"
                    );
                }
                non_copper_pads.push(NonCopperPadModel {
                    number: number.to_string(),
                    uuid,
                    layers: pad_layers,
                });
                continue;
            }
            let number = pad
                .get(1)
                .and_then(SexpNode::as_str)
                .filter(|number| !number.is_empty())
                .context("footprint contains an unnumbered pad")?
                .to_string();
            // A repeated number is a separate physical pad; its UUID tells
            // it apart in the manifest (#338).
            let uuid = pad
                .find_str("uuid")
                .filter(|uuid| !uuid.is_empty())
                .with_context(|| format!("pad {reference}-{number} has no UUID"))?
                .to_string();
            if !pad_uuids.insert(uuid.clone()) {
                bail!("pad {reference}-{number} repeats the UUID {uuid} of another pad");
            }
            let pad_type = pad
                .get(2)
                .and_then(SexpNode::as_str)
                .context("pad has no type")?;
            if !matches!(pad_type, "smd" | "thru_hole") {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} has type '{pad_type}'"
                );
            }
            refuse_per_layer_padstack(pad, &format!("pad {reference}-{number}"))?;
            let shape = match pad.get(3).and_then(SexpNode::as_str) {
                Some("circle") => PadShape::Circle,
                Some("rect") => PadShape::Rect,
                Some("roundrect") => PadShape::RoundRect,
                Some("oval") => PadShape::Oval,
                Some(other) => bail!(
                    "unsupported first routing profile: pad {reference}-{number} has shape '{other}'"
                ),
                None => bail!("pad {reference}-{number} has no shape"),
            };
            let at = pad.find("at").context("pad has no position")?;
            let x_um = snap_nm_um(finite_um_f64(at.get_f64(1), "pad x")?);
            let file_y_um = finite_um_f64(at.get_f64(2), "pad y")?;
            let pad_degrees = finite_number(at.get_f64(3).or(Some(0.0)), "pad rotation")?;
            // Unflipped, a bottom pad's local Y and its turn change sign.
            let (y_um, pin_degrees) = match side {
                Side::Front => (
                    snap_nm_um(-file_y_um),
                    pin_rotation(pad_degrees, rotation_degrees),
                ),
                Side::Back => (
                    snap_nm_um(file_y_um),
                    pin_rotation(rotation_degrees, pad_degrees),
                ),
            };
            let size = pad.find("size").context("pad has no size")?;
            let size_x_um = positive_um(size.get_f64(1), "pad width")?;
            let size_y_um = positive_um(size.get_f64(2), "pad height")?;
            if shape == PadShape::Circle && size_x_um != size_y_um {
                bail!("circle pad {reference}-{number} does not have equal X/Y size");
            }
            let corner_radius_um = (shape == PadShape::RoundRect)
                .then(|| {
                    roundrect_corner_radius_um(
                        pad,
                        size_x_um,
                        size_y_um,
                        &format!("{reference}-{number}"),
                    )
                })
                .transpose()?;
            if pad.find("clearance").is_some() {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} has a local clearance override"
                );
            }
            if pad.find_str("remove_unused_layers") == Some("yes") {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} removes copper on unused layers"
                );
            }
            if let Some(offset) = pad.find("offset") {
                let offset_x_um = finite_um(offset.get_f64(1), "pad offset x")?;
                let offset_y_um = finite_um(offset.get_f64(2), "pad offset y")?;
                if offset_x_um != 0 || offset_y_um != 0 {
                    bail!(
                        "unsupported first routing profile: pad {reference}-{number} has a non-zero shape offset"
                    );
                }
            }
            let layers = pad
                .find("layers")
                .and_then(SexpNode::children)
                .unwrap_or(&[])
                .iter()
                .skip(1)
                .filter_map(SexpNode::as_str)
                .filter(|layer| layer.ends_with(".Cu") || *layer == "*.Cu")
                .map(str::to_string)
                .collect::<Vec<_>>();
            let (layers, drill_um) = if pad_type == "smd" {
                // An SMD pad is on its footprint's side; the front image
                // carries it on F.Cu, as KiCad's export writes it.
                let own = match side {
                    Side::Front => "F.Cu",
                    Side::Back => "B.Cu",
                };
                if layers != [own.to_string()] {
                    bail!(
                        "unsupported SMD pad {reference}-{number}: copper layers are [{}]",
                        layers.join(", ")
                    );
                }
                (vec!["F.Cu".to_string()], None)
            } else {
                // Every copper layer, as a wildcard or each listed once. KiCad
                // saves a pad on every layer as `*.Cu`; a list written by hand
                // or by another tool may follow the ordinal order of pad lists
                // (`F.Cu`, `B.Cu`, `In1.Cu`, ...), so it is compared as a set.
                let listed_in_full = layers.len() == copper_layers.len()
                    && layers.iter().collect::<BTreeSet<_>>()
                        == copper_layers.iter().collect::<BTreeSet<_>>();
                if !(layers == ["*.Cu".to_string()] || listed_in_full) {
                    bail!(
                        "unsupported through-hole pad {reference}-{number}: copper layers are [{}]",
                        layers.join(", ")
                    );
                }
                let drill = pad.find("drill").context("through-hole pad has no drill")?;
                if drill.get(1).and_then(SexpNode::as_str) == Some("oval") {
                    bail!("unsupported through-hole pad {reference}-{number}: oval drill");
                }
                let drill_um = positive_um(drill.get_f64(1), "pad drill")?;
                if drill_um >= size_x_um.min(size_y_um) {
                    bail!(
                        "pad {reference}-{number} drill {drill_um} um is not smaller than its copper size"
                    );
                }
                (copper_layers.to_vec(), Some(drill_um))
            };
            let net = pad
                .find("net")
                .and_then(|node| resolve_net(node, net_table));
            pads.push(PadModel {
                number,
                uuid,
                pin: String::new(),
                x_um,
                y_um,
                rotation_degrees: pin_degrees,
                net,
                padstack: PadstackKey {
                    shape,
                    layers,
                    size_x_um,
                    size_y_um,
                    corner_radius_um,
                    drill_um,
                },
            });
        }
        if pads.is_empty() {
            bail!("footprint '{reference}' has no pads");
        }
        dsn_pin_names(&reference, &mut pads)?;
        pads.sort_by(|left, right| left.pin.cmp(&right.pin));
        non_copper_pads.sort();
        output.push(FootprintModel {
            reference,
            kiid,
            image_name,
            x_um,
            y_um,
            rotation_degrees: place_degrees,
            side,
            pads,
            non_copper_pads,
        });
    }
    output.sort_by(|left, right| left.reference.cmp(&right.reference));
    Ok(output)
}

/// Every routed footprint's side and pins, by footprint UUID, named as
/// the Rust DSN exporter names them.
pub(crate) fn board_pin_identities(board_source: &str) -> Result<BTreeMap<String, LiveFootprint>> {
    let tree = parse_sexp(board_source).context("parse KiCad board snapshot")?;
    let net_table = top_level_net_table(&tree);
    let copper_layers = copper_layers(&tree)?;
    Ok(footprints(&tree, &net_table, &copper_layers)?
        .into_iter()
        .map(|footprint| {
            let reference = footprint.reference;
            let pins = footprint
                .pads
                .into_iter()
                .map(|pad| PinIdentity {
                    dsn_pin: format!("{reference}-{}", pad.pin),
                    pad_number: pad.number,
                    pad_uuid: pad.uuid,
                    net: pad.net,
                })
                .collect();
            (
                footprint.kiid,
                LiveFootprint {
                    side: footprint.side.name(),
                    pins,
                    non_copper_pads: footprint
                        .non_copper_pads
                        .into_iter()
                        .map(|pad| NonCopperPadIdentity {
                            pad_number: pad.number,
                            pad_uuid: pad.uuid,
                            layers: pad.layers,
                        })
                        .collect(),
                },
            )
        })
        .collect())
}

/// Names each pad's DSN pin. A number one pad has is its name, as before. A
/// number several pads share keeps its name on the first and gets `@1`, `@2`
/// on the others, the suffix KiCad's own Specctra export gives them. Unlike
/// KiCad's, the order is by pad UUID, not file order, so the names don't
/// depend on how the pads are listed (#338). A name that is also another
/// pad's number is refused rather than renamed.
fn dsn_pin_names(reference: &str, pads: &mut [PadModel]) -> Result<()> {
    let mut by_number: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, pad) in pads.iter().enumerate() {
        by_number.entry(pad.number.clone()).or_default().push(index);
    }
    for (number, mut indices) in by_number.clone() {
        indices.sort_by(|left, right| pads[*left].uuid.cmp(&pads[*right].uuid));
        for (position, index) in indices.into_iter().enumerate() {
            pads[index].pin = if position == 0 {
                number.clone()
            } else {
                let alias = format!("{number}@{position}");
                if by_number.contains_key(&alias) {
                    bail!(
                        "footprint '{reference}' repeats pad number '{number}', and its DSN name '{alias}' is another pad's number"
                    );
                }
                alias
            };
        }
    }
    Ok(())
}

/// KiCad's corner radius, `min(width, height) * roundrect_rratio` rounded to
/// the nanometre, then down to the micrometre: a smaller radius only moves the
/// exported corner outward. KiCad writes a chamfered pad as `roundrect` too; its
/// cut corners are not represented, so it is refused.
pub(super) fn roundrect_corner_radius_um(
    pad: &SexpNode,
    size_x_um: i64,
    size_y_um: i64,
    label: &str,
) -> Result<i64> {
    if pad
        .find("chamfer")
        .and_then(SexpNode::children)
        .is_some_and(|corners| corners.len() > 1)
    {
        bail!("unsupported first routing profile: pad {label} has chamfered corners");
    }
    let ratio = finite_number(
        pad.find("roundrect_rratio")
            .and_then(|node| node.get_f64(1)),
        "pad roundrect_rratio",
    )?;
    if !(0.0..=0.5).contains(&ratio) {
        bail!("pad {label} roundrect_rratio {ratio} is outside 0..=0.5");
    }
    let radius_nm = (size_x_um.min(size_y_um) as f64 * 1_000.0 * ratio).round() as i64;
    Ok(radius_nm / 1_000)
}

/// A pad's file angle includes its footprint's, which `(place ...)` applies
/// again, so a pin turns by the difference. In [0, 360) and to six significant
/// digits, as KiCad writes it with `%.6g`.
pub(super) fn pin_rotation(pad_degrees: f64, footprint_degrees: f64) -> f64 {
    let turn = (pad_degrees - footprint_degrees).rem_euclid(360.0);
    format!("{turn:.5e}")
        .parse::<f64>()
        .expect("a formatted finite number parses")
        % 360.0
}

pub(super) fn top_level_net_table(tree: &SexpNode) -> BTreeMap<String, String> {
    tree.find_all("net")
        .into_iter()
        .filter_map(|net| {
            let id = net.get(1)?.as_str()?;
            let name = net.get(2)?.as_str()?;
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

fn resolve_net(net: &SexpNode, table: &BTreeMap<String, String>) -> Option<String> {
    if let Some(name) = net.get(2).and_then(SexpNode::as_str) {
        return (!name.is_empty()).then(|| name.to_string());
    }
    let value = net.get(1)?.as_str()?;
    if let Some(name) = table.get(value).filter(|name| !name.is_empty()) {
        return Some(name.clone());
    }
    // KiCad 10 stores the net name directly as `(net "NAME")`. Older board
    // files used `(net id "NAME")`, handled by the branch above.
    (!value.is_empty()).then(|| value.to_string())
}

fn property_value<'a>(node: &'a SexpNode, property_name: &str) -> Option<&'a str> {
    node.find_all("property")
        .into_iter()
        .find(|property| property.get(1).and_then(SexpNode::as_str) == Some(property_name))?
        .get(2)?
        .as_str()
}

/// KiCad omits `(padstack ...)` for a normal padstack. Any other mode can give
/// B.Cu or inner layers a shape the front geometry does not describe (#842).
fn refuse_per_layer_padstack(item: &SexpNode, label: &str) -> Result<()> {
    match item.find("padstack").map(|padstack| padstack.find_str("mode")) {
        None | Some(Some("normal")) => Ok(()),
        Some(Some(mode)) => bail!(
            "unsupported first routing profile: {label} has a '{mode}' padstack, which can shape copper layers differently"
        ),
        Some(None) => {
            bail!("unsupported first routing profile: {label} has a padstack with no mode")
        }
    }
}
