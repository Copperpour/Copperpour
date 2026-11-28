//! Serialize the Rust DSN and its routing manifest.

use super::board::{rule_area_inventory, zone_inventory};
use super::outline::closed_outline;
use super::{
    class_name, CopperZoneModel, FootprintModel, LockedRouting, Manifest, ManifestComponent,
    ManifestLayer, ManifestNet, ManifestNonCopperPad, ManifestPadstack, ManifestPin, PadShape,
    PadstackKey, RuleAreaModel, RuleKey, Side, SupportedProfile, DSN_RESOLUTION,
    MANIFEST_SCHEMA_VERSION,
};
use anyhow::{bail, Context, Result};
use specctra::read::{ListTokenizer, ReadDsn};
use specctra::structure as dsn;
use specctra::write::{ListWriter, WriteSes};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Cursor};
use std::path::Path;

fn dsn_closed_polygon(layer: &str, outline: &[(i64, i64)]) -> dsn::Polygon {
    dsn::Polygon {
        layer: layer.to_string(),
        width: 0.0,
        // Closed, as KiCad writes it.
        coords: closed_outline(outline)
            .into_iter()
            .map(|(x, y)| dsn::Point {
                x: x as f64,
                y: y as f64,
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_pcb(
    board_path: &Path,
    copper_layers: &[String],
    power_layers: &BTreeSet<String>,
    outline: &[(i64, i64)],
    footprints: &[FootprintModel],
    net_pins: &BTreeMap<String, Vec<String>>,
    class_nets: &BTreeMap<RuleKey, Vec<String>>,
    net_classes: &BTreeMap<String, String>,
    padstack_names: &BTreeMap<PadstackKey, String>,
    via_names: &BTreeMap<RuleKey, String>,
    locked_routing: &LockedRouting,
    zones: &[CopperZoneModel],
    rule_areas: &[RuleAreaModel],
) -> Result<dsn::Pcb> {
    let default_rule = class_nets
        .keys()
        .next()
        .context("routing model has no rule class")?;
    let layers = copper_layers
        .iter()
        .enumerate()
        .map(|(index, name)| dsn::Layer {
            name: name.clone(),
            r#type: if power_layers.contains(name) {
                "power"
            } else {
                "signal"
            }
            .to_string(),
            property: Some(dsn::Property { index }),
        })
        .collect();
    let boundary = dsn::Boundary::Path(dsn::Path {
        layer: "pcb".to_string(),
        width: 0.0,
        coords: outline
            .iter()
            .map(|(x, y)| dsn::Point {
                x: *x as f64,
                y: *y as f64,
            })
            .collect(),
    });

    let components = footprints
        .iter()
        .map(|footprint| dsn::Component {
            name: footprint.image_name.clone(),
            places: vec![dsn::Place {
                name: footprint.reference.clone(),
                x: footprint.x_um as f64,
                y: footprint.y_um as f64,
                side: footprint.side.name().to_string(),
                rotation: footprint.rotation_degrees,
                PN: None,
            }],
        })
        .collect();
    let images = footprints
        .iter()
        .map(|footprint| dsn::Image {
            name: footprint.image_name.clone(),
            outlines: Vec::new(),
            pins: footprint
                .pads
                .iter()
                .map(|pad| dsn::Pin {
                    name: padstack_names[&pad.padstack].clone(),
                    rotate: (pad.rotation_degrees != 0.0).then_some(pad.rotation_degrees),
                    id: pad.pin.clone(),
                    x: pad.x_um,
                    y: pad.y_um,
                })
                .collect(),
            keepouts: dsn::Keepouts(Vec::new()),
        })
        .collect();

    let mut padstacks = padstack_names
        .iter()
        .map(|(key, name)| padstack(name, key))
        .collect::<Vec<_>>();
    padstacks.extend(via_names.iter().map(|(rule, name)| {
        dsn::Padstack {
            name: name.clone(),
            shapes: copper_layers
                .iter()
                .map(|layer| {
                    dsn::Shape::Circle(dsn::Circle {
                        layer: layer.clone(),
                        diameter: rule.via_diameter_um as f64,
                        offset: None,
                    })
                })
                .collect(),
            attach: Some(false),
        }
    }));
    padstacks.sort_by(|left, right| left.name.cmp(&right.name));

    let nets = net_pins
        .iter()
        .map(|(name, pins)| dsn::NetPinAssignments {
            name: name.clone(),
            pins: Some(dsn::Pins {
                names: pins.clone(),
            }),
        })
        .collect();
    let classes = class_nets
        .iter()
        .enumerate()
        .map(|(index, (rule, nets))| dsn::Class {
            name: class_name(index),
            nets: nets.clone(),
            circuit: dsn::Circuit {
                use_via: via_names[rule].clone(),
            },
            rule: dsn::Rule {
                width: rule.track_width_um as f32,
                clearances: vec![dsn::Clearance {
                    value: rule.clearance_um as f32,
                    r#type: None,
                }],
            },
        })
        .collect();
    debug_assert_eq!(net_classes.len(), net_pins.len());

    let net_rules = class_nets
        .iter()
        .flat_map(|(rule, nets)| nets.iter().map(move |net| (net, rule)))
        .collect::<BTreeMap<_, _>>();
    let wires = locked_routing
        .tracks
        .iter()
        .map(|track| {
            if !net_rules.contains_key(&track.net) {
                bail!("locked track uses unknown routing net '{}'", track.net);
            }
            Ok(dsn::Wire {
                path: dsn::Path {
                    layer: track.layer.clone(),
                    width: track.width_um as f64,
                    coords: vec![
                        dsn::Point {
                            x: track.start_x_um as f64,
                            y: track.start_y_um as f64,
                        },
                        dsn::Point {
                            x: track.end_x_um as f64,
                            y: track.end_y_um as f64,
                        },
                    ],
                },
                net: track.net.clone(),
                r#type: "fix".to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let fixed_vias = locked_routing
        .vias
        .iter()
        .map(|via| {
            let rule = net_rules
                .get(&via.net)
                .with_context(|| format!("locked via uses unknown routing net '{}'", via.net))?;
            if via.diameter_um != rule.via_diameter_um || via.drill_um != rule.via_drill_um {
                bail!(
                    "locked via on net '{}' is {}:{} um, but its effective via rule is {}:{} um",
                    via.net,
                    via.diameter_um,
                    via.drill_um,
                    rule.via_diameter_um,
                    rule.via_drill_um
                );
            }
            Ok(dsn::Via {
                name: via_names[*rule].clone(),
                x: via.x_um as f64,
                y: via.y_um as f64,
                net: via.net.clone(),
                r#type: Some("fix".to_string()),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(dsn::Pcb {
        name: board_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("board.kicad_pcb")
            .to_string(),
        parser: Some(dsn::Parser {
            string_quote: Some('"'),
            space_in_quoted_tokens: Some(true),
            host_cad: Some("Copperpour".to_string()),
            host_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        resolution: dsn::Resolution {
            unit: "um".to_string(),
            value: DSN_RESOLUTION,
        },
        unit: Some("um".to_string()),
        structure: dsn::Structure {
            layers,
            boundary,
            place_boundary: None,
            planes: zones
                .iter()
                .flat_map(|zone| {
                    zone.layers.iter().map(|layer| dsn::Plane {
                        net: zone.net.clone(),
                        polygon: dsn_closed_polygon(layer, &zone.outline),
                    })
                })
                .collect(),
            keepouts: dsn::Keepouts(
                rule_areas
                    .iter()
                    .filter_map(|area| area.kind.dsn_kind().map(|kind| (area, kind)))
                    .flat_map(|(area, kind)| {
                        area.layers.iter().map(move |layer| dsn::Keepout {
                            kind: kind.clone(),
                            id_: String::new(),
                            sequence_number: None,
                            shape: dsn::Shape::Polygon(dsn_closed_polygon(layer, &area.outline)),
                            rule: None,
                        })
                    })
                    .collect(),
            ),
            via: dsn::ViaNames {
                names: via_names.values().cloned().collect(),
            },
            grids: Vec::new(),
            rules: vec![dsn::StructureRule {
                width: Some(default_rule.track_width_um as f32),
                clearances: vec![dsn::Clearance {
                    value: default_rule.clearance_um as f32,
                    r#type: None,
                }],
            }],
        },
        placement: dsn::Placement { components },
        library: dsn::Library { images, padstacks },
        network: dsn::Network { nets, classes },
        wiring: dsn::Wiring {
            wires,
            vias: fixed_vias,
        },
    })
}

fn padstack(name: &str, key: &PadstackKey) -> dsn::Padstack {
    let shapes = key
        .layers
        .iter()
        .map(|layer| match key.shape {
            PadShape::Circle => dsn::Shape::Circle(dsn::Circle {
                layer: layer.clone(),
                diameter: key.size_x_um as f64,
                offset: None,
            }),
            PadShape::Rect => dsn::Shape::Rect(dsn::Rect {
                layer: layer.clone(),
                x1: -(key.size_x_um as f64) / 2.0,
                y1: -(key.size_y_um as f64) / 2.0,
                x2: key.size_x_um as f64 / 2.0,
                y2: key.size_y_um as f64 / 2.0,
            }),
            PadShape::Oval => dsn::Shape::Path(dsn::Path {
                layer: layer.clone(),
                width: key.size_x_um.min(key.size_y_um) as f64,
                coords: oval_spine(key.size_x_um, key.size_y_um)
                    .into_iter()
                    .map(|(x, y)| dsn::Point { x, y })
                    .collect(),
            }),
            // Specctra has no rounded rectangle, so KiCad writes a polygon too.
            PadShape::RoundRect => dsn::Shape::Polygon(dsn::Polygon {
                layer: layer.clone(),
                width: 0.0,
                coords: roundrect_outline(
                    key.size_x_um,
                    key.size_y_um,
                    key.corner_radius_um.unwrap_or(0),
                )
                .into_iter()
                .map(|(x, y)| dsn::Point { x, y })
                .collect(),
            }),
        })
        .collect();
    dsn::Padstack {
        name: name.to_string(),
        shapes,
        attach: Some(false),
    }
}

/// An oval pad's spine, as KiCad's Specctra export writes it: the segment a
/// round pen of the shorter side's width sweeps to cover the pad, along the
/// longer axis and centred on the pad; zero length for an equal-sided one.
pub(super) fn oval_spine(size_x_um: i64, size_y_um: i64) -> [(f64, f64); 2] {
    let half = (size_x_um - size_y_um).abs() as f64 / 2.0;
    if size_y_um > size_x_um {
        [(0.0, -half), (0.0, half)]
    } else if size_x_um > size_y_um {
        [(-half, 0.0), (half, 0.0)]
    } else {
        [(0.0, 0.0), (0.0, 0.0)]
    }
}

/// Furthest a rounded corner's straight edges may stand outside its arc.
const ROUNDRECT_MAX_ERROR_UM: f64 = 1.0;

/// A closed, counter-clockwise outline of a rounded rectangle centred on the
/// origin. Each corner's edges are tangent to its arc, so the outline encloses
/// it and stands at most `ROUNDRECT_MAX_ERROR_UM` outside, before rounding to
/// the DSN's 0.1 um resolution.
pub(super) fn roundrect_outline(size_x_um: i64, size_y_um: i64, radius_um: i64) -> Vec<(f64, f64)> {
    use std::f64::consts::FRAC_PI_2;
    let radius = radius_um as f64;
    let half_x = size_x_um as f64 / 2.0 - radius;
    let half_y = size_y_um as f64 / 2.0 - radius;
    let segments = if radius_um == 0 {
        1
    } else {
        let max_half_step = (radius / (radius + ROUNDRECT_MAX_ERROR_UM)).acos();
        (FRAC_PI_2 / (2.0 * max_half_step)).ceil() as usize
    };
    let step = FRAC_PI_2 / segments as f64;
    let reach = radius / (step / 2.0).cos();
    let resolution = f64::from(DSN_RESOLUTION);
    // `+ 0.0` writes -0 as 0.
    let snap = |value: f64| (value * resolution).round() / resolution + 0.0;
    let mut points = Vec::new();
    for (quadrant, (centre_x, centre_y)) in [
        (half_x, half_y),
        (-half_x, half_y),
        (-half_x, -half_y),
        (half_x, -half_y),
    ]
    .into_iter()
    .enumerate()
    {
        for segment in 0..segments {
            let angle = quadrant as f64 * FRAC_PI_2 + (segment as f64 + 0.5) * step;
            points.push((
                snap(centre_x + reach * angle.cos()),
                snap(centre_y + reach * angle.sin()),
            ));
        }
    }
    points.dedup();
    points.push(points[0]);
    points
}

pub(super) fn serialize_and_validate(pcb: dsn::Pcb) -> Result<String> {
    let file = dsn::DsnFile { pcb };
    let mut bytes = Vec::new();
    {
        let mut writer = ListWriter::new(&mut bytes);
        file.write_dsn(&mut writer)
            .context("serialize Specctra DSN")?;
    }
    let mut text = String::from_utf8(bytes).context("Specctra writer emitted non-UTF-8")?;
    if text.starts_with('\n') {
        text.remove(0);
    }
    text.push('\n');

    let cursor = Cursor::new(text.as_bytes());
    let mut tokenizer = ListTokenizer::new(BufReader::new(cursor));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .map_err(|error| anyhow::anyhow!("generated DSN failed parser round-trip: {error}"))?;
    Ok(text)
}

/// The sides the board's footprints sit on, derived from them: `front`,
/// `back`, or `both`, the words `export_position_file` uses.
pub(super) fn component_side(footprints: &[FootprintModel]) -> &'static str {
    let front = footprints
        .iter()
        .any(|footprint| footprint.side == Side::Front);
    let back = footprints
        .iter()
        .any(|footprint| footprint.side == Side::Back);
    match (front, back) {
        (true, true) => "both",
        (false, true) => "back",
        _ => "front",
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_manifest(
    board_path: &Path,
    source_sha256: &str,
    copper_layers: &[String],
    footprints: &[FootprintModel],
    net_pins: &BTreeMap<String, Vec<String>>,
    net_classes: &BTreeMap<String, String>,
    padstack_names: &BTreeMap<PadstackKey, String>,
    via_names: &BTreeMap<RuleKey, String>,
    locked_routing: &LockedRouting,
    zones: &[CopperZoneModel],
    rule_areas: &[RuleAreaModel],
    outline: &str,
) -> Result<String> {
    let components = footprints
        .iter()
        .map(|footprint| ManifestComponent {
            reference: footprint.reference.clone(),
            kiid: footprint.kiid.clone(),
            image_name: footprint.image_name.clone(),
            x_um: footprint.x_um,
            y_um: footprint.y_um,
            rotation_degrees: footprint.rotation_degrees,
            side: footprint.side.name().to_string(),
            pads: footprint
                .pads
                .iter()
                .map(|pad| ManifestPin {
                    pad_number: pad.number.clone(),
                    pad_uuid: Some(pad.uuid.clone()),
                    dsn_pin: format!("{}-{}", footprint.reference, pad.pin),
                    net: pad.net.clone(),
                    padstack_name: padstack_names[&pad.padstack].clone(),
                })
                .collect(),
            non_copper_pads: footprint
                .non_copper_pads
                .iter()
                .map(|pad| ManifestNonCopperPad {
                    pad_number: pad.number.clone(),
                    pad_uuid: pad.uuid.clone(),
                    layers: pad.layers.clone(),
                })
                .collect(),
        })
        .collect();
    let nets = net_pins
        .iter()
        .map(|(name, pins)| ManifestNet {
            name: name.clone(),
            pins: pins.clone(),
            class_name: net_classes[name].clone(),
        })
        .collect();
    let mut padstacks = padstack_names
        .iter()
        .map(|(key, name)| ManifestPadstack {
            name: name.clone(),
            purpose: "pad".to_string(),
            shape: key.shape,
            layers: key.layers.clone(),
            size_x_um: key.size_x_um,
            size_y_um: key.size_y_um,
            corner_radius_um: key.corner_radius_um,
            drill_um: key.drill_um,
        })
        .collect::<Vec<_>>();
    padstacks.extend(via_names.iter().map(|(rule, name)| ManifestPadstack {
        name: name.clone(),
        purpose: "via".to_string(),
        shape: PadShape::Circle,
        layers: copper_layers.to_vec(),
        size_x_um: rule.via_diameter_um,
        size_y_um: rule.via_diameter_um,
        corner_radius_um: None,
        drill_um: Some(rule.via_drill_um),
    }));
    padstacks.sort_by(|left, right| left.name.cmp(&right.name));

    serde_json::to_string_pretty(&Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        board_path: board_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        coordinate_unit: "um".to_string(),
        resolution: DSN_RESOLUTION as u32,
        supported_profile: SupportedProfile {
            copper_layers: u32::try_from(copper_layers.len())
                .context("copper layer count does not fit the manifest")?,
            component_side: component_side(footprints).to_string(),
            pad_shapes: ["circle", "rect", "roundrect", "oval"]
                .map(String::from)
                .to_vec(),
            existing_routing: !locked_routing.tracks.is_empty() || !locked_routing.vias.is_empty(),
            locked_track_count: locked_routing.tracks.len(),
            locked_via_count: locked_routing.vias.len(),
            copper_zones: !zones.is_empty(),
            rule_areas: !rule_areas.is_empty(),
            custom_rules: false,
            outline: outline.to_string(),
        },
        layers: copper_layers
            .iter()
            .enumerate()
            .map(|(index, layer)| ManifestLayer {
                kicad_name: layer.clone(),
                dsn_name: layer.clone(),
                index,
            })
            .collect(),
        components,
        nets,
        padstacks,
        zones: zone_inventory(zones),
        rule_areas: rule_area_inventory(rule_areas),
    })
    .context("serialize routing manifest")
}
