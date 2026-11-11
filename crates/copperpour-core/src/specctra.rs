//! Fail-closed KiCad PCB snapshot to Specctra DSN lowering.
//!
//! The input is the exact string returned by KiCad's IPC
//! `SaveDocumentToString`; this module never reads or edits the live board
//! file. The first supported profile is intentionally narrow and rejects
//! every feature it cannot represent without approximation.

use crate::hex::sha256_hex;
use anyhow::{bail, Context, Result};
use copperpour_ipc::IpcEffectiveRoutingRules;
use copperpour_sexp::{parse_sexp, SexpNode};
use serde::{Deserialize, Serialize};
use specctra::read::{ListTokenizer, ReadDsn};
use specctra::structure as dsn;
use specctra::write::{ListWriter, WriteSes};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Cursor};
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
    /// [`dsn_pin_names`].
    pin: String,
    x_um: i64,
    y_um: i64,
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
    rotation_degrees: f64,
    pads: Vec<PadModel>,
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
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 3;

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
}

/// How many copper zones of one net cover one copper layer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestZoneCount {
    pub layer: String,
    pub net: String,
    pub count: usize,
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

pub(crate) fn export_dsn(
    board_path: &Path,
    board_source: &str,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<ExportBundle> {
    let tree = parse_sexp(board_source).context("parse KiCad IPC board snapshot")?;
    if tree.head() != Some("kicad_pcb") {
        bail!("KiCad IPC snapshot root is not 'kicad_pcb'");
    }

    let copper_layers = copper_layers(&tree)?;
    let power_layers = power_layers(&tree);
    let outline = simple_closed_outline(&tree, outline_arc_error_um(board_path, &tree)?)?;
    let net_table = top_level_net_table(&tree);
    let locked_routing = locked_routing(&tree, &net_table, &copper_layers)?;
    let zones = copper_zones(&tree, &net_table, &copper_layers)?;
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

    let pcb = build_pcb(
        board_path,
        &copper_layers,
        &power_layers,
        &outline,
        &footprints,
        &net_pins,
        &class_nets,
        &net_classes,
        &padstack_names,
        &via_names,
        &locked_routing,
        &zones,
    )?;
    let dsn = serialize_and_validate(pcb)?;
    let source_sha256 = sha256_hex(board_source.as_bytes());
    let manifest = build_manifest(
        board_path,
        &source_sha256,
        &copper_layers,
        &footprints,
        &net_pins,
        &net_classes,
        &padstack_names,
        &via_names,
        &locked_routing,
        &zones,
    )?;

    Ok(ExportBundle {
        dsn,
        manifest,
        source_sha256,
        component_count: footprints.len(),
        pad_count: footprints
            .iter()
            .map(|footprint| footprint.pads.len())
            .sum(),
        net_count: net_pins.len(),
        class_count: class_nets.len(),
        copper_layer_count: copper_layers.len(),
        copper_zone_count: zones.len(),
    })
}

/// Replace Copperpour's deterministic DSN with a KiCad-native DSN while keeping
/// the same fail-closed board profile and rewriting the reverse manifest to
/// the exact identifiers KiCad emitted. This lets the strict SES importer
/// remain authoritative instead of trusting a second, looser return path.
pub(crate) fn adopt_native_dsn(
    mut baseline: ExportBundle,
    native_dsn: String,
) -> Result<ExportBundle> {
    let mut manifest: Manifest =
        serde_json::from_str(&baseline.manifest).context("parse baseline routing manifest")?;
    // KiCad names repeated pads by file order and Copperpour by UUID; until a
    // native fixture proves a one-to-one mapping, adoption refuses (#338).
    if let Some((component, pin)) = manifest
        .components
        .iter()
        .flat_map(|component| component.pads.iter().map(move |pin| (component, pin)))
        .find(|(component, pin)| {
            pin.dsn_pin != format!("{}-{}", component.reference, pin.pad_number)
        })
    {
        bail!(
            "KiCad native DSN adoption does not map repeated pad numbers yet: {} has more than one pad '{}'",
            component.reference,
            pin.pad_number
        );
    }
    validate_dsn_syntax(&native_dsn, "KiCad native DSN")?;
    let baseline_tree = parse_dsn_sexp(&baseline.dsn).context("parse Copperpour baseline DSN")?;
    let native_tree = parse_dsn_sexp(&native_dsn).context("parse KiCad native DSN")?;
    let baseline_identity = dsn_identity(&baseline_tree)?;
    let native_identity = dsn_identity(&native_tree)?;

    let layer_names = correlate_layers(&baseline_identity, &native_identity)?;
    let (class_names, via_names) = correlate_classes(&baseline_identity, &native_identity)?;
    let roundrect_models = manifest
        .padstacks
        .iter()
        .filter(|padstack| padstack.shape == PadShape::RoundRect)
        .map(|padstack| (padstack.name.as_str(), padstack))
        .collect();
    let padstack_names =
        correlate_components(&baseline_identity, &native_identity, &roundrect_models)?;
    validate_native_nets(&baseline_identity, &native_identity)?;
    if baseline_identity.planes != native_identity.planes {
        bail!("native DSN changed the copper-zone planes");
    }

    for layer in &mut manifest.layers {
        layer.dsn_name = layer_names
            .get(&layer.dsn_name)
            .with_context(|| format!("native DSN omitted layer '{}'", layer.dsn_name))?
            .clone();
    }
    for component in &mut manifest.components {
        let native = native_identity
            .components
            .get(&component.reference)
            .with_context(|| format!("native DSN omitted component '{}'", component.reference))?;
        component.image_name = native.image_name.clone();
        for pin in &mut component.pads {
            pin.padstack_name = padstack_names
                .get(&pin.padstack_name)
                .with_context(|| {
                    format!(
                        "native DSN omitted padstack mapping for {}-{}",
                        component.reference, pin.pad_number
                    )
                })?
                .clone();
        }
    }
    for net in &mut manifest.nets {
        net.class_name = class_names
            .get(&net.class_name)
            .with_context(|| format!("native DSN omitted class mapping for '{}'", net.name))?
            .clone();
    }
    for padstack in &mut manifest.padstacks {
        let mapping = if padstack.purpose == "via" {
            &via_names
        } else {
            &padstack_names
        };
        padstack.name = mapping
            .get(&padstack.name)
            .with_context(|| format!("native DSN omitted padstack '{}'", padstack.name))?
            .clone();
    }
    manifest
        .padstacks
        .sort_by(|left, right| left.name.cmp(&right.name));
    baseline.manifest =
        serde_json::to_string_pretty(&manifest).context("serialize native routing manifest")?;
    baseline.dsn = native_dsn;
    Ok(baseline)
}

#[derive(Debug, Clone)]
struct DsnPlacementIdentity {
    image_name: String,
    x: i64,
    y: i64,
    side: String,
    rotation: OrderedF64,
    pins: BTreeMap<String, DsnPinIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DsnPinIdentity {
    padstack: String,
    rotation: OrderedF64,
    x: i64,
    y: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedF64(u64);

impl OrderedF64 {
    fn new(value: f64) -> Result<Self> {
        if !value.is_finite() {
            bail!("DSN contains a non-finite number");
        }
        Ok(Self(value.to_bits()))
    }

    fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

#[derive(Debug, Clone)]
struct DsnClassIdentity {
    nets: BTreeSet<String>,
    via_name: String,
    width: i64,
    clearance: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DsnShapeIdentity {
    Circle {
        layer: String,
        diameter: i64,
    },
    Rect {
        layer: String,
        x1: i64,
        y1: i64,
        x2: i64,
        y2: i64,
    },
    Polygon {
        layer: String,
        points: Vec<(OrderedF64, OrderedF64)>,
    },
    /// An oval pad's spine and width (#384). An odd-micrometre difference
    /// between its sides puts the ends on a half micrometre.
    Path {
        layer: String,
        width: OrderedF64,
        points: Vec<(OrderedF64, OrderedF64)>,
    },
}

#[derive(Debug, Clone)]
struct DsnIdentity {
    layers: BTreeMap<usize, String>,
    /// Each layer's `(type …)`, by index.
    layer_types: BTreeMap<usize, String>,
    components: BTreeMap<String, DsnPlacementIdentity>,
    nets: BTreeMap<String, BTreeSet<String>>,
    classes: BTreeMap<String, DsnClassIdentity>,
    padstacks: BTreeMap<String, Vec<DsnShapeIdentity>>,
    /// Every `plane`, sorted: the two exporters need not list them in the
    /// same order.
    planes: Vec<DsnPlaneIdentity>,
}

/// `(net, layer, closed outline)` of one DSN `plane`.
type DsnPlaneIdentity = (String, String, Vec<(i64, i64)>);

fn validate_dsn_syntax(source: &str, label: &str) -> Result<()> {
    let cursor = Cursor::new(source.as_bytes());
    let mut tokenizer = ListTokenizer::new(BufReader::new(cursor));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .map_err(|error| anyhow::anyhow!("{label} failed Specctra parser validation: {error}"))?;
    Ok(())
}

fn parse_dsn_sexp(source: &str) -> Result<SexpNode> {
    // Specctra represents the quote delimiter as the bare token
    // `(string_quote ")`. KiCad's S-expression parser correctly treats the
    // opening quote as a string delimiter, so give only this metadata token an
    // escaped representation for structural inspection. The original DSN is
    // retained byte-for-byte and independently validated by Topola's parser.
    let compatible = source.replace("(string_quote \")", "(string_quote \"\\\"\")");
    parse_sexp(&compatible).context("parse normalized Specctra S-expression")
}

fn dsn_identity(root: &SexpNode) -> Result<DsnIdentity> {
    if root.head() != Some("pcb") {
        bail!("Specctra export root is not 'pcb'");
    }
    let structure = root.find("structure").context("DSN has no structure")?;
    let mut layers = BTreeMap::new();
    let mut layer_types = BTreeMap::new();
    for layer in structure.find_all("layer") {
        let name = dsn_atom(layer, 1, "layer name")?.to_string();
        let index = layer
            .find("property")
            .and_then(|property| property.find("index"))
            .and_then(|index| index.get(1))
            .and_then(SexpNode::as_str)
            .context("DSN layer has no index")?
            .parse::<usize>()
            .context("DSN layer index is not an integer")?;
        let kind = layer
            .find("type")
            .and_then(|kind| kind.get(1))
            .and_then(SexpNode::as_str)
            .context("DSN layer has no type")?
            .to_string();
        if layers.insert(index, name).is_some() {
            bail!("DSN repeats a layer index");
        }
        layer_types.insert(index, kind);
    }
    let mut planes = Vec::new();
    for plane in structure.find_all("plane") {
        let net = dsn_atom(plane, 1, "plane net")?.to_string();
        // `(plane <net> (polygon <layer> 0 x y ...))`; a `window` (a cutout) or
        // any other shape is not a plane the export writes.
        let children = plane.children().map_or(0, <[SexpNode]>::len);
        let polygon = plane
            .get(2)
            .filter(|polygon| polygon.head() == Some("polygon") && children == 3)
            .with_context(|| format!("DSN plane on net '{net}' is not one polygon"))?;
        let DsnShapeIdentity::Polygon { layer, points } = dsn_polygon(polygon)? else {
            unreachable!("dsn_polygon returns a polygon");
        };
        let points = points
            .iter()
            .map(|(x, y)| {
                let integer = |value: OrderedF64| {
                    let value = value.value();
                    (value.fract() == 0.0 && value.abs() < I64_UPPER_EXCLUSIVE)
                        .then_some(value as i64)
                        .context("DSN plane coordinate is not a supported integer")
                };
                Ok((integer(*x)?, integer(*y)?))
            })
            .collect::<Result<Vec<_>>>()?;
        planes.push((net, layer, points));
    }
    planes.sort();

    let library = root.find("library").context("DSN has no library")?;
    let mut image_pins = BTreeMap::<String, BTreeMap<String, DsnPinIdentity>>::new();
    for image in library.find_all("image") {
        let name = dsn_atom(image, 1, "image name")?.to_string();
        let mut pins = BTreeMap::new();
        for pin in image.find_all("pin") {
            let padstack = dsn_atom(pin, 1, "pin padstack")?.to_string();
            // `(pin <padstack> [(rotate <degrees>)] <number> <x> <y>)`
            let (rotation, at) = match pin.get(2) {
                Some(rotate) if rotate.head() == Some("rotate") => {
                    (dsn_number(rotate, 1, "pin rotation")?, 3)
                }
                _ => (0.0, 2),
            };
            let number = dsn_atom(pin, at, "pin number")?.to_string();
            let identity = DsnPinIdentity {
                padstack,
                rotation: OrderedF64::new(rotation)?,
                x: dsn_integer(pin, at + 1, "pin x")?,
                y: dsn_integer(pin, at + 2, "pin y")?,
            };
            if pins.insert(number, identity).is_some() {
                bail!("DSN image '{name}' repeats a pin number");
            }
        }
        if image_pins.insert(name.clone(), pins).is_some() {
            bail!("DSN repeats image '{name}'");
        }
    }
    let mut padstacks = BTreeMap::new();
    for padstack in library.find_all("padstack") {
        let name = dsn_atom(padstack, 1, "padstack name")?.to_string();
        let shapes = padstack
            .find_all("shape")
            .into_iter()
            .map(|shape| {
                let geometry = shape.get(1).context("DSN shape has no geometry")?;
                match geometry.head() {
                    Some("circle") => Ok(DsnShapeIdentity::Circle {
                        layer: dsn_atom(geometry, 1, "circle layer")?.to_string(),
                        diameter: dsn_integer(geometry, 2, "circle diameter")?,
                    }),
                    Some("rect") => Ok(DsnShapeIdentity::Rect {
                        layer: dsn_atom(geometry, 1, "rect layer")?.to_string(),
                        x1: dsn_integer(geometry, 2, "rect x1")?,
                        y1: dsn_integer(geometry, 3, "rect y1")?,
                        x2: dsn_integer(geometry, 4, "rect x2")?,
                        y2: dsn_integer(geometry, 5, "rect y2")?,
                    }),
                    Some("polygon") => dsn_polygon(geometry),
                    Some("path") => dsn_path(geometry),
                    other => bail!("DSN padstack has unsupported shape {other:?}"),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        if shapes.is_empty() || padstacks.insert(name.clone(), shapes).is_some() {
            bail!("DSN contains an empty or duplicate padstack '{name}'");
        }
    }

    let placement = root.find("placement").context("DSN has no placement")?;
    let mut components = BTreeMap::new();
    for component in placement.find_all("component") {
        let image_name = dsn_atom(component, 1, "component image")?.to_string();
        let pins = image_pins
            .get(&image_name)
            .with_context(|| format!("DSN placement uses unknown image '{image_name}'"))?
            .clone();
        for place in component.find_all("place") {
            let reference = dsn_atom(place, 1, "place reference")?.to_string();
            let x = dsn_integer(place, 2, "place x")?;
            let y = dsn_integer(place, 3, "place y")?;
            let side = dsn_atom(place, 4, "place side")?.to_string();
            let rotation = OrderedF64::new(dsn_number(place, 5, "place rotation")?)?;
            let identity = DsnPlacementIdentity {
                image_name: image_name.clone(),
                x,
                y,
                side,
                rotation,
                pins: pins.clone(),
            };
            if components.insert(reference.clone(), identity).is_some() {
                bail!("DSN repeats component '{reference}'");
            }
        }
    }

    let network = root.find("network").context("DSN has no network")?;
    let mut nets = BTreeMap::new();
    for net in network.find_all("net") {
        let name = dsn_atom(net, 1, "net name")?.to_string();
        let pins = net
            .find("pins")
            .context("DSN net has no pins")?
            .children()
            .context("DSN pins is not a list")?
            .iter()
            .skip(1)
            .map(|pin| {
                pin.as_str()
                    .map(str::to_string)
                    .context("DSN pins contains a list")
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if pins.is_empty() || nets.insert(name.clone(), pins).is_some() {
            bail!("DSN contains an empty or duplicate net '{name}'");
        }
    }
    let mut classes = BTreeMap::new();
    for class in network.find_all("class") {
        let name = dsn_atom(class, 1, "class name")?.to_string();
        let children = class.children().context("DSN class is not a list")?;
        let nets = children
            .iter()
            .skip(2)
            .take_while(|child| child.as_str().is_some())
            .map(|child| child.as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        let via_name = class
            .find("circuit")
            .and_then(|circuit| circuit.find("use_via"))
            .and_then(|via| via.get(1))
            .and_then(SexpNode::as_str)
            .context("DSN class has no use_via")?
            .to_string();
        let rule = class.find("rule").context("DSN class has no rule")?;
        let width = rule.find("width").context("DSN class has no width")?;
        let clearance = rule
            .find("clearance")
            .context("DSN class has no clearance")?;
        let width = dsn_integer(width, 1, "class width")?;
        let clearance = dsn_integer(clearance, 1, "class clearance")?;
        if nets.is_empty()
            || !nets.iter().all(|net| {
                network
                    .find_all("net")
                    .iter()
                    .any(|node| node.get(1).and_then(SexpNode::as_str) == Some(net))
            })
            || classes
                .insert(
                    name.clone(),
                    DsnClassIdentity {
                        nets,
                        via_name,
                        width,
                        clearance,
                    },
                )
                .is_some()
        {
            bail!("DSN contains an invalid or duplicate class '{name}'");
        }
    }
    Ok(DsnIdentity {
        layers,
        layer_types,
        components,
        nets,
        classes,
        padstacks,
        planes,
    })
}

fn correlate_layers(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
) -> Result<BTreeMap<String, String>> {
    if baseline.layers.len() != native.layers.len() {
        bail!("native DSN changed the copper-layer count");
    }
    let mappings = baseline
        .layers
        .iter()
        .map(|(index, baseline_name)| {
            let native_name = native
                .layers
                .get(index)
                .with_context(|| format!("native DSN omitted layer index {index}"))?;
            Ok((baseline_name.clone(), native_name.clone()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    if mappings.iter().any(|(baseline, native)| baseline != native) {
        bail!("native DSN renamed a KiCad copper layer");
    }
    if baseline.layer_types != native.layer_types {
        bail!("native DSN changed a copper layer's signal or power type");
    }
    Ok(mappings)
}

fn correlate_components(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
    roundrect_models: &BTreeMap<&str, &ManifestPadstack>,
) -> Result<BTreeMap<String, String>> {
    if baseline.components.len() != native.components.len() {
        bail!("native DSN changed the component count");
    }
    let mut padstacks = BTreeMap::new();
    for (reference, baseline_component) in &baseline.components {
        let native_component = native
            .components
            .get(reference)
            .with_context(|| format!("native DSN omitted component '{reference}'"))?;
        if baseline_component.x != native_component.x
            || baseline_component.y != native_component.y
            || baseline_component.side != native_component.side
            || baseline_component.rotation != native_component.rotation
            || baseline_component.pins.keys().collect::<Vec<_>>()
                != native_component.pins.keys().collect::<Vec<_>>()
        {
            bail!("native DSN changed placement or pins for component '{reference}'");
        }
        for (pin, baseline_pin) in &baseline_component.pins {
            let native_pin = native_component.pins.get(pin).unwrap();
            if baseline_pin.x != native_pin.x || baseline_pin.y != native_pin.y {
                bail!("native DSN changed pin position for component '{reference}' pin '{pin}'");
            }
            if baseline_pin.rotation != native_pin.rotation {
                bail!("native DSN changed pin rotation for component '{reference}' pin '{pin}'");
            }
            let baseline_shape = baseline
                .padstacks
                .get(&baseline_pin.padstack)
                .with_context(|| {
                    format!("baseline DSN omitted padstack '{}'", baseline_pin.padstack)
                })?;
            let native_shape = native
                .padstacks
                .get(&native_pin.padstack)
                .with_context(|| {
                    format!("native DSN omitted padstack '{}'", native_pin.padstack)
                })?;
            // Each padstack pair is judged once; later pins only need the same pair.
            let already_matched =
                padstacks.get(&baseline_pin.padstack) == Some(&native_pin.padstack);
            if !already_matched
                && !pad_geometry_matches(
                    baseline_shape,
                    native_shape,
                    roundrect_models
                        .get(baseline_pin.padstack.as_str())
                        .copied(),
                )
            {
                bail!("native DSN changed pad geometry for component '{reference}' pin '{pin}'");
            }
            if padstacks
                .insert(baseline_pin.padstack.clone(), native_pin.padstack.clone())
                .is_some_and(|previous| previous != native_pin.padstack)
            {
                bail!("native DSN maps one pad geometry to inconsistent padstacks");
            }
        }
    }
    Ok(padstacks)
}

/// A rounded rectangle is judged against its pad; every other shape must be
/// the one Copperpour wrote.
fn pad_geometry_matches(
    baseline: &[DsnShapeIdentity],
    native: &[DsnShapeIdentity],
    roundrect: Option<&ManifestPadstack>,
) -> bool {
    match roundrect {
        Some(model) => polygons_trace_roundrect(native, model),
        None => baseline == native,
    }
}

fn validate_native_nets(baseline: &DsnIdentity, native: &DsnIdentity) -> Result<()> {
    if baseline.nets != native.nets {
        bail!("native DSN changed net or pin membership");
    }
    Ok(())
}

fn correlate_classes(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    if baseline.classes.len() != native.classes.len() {
        bail!("native DSN changed the routing-class count");
    }
    let mut class_names = BTreeMap::new();
    let mut via_names = BTreeMap::new();
    for (baseline_name, baseline_class) in &baseline.classes {
        let matches = native
            .classes
            .iter()
            .filter(|(_, native_class)| native_class.nets == baseline_class.nets)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!("native DSN has no unique class for '{baseline_name}'");
        }
        let (native_name, native_class) = matches[0];
        if baseline_class.width != native_class.width
            || baseline_class.clearance != native_class.clearance
        {
            bail!("native DSN changed width or clearance for class '{baseline_name}'");
        }
        let baseline_via = baseline
            .padstacks
            .get(&baseline_class.via_name)
            .with_context(|| format!("baseline DSN omitted via '{}'", baseline_class.via_name))?;
        let native_via = native
            .padstacks
            .get(&native_class.via_name)
            .with_context(|| format!("native DSN omitted via '{}'", native_class.via_name))?;
        if baseline_via != native_via {
            bail!("native DSN changed via geometry for class '{baseline_name}'");
        }
        class_names.insert(baseline_name.clone(), native_name.clone());
        if via_names
            .insert(
                baseline_class.via_name.clone(),
                native_class.via_name.clone(),
            )
            .is_some_and(|previous| previous != native_class.via_name)
        {
            bail!("native DSN maps one via rule to inconsistent padstacks");
        }
    }
    Ok((class_names, via_names))
}

fn dsn_polygon(geometry: &SexpNode) -> Result<DsnShapeIdentity> {
    let layer = dsn_atom(geometry, 1, "polygon layer")?.to_string();
    if dsn_number(geometry, 2, "polygon aperture width")? != 0.0 {
        bail!("DSN pad polygon has a non-zero aperture width");
    }
    let count = geometry.children().map_or(0, <[SexpNode]>::len);
    if count <= 3 {
        bail!("DSN pad polygon has no coordinates");
    }
    if !(count - 3).is_multiple_of(2) {
        bail!("DSN pad polygon has an odd number of coordinates");
    }
    let points = (3..count)
        .step_by(2)
        .map(|index| {
            Ok((
                OrderedF64::new(dsn_number(geometry, index, "polygon x")?)?,
                OrderedF64::new(dsn_number(geometry, index + 1, "polygon y")?)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if points.len() < 4 || points.first() != points.last() {
        bail!("DSN pad polygon is not a closed outline");
    }
    Ok(DsnShapeIdentity::Polygon { layer, points })
}

/// An oval pad's `path`: a positive width and the spine's two ends.
fn dsn_path(geometry: &SexpNode) -> Result<DsnShapeIdentity> {
    let layer = dsn_atom(geometry, 1, "path layer")?.to_string();
    let width = dsn_number(geometry, 2, "path width")?;
    if width <= 0.0 {
        bail!("DSN pad path on '{layer}' has no width");
    }
    let count = geometry.children().map_or(0, <[SexpNode]>::len);
    if count != 7 {
        bail!("DSN pad path on '{layer}' is not one segment");
    }
    let points = (3..count)
        .step_by(2)
        .map(|index| {
            Ok((
                OrderedF64::new(dsn_number(geometry, index, "path x")?)?,
                OrderedF64::new(dsn_number(geometry, index + 1, "path y")?)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DsnShapeIdentity::Path {
        layer,
        width: OrderedF64::new(width)?,
        points,
    })
}

/// Arc error KiCad allows by default (`max_error`, 0.005 mm). KiCad keeps its
/// polygon within this of the arc on the inside.
const KICAD_DEFAULT_MAX_ERROR_UM: f64 = 5.0;

/// Whether `shapes` draw the rounded rectangle `model` describes on each of its
/// layers. The two exporters approximate the corners differently, so a polygon
/// is judged against the pad, not against the other exporter's vertices.
fn polygons_trace_roundrect(shapes: &[DsnShapeIdentity], model: &ManifestPadstack) -> bool {
    let Some(radius_um) = model.corner_radius_um else {
        return false;
    };
    shapes.len() == model.layers.len()
        && shapes.iter().zip(&model.layers).all(|(shape, layer)| {
            matches!(shape, DsnShapeIdentity::Polygon { layer: shape_layer, points }
                if shape_layer == layer
                    && polygon_traces_roundrect(points, model.size_x_um, model.size_y_um, radius_um))
        })
}

fn polygon_traces_roundrect(
    points: &[(OrderedF64, OrderedF64)],
    size_x_um: i64,
    size_y_um: i64,
    radius_um: i64,
) -> bool {
    let radius = radius_um as f64;
    let (half_x, half_y) = (size_x_um as f64 / 2.0, size_y_um as f64 / 2.0);
    // KiCad grows the radius by r * (1 - cos(pi / 36)) and puts its vertices on
    // that larger arc, so its outline stands up to that far outside the pad.
    // Its chords sag inside by up to the arc error. The extra 1 um covers
    // `corner_radius_um` and the pad size being rounded to the micrometre.
    let outside = radius * (1.0 - (std::f64::consts::PI / 36.0).cos()) + 1.0;
    let inside = KICAD_DEFAULT_MAX_ERROR_UM + 1.0;
    // Signed distance from the rounded rectangle's outline, positive outside.
    let off_outline = |x: f64, y: f64| {
        let qx = x.abs() - (half_x - radius);
        let qy = y.abs() - (half_y - radius);
        let distance = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
        !(-inside..=outside).contains(&distance)
    };
    let (mut low_x, mut high_x, mut low_y, mut high_y) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for edge in points.windows(2) {
        let (start_x, start_y) = (edge[0].0.value(), edge[0].1.value());
        let (end_x, end_y) = (edge[1].0.value(), edge[1].1.value());
        if off_outline(start_x, start_y)
            || off_outline((start_x + end_x) / 2.0, (start_y + end_y) / 2.0)
        {
            return false;
        }
        (low_x, high_x) = (low_x.min(start_x), high_x.max(start_x));
        (low_y, high_y) = (low_y.min(start_y), high_y.max(start_y));
    }
    // Each side reaches the pad's edge: never more than rounding short of it.
    [
        -half_x - low_x,
        high_x - half_x,
        -half_y - low_y,
        high_y - half_y,
    ]
    .iter()
    .all(|reach| (-1.0..=outside).contains(reach))
}

fn dsn_atom<'a>(node: &'a SexpNode, index: usize, label: &str) -> Result<&'a str> {
    node.get(index)
        .and_then(SexpNode::as_str)
        .with_context(|| format!("DSN is missing {label}"))
}

fn dsn_number(node: &SexpNode, index: usize, label: &str) -> Result<f64> {
    let value = dsn_atom(node, index, label)?
        .parse::<f64>()
        .with_context(|| format!("DSN {label} is not numeric"))?;
    if !value.is_finite() {
        bail!("DSN {label} is not finite");
    }
    Ok(value)
}

fn dsn_integer(node: &SexpNode, index: usize, label: &str) -> Result<i64> {
    let value = dsn_number(node, index, label)?;
    if value.fract() != 0.0 || value < i64::MIN as f64 || value > i64::MAX as f64 {
        bail!("DSN {label} is not a supported integer");
    }
    Ok(value as i64)
}

fn locked_routing(
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

/// The board's copper zones (#430). Each one is exported as its outline, one
/// `plane` per copper layer it covers, as KiCad's own exporter writes it; what
/// that cannot say exactly is refused: a rule area (keep-outs stay with #81),
/// a zone without a net (KiCad would invent a net for it), an outline with a
/// cutout (a `window` in the DSN), a zone off the copper stack, and a zone
/// inside a footprint.
fn copper_zones(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
    copper_layers: &[String],
) -> Result<Vec<CopperZoneModel>> {
    for footprint in tree.find_all("footprint") {
        if !footprint.find_all("zone").is_empty() {
            let reference = property_value(footprint, "Reference").unwrap_or("?");
            bail!("unsupported routing profile: footprint {reference} contains a zone");
        }
    }
    let mut zones = Vec::new();
    for zone in tree.find_all("zone") {
        let name = zone.find_str("name").unwrap_or("");
        let label = if name.is_empty() {
            "copper zone".to_string()
        } else {
            format!("copper zone '{name}'")
        };
        if zone.find("keepout").is_some() || zone.find("placement").is_some() {
            bail!("unsupported routing profile: board contains a rule area, which this exporter does not write as a keep-out");
        }
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
        let covered = if named == ["*.Cu"] {
            copper_layers.to_vec()
        } else {
            if let Some(layer) = named.iter().find(|name| !copper_layers.contains(name)) {
                bail!("unsupported routing profile: {label} is on '{layer}', which is not a copper layer of the board");
            }
            if named.iter().collect::<BTreeSet<_>>().len() != named.len() {
                bail!("{label} repeats a layer");
            }
            copper_layers
                .iter()
                .filter(|layer| named.contains(layer))
                .cloned()
                .collect::<Vec<_>>()
        };
        let net = zone
            .find("net")
            .and_then(|node| resolve_net(node, net_table))
            .with_context(|| {
                format!("unsupported routing profile: {label} has no net, which KiCad exports under an invented net name")
            })?;
        let polygons = zone.find_all("polygon");
        if polygons.len() != 1 {
            bail!(
                "unsupported routing profile: {label} on net '{net}' has {} outlines; an outline with a cutout is not exported",
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
            let x = finite_um(point.get_f64(1), "zone x")?;
            let y = finite_um(point.get_f64(2), "zone y")?
                .checked_neg()
                .context("zone y is outside the supported coordinate range")?;
            outline.push((x, y));
        }
        if outline.iter().collect::<BTreeSet<_>>().len() < 3 {
            bail!("{label} outline has fewer than three distinct points");
        }
        zones.push(CopperZoneModel {
            net,
            layers: covered,
            outline,
        });
    }
    Ok(zones)
}

/// How many copper zones of each net cover each copper layer of the board,
/// sorted by layer then net: the inventory the manifest records.
fn zone_inventory(zones: &[CopperZoneModel]) -> Vec<ManifestZoneCount> {
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

/// The copper-zone inventory of a live board, read the way the export reads it.
pub(crate) fn board_zone_inventory(board_source: &str) -> Result<Vec<ManifestZoneCount>> {
    let tree = parse_sexp(board_source).context("parse KiCad board for its copper zones")?;
    let copper_layers = copper_layers(&tree)?;
    let zones = copper_zones(&tree, &top_level_net_table(&tree), &copper_layers)?;
    Ok(zone_inventory(&zones))
}

/// The board's copper layers from top to bottom: `F.Cu`, `In1.Cu`..`In<N-2>.Cu`
/// and `B.Cu`, the order Specctra layer indices follow. KiCad 10 writes the
/// layer table top to bottom (`F.Cu` 0, `In1.Cu` 4, `In2.Cu` 6, `B.Cu` 2) but
/// writes a pad's layer list by ordinal (`F.Cu`, `B.Cu`, `In2.Cu`), so the
/// order is rebuilt from the names rather than taken from either list; a set
/// with a gap, a repeat or a missing outer layer is refused.
fn copper_layers(tree: &SexpNode) -> Result<Vec<String>> {
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
fn power_layers(tree: &SexpNode) -> BTreeSet<String> {
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

fn outline_arc_error_um(board_path: &Path, tree: &SexpNode) -> Result<f64> {
    if !tree
        .children()
        .unwrap_or(&[])
        .iter()
        .any(|node| node.head() == Some("gr_arc") && node.find_str("layer") == Some("Edge.Cuts"))
    {
        return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
    }
    let project_path = board_path.with_extension("kicad_pro");
    let bytes = match std::fs::read(&project_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read project {}", project_path.display()));
        }
    };
    let project: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse project {}", project_path.display()))?;
    let Some(value) = project.pointer("/board/design_settings/rules/max_error") else {
        return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
    };
    let error_mm = value.as_f64().with_context(|| {
        format!(
            "project {} board.design_settings.rules.max_error is not a number",
            project_path.display()
        )
    })?;
    let error_um = error_mm * UM_PER_MM;
    if !error_um.is_finite() || error_um <= 0.0 {
        bail!(
            "project {} board.design_settings.rules.max_error must be a positive finite number",
            project_path.display()
        );
    }
    Ok(error_um)
}

fn simple_closed_outline(tree: &SexpNode, max_error_um: f64) -> Result<Vec<(i64, i64)>> {
    let mut edges: Vec<Vec<(i64, i64)>> = Vec::new();
    for node in tree.children().unwrap_or(&[]) {
        if node.find_str("layer") != Some("Edge.Cuts") {
            continue;
        }
        match node.head() {
            Some("gr_line") => {
                let start = point_um(node, "start").context("Edge.Cuts line has no start")?;
                let end = point_um(node, "end").context("Edge.Cuts line has no end")?;
                if start == end {
                    bail!("unsupported outline: zero-length Edge.Cuts line");
                }
                edges.push(vec![start, end]);
            }
            Some("gr_arc") => {
                let start = point_um_f64(node, "start").context("Edge.Cuts arc has no start")?;
                let mid = point_um_f64(node, "mid").context("Edge.Cuts arc has no mid")?;
                let end = point_um_f64(node, "end").context("Edge.Cuts arc has no end")?;
                edges.push(outline_arc_points(start, mid, end, max_error_um)?);
            }
            Some(tag) if tag.starts_with("gr_") => {
                bail!("unsupported first routing profile: Edge.Cuts '{tag}' is not a line or circular arc")
            }
            _ => {}
        }
    }
    // Two edges are enough for a D-shaped board (one line, one arc). A single
    // edge fails the degree check below, and the enclosed-area check after the
    // walk refuses two edges that only retrace each other.
    if edges.is_empty() {
        bail!("unsupported outline: no Edge.Cuts lines or arcs");
    }

    // Stable edge order makes the chosen direction deterministic even when
    // KiCad serializes the outline primitives in a different order.
    edges.sort_by(|left, right| {
        let canonical = |points: &[(i64, i64)]| {
            let forward = points.to_vec();
            let reverse = points.iter().rev().copied().collect::<Vec<_>>();
            if forward <= reverse {
                forward
            } else {
                reverse
            }
        };
        canonical(left).cmp(&canonical(right))
    });

    let mut adjacency: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for (index, points) in edges.iter().enumerate() {
        let start = *points.first().context("outline edge has no points")?;
        let end = *points.last().context("outline edge has no points")?;
        adjacency.entry(start).or_default().push(index);
        adjacency.entry(end).or_default().push(index);
    }
    for (point, incident_edges) in &mut adjacency {
        incident_edges.sort_unstable();
        if incident_edges.len() != 2 {
            bail!(
                "unsupported outline: vertex ({}, {}) has degree {}, expected 2",
                point.0,
                point.1,
                incident_edges.len()
            );
        }
    }

    let start = *adjacency.keys().next().context("outline has no vertices")?;
    let mut ordered = vec![start];
    let mut current = start;
    let mut used_edges = BTreeSet::new();
    loop {
        let edge_index = adjacency[&current]
            .iter()
            .copied()
            .find(|index| !used_edges.contains(index))
            .context("unsupported outline: Edge.Cuts is not one closed loop")?;
        used_edges.insert(edge_index);
        let points = &edges[edge_index];
        let directed = if points.first() == Some(&current) {
            points.clone()
        } else if points.last() == Some(&current) {
            points.iter().rev().copied().collect()
        } else {
            bail!("unsupported outline: Edge.Cuts edge is disconnected");
        };
        ordered.extend(directed.into_iter().skip(1));
        current = *ordered.last().context("outline has no points")?;
        if current == start {
            break;
        }
    }
    if used_edges.len() != edges.len() {
        bail!("unsupported outline: Edge.Cuts contains more than one loop");
    }
    // Two straight lines between the same two points, or one arc drawn twice,
    // walk back over themselves: the loop is closed but encloses nothing.
    if edges.len() == 2 {
        let doubled_area: i128 = ordered
            .windows(2)
            .map(|edge| {
                i128::from(edge[0].0) * i128::from(edge[1].1)
                    - i128::from(edge[1].0) * i128::from(edge[0].1)
            })
            .sum();
        if doubled_area == 0 {
            bail!("unsupported outline: two Edge.Cuts edges enclose no area");
        }
    }
    Ok(ordered)
}

/// Flatten a KiCad three-point arc into chords that stay within the board's
/// `max_error`. A 0.707 µm allowance covers rounding the board's input points
/// to the exporter’s micrometre coordinate grid.
fn outline_arc_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
    max_error_um: f64,
) -> Result<Vec<(i64, i64)>> {
    use std::f64::consts::{PI, TAU};

    if start == end || start == mid || mid == end {
        bail!("unsupported outline: Edge.Cuts arc has coincident points");
    }
    let (bx, by) = (mid.0 - start.0, mid.1 - start.1);
    let (cx, cy) = (end.0 - start.0, end.1 - start.1);
    let cross = bx * cy - by * cx;
    let span = bx.hypot(by).max(cx.hypot(cy)).max((cx - bx).hypot(cy - by));
    if !cross.is_finite() || cross.abs() <= f64::EPSILON * span * span * 16.0 {
        bail!("unsupported outline: Edge.Cuts arc points are collinear");
    }

    // Circumcentre relative to start; using local coordinates avoids loss of
    // precision for boards whose origin is far from the outline.
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (cy * b2 - by * c2) / (2.0 * cross);
    let uy = (bx * c2 - cx * b2) / (2.0 * cross);
    let radius = ux.hypot(uy);
    if !radius.is_finite() || radius <= 0.0 {
        bail!("unsupported outline: Edge.Cuts arc has no finite radius");
    }

    let angle = |x: f64, y: f64| (y - uy).atan2(x - ux);
    let start_angle = angle(0.0, 0.0);
    let to_mid = (angle(bx, by) - start_angle).rem_euclid(TAU);
    let to_end = (angle(cx, cy) - start_angle).rem_euclid(TAU);
    let sweep = if to_mid <= to_end {
        to_end
    } else {
        to_end - TAU
    };
    let chord_error_um = max_error_um - std::f64::consts::FRAC_1_SQRT_2;
    if !chord_error_um.is_finite() || chord_error_um <= 0.0 {
        bail!("unsupported outline: board max_error is smaller than the DSN coordinate resolution");
    }
    let max_step = if chord_error_um >= radius {
        PI
    } else {
        2.0 * (1.0 - chord_error_um / radius).acos()
    };
    if !max_step.is_finite() || max_step <= 0.0 {
        bail!("unsupported outline: board max_error cannot represent an Edge.Cuts arc");
    }
    let segment_count = (sweep.abs() / max_step).ceil().max(1.0) as usize;
    if segment_count > MAX_OUTLINE_ARC_SEGMENTS {
        bail!(
            "unsupported outline: Edge.Cuts arc needs more than {MAX_OUTLINE_ARC_SEGMENTS} segments"
        );
    }

    let mut points = Vec::with_capacity(segment_count + 1);
    let start_point = (round_um_i64(start.0)?, round_um_i64(start.1)?);
    let end_point = (round_um_i64(end.0)?, round_um_i64(end.1)?);
    points.push(start_point);
    for index in 1..segment_count {
        let theta = start_angle + sweep * index as f64 / segment_count as f64;
        let point = (
            round_um_i64(start.0 + ux + radius * theta.cos())?,
            round_um_i64(start.1 + uy + radius * theta.sin())?,
        );
        if points.last() != Some(&point) {
            points.push(point);
        }
    }
    if points.last() != Some(&end_point) {
        points.push(end_point);
    }
    if points.len() < 2 {
        bail!("unsupported outline: Edge.Cuts arc collapsed at DSN resolution");
    }
    Ok(points)
}

fn round_um_i64(value_um: f64) -> Result<i64> {
    let rounded = value_um.round();
    // `i64::MAX as f64` rounds up to 2^63, so the upper bound is exclusive.
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= I64_UPPER_EXCLUSIVE {
        bail!("unsupported outline: flattened Edge.Cuts arc exceeds coordinate range");
    }
    Ok(rounded as i64)
}

fn footprints(
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
        if layer != "F.Cu" {
            bail!("unsupported first routing profile: footprint on '{layer}'");
        }
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
        let image_name = format!("copperpour_image_{reference}");

        let mut pads = Vec::new();
        for pad in footprint.find_all("pad") {
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
            let x_um = finite_um(at.get_f64(1), "pad x")?;
            let y_um = -finite_um(at.get_f64(2), "pad y")?;
            let pin_degrees = pin_rotation(
                finite_number(at.get_f64(3).or(Some(0.0)), "pad rotation")?,
                rotation_degrees,
            );
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
                if layers != ["F.Cu".to_string()] {
                    bail!(
                        "unsupported SMD pad {reference}-{number}: copper layers are [{}]",
                        layers.join(", ")
                    );
                }
                (layers, None)
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
        output.push(FootprintModel {
            reference,
            kiid,
            image_name,
            x_um,
            y_um,
            rotation_degrees,
            pads,
        });
    }
    output.sort_by(|left, right| left.reference.cmp(&right.reference));
    Ok(output)
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

/// Every routed footprint's pins, by footprint UUID, named as [`export_dsn`]
/// names them.
pub(crate) fn board_pin_identities(
    board_source: &str,
) -> Result<BTreeMap<String, Vec<PinIdentity>>> {
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
            (footprint.kiid, pins)
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
fn roundrect_corner_radius_um(
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
fn pin_rotation(pad_degrees: f64, footprint_degrees: f64) -> f64 {
    let turn = (pad_degrees - footprint_degrees).rem_euclid(360.0);
    format!("{turn:.5e}")
        .parse::<f64>()
        .expect("a formatted finite number parses")
        % 360.0
}

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

#[allow(clippy::too_many_arguments)]
fn build_pcb(
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
                side: "front".to_string(),
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
                    x: pad.x_um as f64,
                    y: pad.y_um as f64,
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
                        polygon: dsn::Polygon {
                            layer: layer.clone(),
                            width: 0.0,
                            // Closed, as KiCad writes it.
                            coords: zone
                                .outline
                                .iter()
                                .chain(zone.outline.first())
                                .map(|(x, y)| dsn::Point {
                                    x: *x as f64,
                                    y: *y as f64,
                                })
                                .collect(),
                        },
                    })
                })
                .collect(),
            keepouts: dsn::Keepouts(Vec::new()),
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
fn oval_spine(size_x_um: i64, size_y_um: i64) -> [(f64, f64); 2] {
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
fn roundrect_outline(size_x_um: i64, size_y_um: i64, radius_um: i64) -> Vec<(f64, f64)> {
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

fn serialize_and_validate(pcb: dsn::Pcb) -> Result<String> {
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

#[allow(clippy::too_many_arguments)]
fn build_manifest(
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
            side: "front".to_string(),
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
            component_side: "front".to_string(),
            pad_shapes: ["circle", "rect", "roundrect", "oval"]
                .map(String::from)
                .to_vec(),
            existing_routing: !locked_routing.tracks.is_empty() || !locked_routing.vias.is_empty(),
            locked_track_count: locked_routing.tracks.len(),
            locked_via_count: locked_routing.vias.len(),
            copper_zones: !zones.is_empty(),
            custom_rules: false,
            outline: "one closed loop of Edge.Cuts lines and circular arcs, with arcs flattened within board max_error".to_string(),
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
    })
    .context("serialize routing manifest")
}

fn top_level_net_table(tree: &SexpNode) -> BTreeMap<String, String> {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use copperpour_ipc::IpcRoutingRules;

    fn rules() -> IpcEffectiveRoutingRules {
        ["GND", "VCC"]
            .into_iter()
            .map(|net| {
                (
                    net.to_string(),
                    IpcRoutingRules {
                        class_name: "Default".to_string(),
                        constituents: vec!["Default".to_string()],
                        track_width_mm: Some(0.25),
                        clearance_mm: Some(0.2),
                        via_diameter_mm: Some(0.6),
                        via_drill_mm: Some(0.3),
                    },
                )
            })
            .collect()
    }

    fn native_fixture_rules() -> IpcEffectiveRoutingRules {
        let mut rules = rules();
        for rule in rules.values_mut() {
            rule.track_width_mm = Some(0.2);
        }
        rules
    }

    const STOCK_ROUNDRECT: &str =
        include_str!("../tests/fixtures/specctra_stock_roundrect.kicad_pcb");
    const STOCK_ROUNDRECT_NATIVE: &str =
        include_str!("../tests/fixtures/specctra_stock_roundrect.native-kicad-10.dsn");

    /// KiCad's default netclass, which every net of the stock fixture uses.
    fn stock_rules() -> IpcEffectiveRoutingRules {
        let mut rules = native_fixture_rules();
        let mut sig = rules["VCC"].clone();
        sig.class_name = "Default".to_string();
        rules.insert("SIG".to_string(), sig);
        rules
    }

    /// KiCad's export of the stock fixture without its courtyard
    /// `(outline (polygon ...))` lines, which the Specctra parser behind
    /// `validate_dsn_syntax` cannot read (#841). Pads and padstacks are
    /// KiCad's, unchanged.
    fn stock_roundrect_native() -> String {
        let native = STOCK_ROUNDRECT_NATIVE
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert_eq!(
            STOCK_ROUNDRECT_NATIVE.lines().count() - native.lines().count(),
            3,
            "one courtyard polygon per image"
        );
        native
    }

    fn stock_export() -> ExportBundle {
        export_dsn(
            Path::new("board.kicad_pcb"),
            STOCK_ROUNDRECT,
            &stock_rules(),
        )
        .unwrap()
    }

    fn manifest_pads(manifest: &str) -> Vec<ManifestPadstack> {
        let manifest: Manifest = serde_json::from_str(manifest).unwrap();
        manifest
            .padstacks
            .into_iter()
            .filter(|padstack| padstack.purpose == "pad")
            .collect()
    }

    #[test]
    fn stock_roundrect_pads_export_as_polygons() {
        let export = stock_export();

        // Pad sizes and radii KiCad 10.0.6 named in its own export of this
        // board (`RoundRect[T]Pad_<w>x<h>_<r + inflation>_um`), see the README.
        let pads = manifest_pads(&export.manifest)
            .iter()
            .map(|pad| {
                (
                    pad.shape,
                    pad.size_x_um,
                    pad.size_y_um,
                    pad.corner_radius_um,
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            pads,
            BTreeSet::from([
                (PadShape::RoundRect, 540, 640, Some(135)),
                (PadShape::RoundRect, 800, 950, Some(200)),
                (PadShape::RoundRect, 900, 950, Some(225)),
            ])
        );
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(
            manifest["supported_profile"]["pad_shapes"],
            serde_json::json!(["circle", "rect", "roundrect", "oval"])
        );
        assert_eq!(export.pad_count, 6);
        assert!(export.dsn.contains("(polygon F.Cu 0"), "{}", export.dsn);
    }

    #[test]
    fn roundrect_outline_encloses_the_pad_within_its_error() {
        for (size_x, size_y, radius) in [(540, 640, 135), (1025, 1400, 250), (600, 600, 300)] {
            let outline = roundrect_outline(size_x, size_y, radius);
            let model = ManifestPadstack {
                name: String::new(),
                purpose: "pad".to_string(),
                shape: PadShape::RoundRect,
                layers: vec!["F.Cu".to_string()],
                size_x_um: size_x,
                size_y_um: size_y,
                corner_radius_um: Some(radius),
                drill_um: None,
            };
            let (half_x, half_y, r) = (size_x as f64 / 2.0, size_y as f64 / 2.0, radius as f64);
            // Every point of the copper outline is inside the polygon, up to
            // the 0.05 um the DSN's resolution can move a vertex.
            for step in 0..3600 {
                let angle = f64::from(step).to_radians() / 10.0;
                let (cos, sin) = (angle.cos(), angle.sin());
                let copper = (
                    (half_x - r).copysign(cos) + r * cos,
                    (half_y - r).copysign(sin) + r * sin,
                );
                let outside = outline.windows(2).any(|edge| {
                    let (a, b) = (edge[0], edge[1]);
                    let cross = (b.0 - a.0) * (copper.1 - a.1) - (b.1 - a.1) * (copper.0 - a.0);
                    cross < -0.05 * (b.0 - a.0).hypot(b.1 - a.1)
                });
                assert!(!outside, "{copper:?} outside {size_x}x{size_y} r{radius}");
            }
            let shape = DsnShapeIdentity::Polygon {
                layer: "F.Cu".to_string(),
                points: outline
                    .iter()
                    .map(|(x, y)| (OrderedF64::new(*x).unwrap(), OrderedF64::new(*y).unwrap()))
                    .collect(),
            };
            assert!(polygons_trace_roundrect(&[shape], &model));
        }
    }

    #[test]
    fn native_kicad_roundrect_polygons_are_adopted() {
        let baseline = stock_export();
        let native = stock_roundrect_native();
        let adopted = adopt_native_dsn(baseline, native.clone()).unwrap();

        assert_eq!(adopted.dsn, native);
        let names = manifest_pads(&adopted.manifest)
            .into_iter()
            .map(|pad| (pad.name, pad.corner_radius_um))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                (
                    "RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0".to_string(),
                    Some(135)
                ),
                (
                    "RoundRect[T]Pad_800.000000x950.000000_200.761000_um_0.000000_0".to_string(),
                    Some(200)
                ),
                (
                    "RoundRect[T]Pad_900.000000x950.000000_225.856000_um_0.000000_0".to_string(),
                    Some(225)
                ),
            ]
        );
    }

    /// Freerouting 2.3.0 routed both exports of the stock board completely;
    /// the strict import planner accepts each against its own manifest. It
    /// still accepts a `konnect_*` job written before the rename
    /// (Copperpour/Copperpour#250), because SES names are checked against the
    /// manifest, and refuses an SES paired with the other naming.
    #[test]
    fn freerouting_sessions_of_stock_roundrect_pads_pass_the_import_planner() {
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("specctra_stock_roundrect.kicad_pcb");
        std::fs::write(&board_path, STOCK_ROUNDRECT).unwrap();
        let rust = export_dsn(&board_path, STOCK_ROUNDRECT, &stock_rules()).unwrap();
        let native = adopt_native_dsn(rust.clone(), stock_roundrect_native()).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/specctra_stock_roundrect.konnect-manifest.json"
        ))
        .unwrap();
        legacy["board_path"] = board_path.to_string_lossy().into();
        let legacy = legacy.to_string();
        let current_ses =
            include_str!("../tests/fixtures/specctra_stock_roundrect.freerouting-2.3.0.ses");
        let legacy_ses = include_str!(
            "../tests/fixtures/specctra_stock_roundrect.konnect.freerouting-2.3.0.ses"
        );
        for (manifest, ses) in [
            (&rust.manifest, current_ses),
            (&legacy, legacy_ses),
            (
                &native.manifest,
                include_str!(
                    "../tests/fixtures/specctra_stock_roundrect.native-kicad-10.freerouting-2.3.0.ses"
                ),
            ),
        ] {
            let plan =
                crate::specctra_ses::parse_import_plan(&board_path, STOCK_ROUNDRECT, manifest, ses)
                    .unwrap();
            let nets = plan
                .tracks
                .iter()
                .map(|track| track.net_name.as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(nets, BTreeSet::from(["GND", "SIG", "VCC"]));
        }
        for (manifest, ses, expected) in [
            (&rust.manifest, legacy_ses, "copperpour_image_C1"),
            (&legacy, current_ses, "konnect_image_C1"),
        ] {
            let error =
                crate::specctra_ses::parse_import_plan(&board_path, STOCK_ROUNDRECT, manifest, ses)
                    .unwrap_err()
                    .to_string();
            assert!(
                error.contains(&format!("changed from '{expected}'")),
                "{error}"
            );
        }
    }

    /// KiCad 10.0.6 named the stock 0805 padstack (1025 x 1400 um,
    /// `roundrect_rratio 0.243902`) `RoundRect[T]Pad_..._250.951000_um`:
    /// radius 250 um plus 0.951 um of growth. A plain floor of 249.99955
    /// would give 249.
    #[test]
    fn roundrect_corner_radius_follows_kicad_rounding() {
        let pad = |ratio: &str| {
            parse_sexp(&format!(
                "(pad \"1\" smd roundrect (size 1.025 1.4) (roundrect_rratio {ratio}))"
            ))
            .unwrap()
        };
        assert_eq!(
            roundrect_corner_radius_um(&pad("0.243902"), 1025, 1400, "R2-1").unwrap(),
            250
        );
        let error = roundrect_corner_radius_um(&pad("0.6"), 1025, 1400, "R2-1")
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside 0..=0.5"), "{error}");
    }

    /// KiCad's own polygon for the 0402 pad traces that pad, and no pad a few
    /// micrometres larger, smaller, or rounder.
    #[test]
    fn kicad_roundrect_polygon_traces_only_its_own_pad() {
        let native = dsn_identity(&parse_dsn_sexp(&stock_roundrect_native()).unwrap()).unwrap();
        let shapes =
            &native.padstacks["RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0"];
        let model = |size_x_um, size_y_um, corner_radius_um| ManifestPadstack {
            name: String::new(),
            purpose: "pad".to_string(),
            shape: PadShape::RoundRect,
            layers: vec!["F.Cu".to_string()],
            size_x_um,
            size_y_um,
            corner_radius_um: Some(corner_radius_um),
            drill_um: None,
        };
        assert!(polygons_trace_roundrect(shapes, &model(540, 640, 135)));
        assert!(!polygons_trace_roundrect(shapes, &model(550, 650, 137)));
        assert!(!polygons_trace_roundrect(shapes, &model(530, 630, 132)));
        // Within the arc error everywhere, but 1.5 um short of each side.
        assert!(!polygons_trace_roundrect(shapes, &model(544, 644, 135)));
        // KiCad's sharper corners stand about 4 um outside a 145 um corner.
        assert!(!polygons_trace_roundrect(shapes, &model(540, 640, 145)));
    }

    #[test]
    fn native_roundrect_of_another_size_is_refused() {
        let baseline = stock_export();
        // R1's pins point at the 0603 padstack in the native file.
        let native = stock_roundrect_native();
        let moved = native.replace(
            "(pin RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0 1",
            "(pin RoundRect[T]Pad_900.000000x950.000000_225.856000_um_0.000000_0 1",
        );
        assert_ne!(moved, native);
        let error = adopt_native_dsn(baseline, moved).unwrap_err().to_string();
        assert!(
            error.contains("changed pad geometry for component 'R1' pin '1'"),
            "{error}"
        );
    }

    #[test]
    fn chamfered_roundrect_pad_is_refused() {
        let source = STOCK_ROUNDRECT.replacen(
            "(roundrect_rratio 0.25)",
            "(roundrect_rratio 0.25)\n\t\t\t(chamfer_ratio 0.2)\n\t\t\t(chamfer top_left)",
            1,
        );
        assert_ne!(source, STOCK_ROUNDRECT);
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &stock_rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("chamfered corners"), "{error}");
    }

    fn native_baseline(source: &str) -> ExportBundle {
        export_dsn(
            Path::new("board.kicad_pcb"),
            source,
            &native_fixture_rules(),
        )
        .unwrap()
    }

    /// KiCad's stock JST SH `1MP` connector as J1: pad 1 on VCC, pad 2 and
    /// both `MP` pads on GND (#338). See its README.
    const DUPLICATE_MP: &str = include_str!("../tests/fixtures/specctra_duplicate_mp.kicad_pcb");
    /// The two `MP` pads' UUIDs, in the order that names them `MP` and `MP@1`.
    const MP_UUIDS: [&str; 2] = [
        "550c489f-8422-4919-91a6-15d69b43a49a",
        "ce9e8222-11b6-4e53-9716-46236e235ef7",
    ];

    fn export_board(source: &str) -> Result<ExportBundle> {
        export_dsn(Path::new("board.kicad_pcb"), source, &rules())
    }

    /// J1's manifest pins as (pad number, pad UUID, DSN pin, net).
    fn j1_pins(bundle: &ExportBundle) -> Vec<(String, String, String, Option<String>)> {
        let manifest: Manifest = serde_json::from_str(&bundle.manifest).unwrap();
        manifest
            .components
            .into_iter()
            .find(|component| component.reference == "J1")
            .unwrap()
            .pads
            .into_iter()
            .map(|pin| (pin.pad_number, pin.pad_uuid.unwrap(), pin.dsn_pin, pin.net))
            .collect()
    }

    /// `source` with `edit` applied to the pad block holding `uuid`.
    fn edit_pad(source: &str, uuid: &str, edit: impl Fn(&str) -> String) -> String {
        let at = source.find(uuid).unwrap();
        let start = source[..at].rfind("\t\t(pad ").unwrap();
        let end = at + source[at..].find("\n\t\t)").unwrap() + "\n\t\t)".len();
        format!(
            "{}{}{}",
            &source[..start],
            edit(&source[start..end]),
            &source[end..]
        )
    }

    fn pin(
        number: &str,
        uuid: &str,
        dsn: &str,
        net: Option<&str>,
    ) -> (String, String, String, Option<String>) {
        (
            number.into(),
            uuid.into(),
            dsn.into(),
            net.map(String::from),
        )
    }

    /// Two pads share `MP`, both on GND: each is its own DSN pin on GND,
    /// named as KiCad's exporter names them, and the manifest records which
    /// physical pad each name is (#338).
    #[test]
    fn repeated_mp_pads_export_as_two_pins_on_their_net() {
        let bundle = export_board(DUPLICATE_MP).unwrap();
        assert_eq!(bundle.pad_count, 8);
        assert!(
            bundle
                .dsn
                .contains("(pin copperpour_pad_0003 MP -1800 -1875)"),
            "{}",
            bundle.dsn
        );
        assert!(
            bundle
                .dsn
                .contains("(pin copperpour_pad_0003 MP@1 1800 -1875)"),
            "{}",
            bundle.dsn
        );
        assert!(
            bundle.dsn.contains("(pins J1-2 J1-MP J1-MP@1 R1-2 R2-1)"),
            "{}",
            bundle.dsn
        );
        let manifest: serde_json::Value = serde_json::from_str(&bundle.manifest).unwrap();
        assert_eq!(manifest["schema_version"], 3);
        assert_eq!(
            j1_pins(&bundle),
            [
                pin(
                    "1",
                    "95236974-fb55-4796-a22a-73703cb8f5cf",
                    "J1-1",
                    Some("VCC")
                ),
                pin(
                    "2",
                    "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
                    "J1-2",
                    Some("GND")
                ),
                pin("MP", MP_UUIDS[0], "J1-MP", Some("GND")),
                pin("MP", MP_UUIDS[1], "J1-MP@1", Some("GND")),
            ]
        );
        // A number one pad has keeps its name: nothing changes there.
        let two = export_board(include_str!(
            "../tests/fixtures/specctra_two_resistors.kicad_pcb"
        ))
        .unwrap();
        assert!(
            two.dsn.contains("(pins R1-2 R2-1)") || two.dsn.contains("(pins R1-1 R2-2)"),
            "{}",
            two.dsn
        );
        assert!(!two.dsn.contains('@'), "{}", two.dsn);
    }

    /// The names follow the pads' UUIDs, not the order the file lists them.
    #[test]
    fn repeated_pad_names_do_not_depend_on_pad_order() {
        let first = edit_pad(DUPLICATE_MP, MP_UUIDS[0], |block| block.to_string());
        let second_block = {
            let at = DUPLICATE_MP.find(MP_UUIDS[1]).unwrap();
            let start = DUPLICATE_MP[..at].rfind("\t\t(pad ").unwrap();
            let end = at + DUPLICATE_MP[at..].find("\n\t\t)").unwrap() + "\n\t\t)".len();
            DUPLICATE_MP[start..end].to_string()
        };
        // Move the second MP block in front of the first.
        let without = DUPLICATE_MP.replacen(&format!("\n{second_block}"), "", 1);
        let first_start = without
            .find(MP_UUIDS[0])
            .map(|at| without[..at].rfind("\t\t(pad ").unwrap())
            .unwrap();
        let swapped = format!(
            "{}{second_block}\n{}",
            &without[..first_start],
            &without[first_start..]
        );
        assert_ne!(swapped, first);
        assert!(swapped.find(MP_UUIDS[1]) < swapped.find(MP_UUIDS[0]));
        let original = export_board(DUPLICATE_MP).unwrap();
        let reordered = export_board(&swapped).unwrap();
        assert_eq!(reordered.dsn, original.dsn);
        assert_eq!(j1_pins(&reordered), j1_pins(&original));
    }

    /// Pads sharing a number on different nets stay separate pins, each on
    /// its own net; pads sharing a number on no net are exported, on no net.
    #[test]
    fn repeated_signal_and_netless_pads_keep_their_own_nets() {
        let as_one = |block: &str| block.replace("(pad \"MP\"", "(pad \"1\"");
        let signal = edit_pad(
            &edit_pad(DUPLICATE_MP, MP_UUIDS[0], as_one),
            MP_UUIDS[1],
            as_one,
        );
        let bundle = export_board(&signal).unwrap();
        assert_eq!(
            j1_pins(&bundle),
            [
                pin("1", MP_UUIDS[0], "J1-1", Some("GND")),
                pin(
                    "1",
                    "95236974-fb55-4796-a22a-73703cb8f5cf",
                    "J1-1@1",
                    Some("VCC")
                ),
                pin("1", MP_UUIDS[1], "J1-1@2", Some("GND")),
                pin(
                    "2",
                    "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
                    "J1-2",
                    Some("GND")
                ),
            ]
        );
        assert!(
            bundle.dsn.contains("(pins J1-1 J1-1@2 J1-2 R1-2 R2-1)"),
            "{}",
            bundle.dsn
        );
        assert!(
            bundle.dsn.contains("(pins J1-1@1 R1-1 R2-2)"),
            "{}",
            bundle.dsn
        );

        let netless = |block: &str| block.replace("\t\t\t(net \"GND\")\n", "");
        let netless = edit_pad(
            &edit_pad(DUPLICATE_MP, MP_UUIDS[0], netless),
            MP_UUIDS[1],
            netless,
        );
        let bundle = export_board(&netless).unwrap();
        assert_eq!(
            &j1_pins(&bundle)[2..],
            [
                pin("MP", MP_UUIDS[0], "J1-MP", None),
                pin("MP", MP_UUIDS[1], "J1-MP@1", None),
            ]
        );
        assert!(bundle.dsn.contains("MP@1 1800 -1875)"), "{}", bundle.dsn);
        assert!(
            bundle.dsn.contains("(pins J1-2 R1-2 R2-1)"),
            "{}",
            bundle.dsn
        );
    }

    /// A name the suffix would give that is another pad's number, a pad
    /// without a UUID, and a UUID two pads share are refused.
    #[test]
    fn ambiguous_pad_identities_are_refused() {
        let collides = edit_pad(
            DUPLICATE_MP,
            "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
            |block| block.replace("(pad \"2\"", "(pad \"MP@1\""),
        );
        let error = export_board(&collides).unwrap_err().to_string();
        assert!(
            error.contains("its DSN name 'MP@1' is another pad's number"),
            "{error}"
        );

        let no_uuid = edit_pad(DUPLICATE_MP, MP_UUIDS[1], |block| {
            block.replace(&format!("\t\t\t(uuid \"{}\")\n", MP_UUIDS[1]), "")
        });
        let error = export_board(&no_uuid).unwrap_err().to_string();
        assert!(error.contains("pad J1-MP has no UUID"), "{error}");

        let shared = DUPLICATE_MP.replace(MP_UUIDS[1], MP_UUIDS[0]);
        let error = export_board(&shared).unwrap_err().to_string();
        assert!(
            error.contains(&format!("repeats the UUID {}", MP_UUIDS[0])),
            "{error}"
        );
    }

    /// FreeRouting 2.3.0 routed the fixture's export, reading `J1-MP` and
    /// `J1-MP@1` as two pins on GND, and the strict import planner accepts
    /// its session: GND tracks end on both `MP` pads (#338).
    #[test]
    fn freerouting_routes_both_repeated_pads_and_the_planner_accepts_it() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_duplicate_mp.kicad_pcb");
        std::fs::write(&board, DUPLICATE_MP).unwrap();
        let bundle = export_dsn(&board, DUPLICATE_MP, &rules()).unwrap();
        let plan = crate::specctra_ses::parse_import_plan(
            &board,
            DUPLICATE_MP,
            &bundle.manifest,
            include_str!("../tests/fixtures/specctra_duplicate_mp.freerouting-2.3.0.ses"),
        )
        .unwrap();
        let gnd_ends = plan
            .tracks
            .iter()
            .filter(|track| track.net_name == "GND")
            .flat_map(|track| [(track.x1_mm, track.y1_mm), (track.x2_mm, track.y2_mm)])
            .collect::<Vec<_>>();
        // J1 sits at (105, 60); its MP pads at x 105 -/+ 1.8, y 60 + 1.875.
        for pad in [(103.2, 61.875), (106.8, 61.875)] {
            assert!(
                gnd_ends
                    .iter()
                    .any(|end| (end.0 - pad.0).abs() < 1e-6 && (end.1 - pad.1).abs() < 1e-6),
                "no GND track ends on the MP pad at {pad:?}: {gnd_ends:?}"
            );
        }
    }

    /// KiCad's stock through-hole JST PH as J1, turned 90 degrees, whose pad
    /// 2 is oval, and R3 and R4 with oval SMD pads: tall, wide and
    /// equal-sided (#384). See its README.
    const OVAL_PADS: &str = include_str!("../tests/fixtures/specctra_oval_pads.kicad_pcb");
    const OVAL_PADS_NATIVE: &str =
        include_str!("../tests/fixtures/specctra_oval_pads.native-kicad-10.dsn");

    /// An oval pad is the `path` KiCad's own exporter writes: the shorter
    /// side as its width, along the longer axis, on each copper layer.
    #[test]
    fn oval_pads_export_as_kicad_writes_them() {
        let bundle = export_dsn(Path::new("board.kicad_pcb"), OVAL_PADS, &rules()).unwrap();
        for (ours, kicad) in [
            (
                "(path F.Cu 1200 0 -275 0 275)",
                "(path F.Cu 1200  0 -275  0 275)",
            ),
            (
                "(path B.Cu 1200 0 -275 0 275)",
                "(path B.Cu 1200  0 -275  0 275)",
            ),
            (
                "(path F.Cu 600 0 -150 0 150)",
                "(path F.Cu 600  0 -150  0 150)",
            ),
            (
                "(path F.Cu 600 -150 0 150 0)",
                "(path F.Cu 600  -150 0  150 0)",
            ),
            ("(path F.Cu 700 0 0 0 0)", "(path F.Cu 700  0 0  0 0)"),
        ] {
            assert!(
                OVAL_PADS_NATIVE.contains(kicad),
                "KiCad's DSN lacks {kicad}"
            );
            assert!(
                bundle.dsn.contains(ours),
                "{ours} missing from\n{}",
                bundle.dsn
            );
        }
        let manifest: serde_json::Value = serde_json::from_str(&bundle.manifest).unwrap();
        let ovals = manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|padstack| padstack["shape"] == "oval")
            .map(|padstack| {
                (
                    padstack["size_x_um"].as_i64().unwrap(),
                    padstack["size_y_um"].as_i64().unwrap(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ovals,
            BTreeSet::from([(600, 900), (700, 700), (900, 600), (1200, 1750)])
        );
        assert!(manifest["supported_profile"]["pad_shapes"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("oval")));
    }

    #[test]
    fn an_oval_spine_runs_along_the_longer_side() {
        assert_eq!(oval_spine(600, 900), [(0.0, -150.0), (0.0, 150.0)]);
        assert_eq!(oval_spine(900, 600), [(-150.0, 0.0), (150.0, 0.0)]);
        assert_eq!(oval_spine(700, 700), [(0.0, 0.0), (0.0, 0.0)]);
    }

    /// An oval pad with an oval drill (a slot) is still refused: Specctra's
    /// padstacks can't carry the slot.
    #[test]
    fn an_oval_drill_is_still_refused() {
        let at = OVAL_PADS.find("(pad \"2\" thru_hole oval").unwrap();
        let drill = at + OVAL_PADS[at..].find("(drill 0.75)").unwrap();
        let slotted = format!(
            "{}(drill oval 0.75 1.2){}",
            &OVAL_PADS[..drill],
            &OVAL_PADS[drill + "(drill 0.75)".len()..]
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &slotted, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("pad J1-2: oval drill"), "{error}");
    }

    /// KiCad's DSN of the stock JST PH carries its courtyard as an
    /// `(outline (polygon ...))`, which the parser refuses (#98), so adoption
    /// fails closed. Without those lines, the oval padstacks map to KiCad's
    /// own names.
    #[test]
    fn native_oval_padstacks_map_to_kicads_names() {
        let baseline = || {
            export_dsn(
                Path::new("board.kicad_pcb"),
                OVAL_PADS,
                &native_fixture_rules(),
            )
            .unwrap()
        };
        let error = adopt_native_dsn(baseline(), OVAL_PADS_NATIVE.to_string())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("failed Specctra parser validation"),
            "{error}"
        );

        let without_courtyards = OVAL_PADS_NATIVE
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let adopted = adopt_native_dsn(baseline(), without_courtyards).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
        let names = manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|padstack| padstack["shape"] == "oval")
            .map(|padstack| padstack["name"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from([
                "Oval[A]Pad_1200.000000x1750.000000_um".to_string(),
                "Oval[T]Pad_600.000000x900.000000_um".to_string(),
                "Oval[T]Pad_700.000000x700.000000_um".to_string(),
                "Oval[T]Pad_900.000000x600.000000_um".to_string(),
            ])
        );
    }

    /// An oval whose sides differ by an odd number of micrometres has its
    /// spine's ends on a half micrometre, in KiCad's DSN as in Copperpour's,
    /// and the two still match.
    #[test]
    fn a_half_micrometre_oval_spine_matches_kicads() {
        let odd = OVAL_PADS.replacen("(size 0.6 0.9)", "(size 0.6 0.901)", 1);
        assert_ne!(odd, OVAL_PADS);
        let baseline =
            export_dsn(Path::new("board.kicad_pcb"), &odd, &native_fixture_rules()).unwrap();
        assert!(
            baseline.dsn.contains("(path F.Cu 600 0 -150.5 0 150.5)"),
            "{}",
            baseline.dsn
        );
        let native = include_str!("../tests/fixtures/specctra_oval_pads_odd.native-kicad-10.dsn")
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert!(native.contains("(path F.Cu 600  0 -150.5  0 150.5)"));
        let adopted = adopt_native_dsn(baseline, native).unwrap();
        assert!(adopted
            .manifest
            .contains("Oval[T]Pad_600.000000x901.000000_um"));
    }

    /// Freerouting 2.3.0 routed the fixture's export, oval pads included, and
    /// the strict import planner accepts its session: each oval pad has a
    /// track of its net ending on its centre.
    #[test]
    fn freerouting_routes_to_oval_pads_and_the_planner_accepts_it() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_oval_pads.kicad_pcb");
        std::fs::write(&board, OVAL_PADS).unwrap();
        let bundle = export_dsn(&board, OVAL_PADS, &rules()).unwrap();
        let plan = crate::specctra_ses::parse_import_plan(
            &board,
            OVAL_PADS,
            &bundle.manifest,
            include_str!("../tests/fixtures/specctra_oval_pads.freerouting-2.3.0.ses"),
        )
        .unwrap();
        let ends = |net: &str| {
            plan.tracks
                .iter()
                .filter(|track| track.net_name == net)
                .flat_map(|track| [(track.x1_mm, track.y1_mm), (track.x2_mm, track.y2_mm)])
                .collect::<Vec<_>>()
        };
        // J1 at (92, 60) turned 90 degrees puts its pad 2 at (92, 58).
        for (net, pad) in [
            ("GND", (92.0, 58.0)),
            ("VCC", (115.5, 60.0)),
            ("GND", (116.5, 60.0)),
            ("VCC", (123.5, 60.0)),
            ("GND", (124.5, 60.0)),
        ] {
            assert!(
                ends(net)
                    .iter()
                    .any(|end| (end.0 - pad.0).abs() < 1e-6 && (end.1 - pad.1).abs() < 1e-6),
                "no {net} track ends on the oval pad at {pad:?}: {:?}",
                ends(net)
            );
        }
    }

    /// KiCad names repeated pads by file order, and adoption of its DSN
    /// isn't proven for them: it is refused, naming the footprint (#338).
    #[test]
    fn native_adoption_refuses_repeated_pad_numbers() {
        let native = include_str!("../tests/fixtures/specctra_duplicate_mp.native-kicad-10.dsn");
        assert!(native.contains("(pins R1-2 R2-1 J1-2 J1-MP J1-MP@1)"));
        let error = adopt_native_dsn(export_board(DUPLICATE_MP).unwrap(), native.to_string())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not map repeated pad numbers yet: J1 has more than one pad 'MP'"),
            "{error}"
        );
    }

    #[test]
    fn deterministic_export_round_trips_through_specctra_parser() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let first = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
        let second = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();

        assert_eq!(first.dsn, second.dsn);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(first.component_count, 2);
        assert_eq!(first.pad_count, 4);
        assert_eq!(first.net_count, 2);
        assert!(first.dsn.contains("(pcb board.kicad_pcb"));
        assert!(first.dsn.contains("(boundary"));
        assert!(first.dsn.contains("(net GND"));
        assert!(first.dsn.contains("R1-1"));
        let manifest: serde_json::Value = serde_json::from_str(&first.manifest).unwrap();
        assert_eq!(manifest["components"][0]["reference"], "R1");
        assert_eq!(manifest["components"][0]["x_um"], 100_000);
        assert_eq!(manifest["components"][0]["y_um"], -50_000);
        assert_eq!(manifest["components"][0]["rotation_degrees"], 0.0);
        assert_eq!(manifest["components"][0]["side"], "front");
    }

    #[test]
    fn freerouting_owned_dsn_corpus_parses() {
        let source = include_str!("../tests/fixtures/freerouting_issue269_minimal_v2_3_0.dsn");
        let mut tokenizer = ListTokenizer::new(BufReader::new(Cursor::new(source.as_bytes())));
        dsn::DsnFile::read_dsn(&mut tokenizer)
            .expect("Freerouting v2.3.0 corpus fixture must remain parseable");
    }

    #[test]
    fn native_kicad_dsn_rewrites_manifest_identifiers_without_changing_semantics() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let baseline = native_baseline(source);
        let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();

        assert_eq!(adopted.dsn, native);
        assert_eq!(
            manifest["components"][0]["image_name"],
            "Resistor_SMD:R_0402"
        );
        assert_eq!(
            manifest["components"][0]["pads"][0]["padstack_name"],
            "Rect[T]Pad_600.000000x500.000000_um"
        );
        assert_eq!(manifest["nets"][0]["class_name"], "kicad_default");
        assert!(manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|padstack| padstack["name"] == "Via[0-1]_600:300_um"));
    }

    /// Optional local parity check against the Freerouting engine. CI does not
    /// install Java or Freerouting; maintainers can opt in with
    /// `FREEROUTING_JAR=/path/to/freerouting.jar cargo test -p copperpour-core
    /// freerouting_accepts_exported_fixture -- --ignored`.
    #[cfg(feature = "freerouting")]
    #[test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    fn freerouting_accepts_exported_fixture() {
        let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().expect("tempdir");
        let board_path = temp.path().join("board.kicad_pcb");
        let dsn_path = temp.path().join("board.dsn");
        let ses_path = temp.path().join("board.ses");
        std::fs::write(&board_path, source).expect("write board fixture");
        let export = export_dsn(&board_path, source, &rules()).unwrap();
        std::fs::write(&dsn_path, export.dsn).expect("write DSN");

        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let output = std::process::Command::new(crate::java_runtime::java_executable())
            .arg("-jar")
            .arg(jar)
            .arg("-de")
            .arg(&dsn_path)
            .arg("-do")
            .arg(&ses_path)
            .arg("-mp")
            .arg("2")
            .output()
            .expect("launch Freerouting");

        assert!(
            output.status.success(),
            "Freerouting refused generated DSN:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::metadata(&ses_path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false),
            "Freerouting did not produce a non-empty SES"
        );
        let ses = std::fs::read_to_string(&ses_path).expect("read Freerouting SES");
        crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
            .expect("Freerouting SES must pass Copperpour's strict import planner");
    }

    /// Real-engine parity for KiCad 10's native exporter identifiers plus the
    /// rewritten revision manifest consumed by the strict SES planner.
    #[cfg(feature = "freerouting")]
    #[test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    fn freerouting_round_trips_native_kicad_export() {
        let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let temp = tempfile::tempdir().expect("tempdir");
        let board_path = temp.path().join("board.kicad_pcb");
        let dsn_path = temp.path().join("board.dsn");
        let ses_path = temp.path().join("board.ses");
        std::fs::write(&board_path, source).expect("write board fixture");
        let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
        let export = adopt_native_dsn(baseline, native.to_string()).unwrap();
        std::fs::write(&dsn_path, &export.dsn).expect("write native DSN");

        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let output = std::process::Command::new(crate::java_runtime::java_executable())
            .arg("-jar")
            .arg(jar)
            .arg("-de")
            .arg(&dsn_path)
            .arg("-do")
            .arg(&ses_path)
            .arg("-mp")
            .arg("2")
            .output()
            .expect("run Freerouting");
        assert!(
            output.status.success(),
            "Freerouting failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let ses = std::fs::read_to_string(&ses_path).expect("read SES");
        let plan =
            crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
                .expect("strict SES planner accepts native KiCad identifiers");
        assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
    }

    #[test]
    fn incomplete_effective_rules_are_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let mut rules = rules();
        rules.get_mut("GND").unwrap().via_drill_mm = None;
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules)
            .unwrap_err()
            .to_string();
        assert!(error.contains("via drill"), "{error}");
    }

    #[test]
    fn unlocked_routing_is_refused_before_export() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb").replace(
            "\n)",
            "\n  (segment (start 1 1) (end 2 2) (width 0.2) (layer \"F.Cu\") (net 1))\n)",
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("existing track segment"), "{error}");
    }

    #[test]
    fn locked_tracks_and_vias_are_exported_as_fixed_wiring() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let export = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
        assert!(export.dsn.contains("(type fix)"));
        assert!(export.dsn.contains("(path F.Cu 250"));
        // The names a DSN, its manifest and Freerouting's SES share (Copperpour/Copperpour#250).
        for name in [
            "(via copperpour_via_",
            "(image copperpour_image_",
            "(pin copperpour_pad_",
            "(class copperpour_class_",
        ] {
            assert!(export.dsn.contains(name), "{name}");
        }
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(manifest["supported_profile"]["existing_routing"], true);
        assert_eq!(manifest["supported_profile"]["locked_track_count"], 1);
        assert_eq!(manifest["supported_profile"]["locked_via_count"], 1);
    }

    #[test]
    fn freerouting_session_preserves_locked_routing_outside_the_import_plan() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let native =
            include_str!("../tests/fixtures/specctra_two_resistors_locked.native-kicad-10.dsn");
        let ses =
            include_str!("../tests/fixtures/specctra_two_resistors_locked.freerouting-2.3.0.ses");
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("specctra_two_resistors_locked.kicad_pcb");
        std::fs::write(&board_path, source).unwrap();
        let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
        let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
        let plan =
            crate::specctra_ses::parse_import_plan(&board_path, source, &adopted.manifest, ses)
                .unwrap();
        assert_eq!(plan.locked_track_count, 1);
        assert_eq!(plan.locked_via_count, 1);
        assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
    }

    #[test]
    fn locked_arcs_are_refused_instead_of_approximated() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked_arc.kicad_pcb");
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked routed arc"), "{error}");
        assert!(error.contains("without approximation"), "{error}");
    }

    #[test]
    fn branched_outline_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb")
            .replace(
                "\n)",
                "\n  (gr_line (start 80 30) (end 90 40) (stroke (width 0.05) (type default)) (layer \"Edge.Cuts\") (uuid \"branch\"))\n)",
            );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("degree"), "{error}");
    }

    const ROUNDED_OUTLINE: &str =
        include_str!("../tests/fixtures/specctra_rounded_outline.kicad_pcb");

    fn point_segment_distance(point: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
        let delta = (b.0 - a.0, b.1 - a.1);
        let t = (((point.0 - a.0) * delta.0 + (point.1 - a.1) * delta.1)
            / (delta.0 * delta.0 + delta.1 * delta.1))
            .clamp(0.0, 1.0);
        (point.0 - a.0 - t * delta.0).hypot(point.1 - a.1 - t * delta.1)
    }

    fn boundary_points(dsn: &str) -> Vec<(f64, f64)> {
        let tree = parse_dsn_sexp(dsn).unwrap();
        let path = tree
            .find("structure")
            .unwrap()
            .find("boundary")
            .unwrap()
            .find("path")
            .unwrap();
        path.children().unwrap()[3..]
            .chunks_exact(2)
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().parse().unwrap(),
                    pair[1].as_str().unwrap().parse().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn rounded_outline_arc_is_flattened_and_closed() {
        let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
        let outline = simple_closed_outline(&tree, 5.0).unwrap();
        assert_eq!(outline.first(), outline.last());
        // Three convex corners (12 chords each), one concave notch (19), four lines.
        assert_eq!(outline.len(), 12 * 3 + 19 + 4 + 1);
    }

    #[test]
    fn exporter_accepts_rounded_outline_arcs_and_reports_tolerance() {
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("rounded.kicad_pcb");
        std::fs::write(
            board_path.with_extension("kicad_pro"),
            include_str!("../tests/fixtures/specctra_rounded_outline.kicad_pro"),
        )
        .unwrap();
        let export = export_dsn(&board_path, ROUNDED_OUTLINE, &rules()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(
            manifest["supported_profile"]["outline"],
            "one closed loop of Edge.Cuts lines and circular arcs, with arcs flattened within board max_error"
        );
        let native = include_str!("../tests/fixtures/specctra_rounded_outline.native-kicad-10.dsn");
        // Independent circles and counts from KiCad's native export, not the flattener.
        for points in [boundary_points(&export.dsn), boundary_points(native)] {
            assert_eq!(points.first(), points.last());
            for (center, radius, expected_segments) in [
                ((128_000.0, -32_000.0), 2_000.0, 12),
                ((130_000.0, -70_000.0), 5_000.0, 19),
                ((82_000.0, -68_000.0), 2_000.0, 12),
                ((82_000.0, -32_000.0), 2_000.0, 12),
            ] {
                let radial_error =
                    |p: (f64, f64)| ((p.0 - center.0).hypot(p.1 - center.1) - radius).abs();
                let arc_edges = points
                    .windows(2)
                    .filter(|edge| radial_error(edge[0]) < 5.0 && radial_error(edge[1]) < 5.0)
                    .collect::<Vec<_>>();
                assert_eq!(arc_edges.len(), expected_segments, "circle {center:?}");
                for edge in arc_edges {
                    assert!(radial_error(edge[0]) <= 5.0);
                    assert!(radial_error(edge[1]) <= 5.0);
                    let midpoint = ((edge[0].0 + edge[1].0) / 2.0, (edge[0].1 + edge[1].1) / 2.0);
                    assert!(radial_error(midpoint) <= 5.0, "chord {edge:?}");
                }
            }
        }
    }

    #[test]
    fn outline_arc_flattening_respects_tolerance_in_both_directions() {
        let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
        for arc in tree.find_all("gr_arc") {
            let start = point_um_f64(arc, "start").unwrap();
            let mid = point_um_f64(arc, "mid").unwrap();
            let end = point_um_f64(arc, "end").unwrap();
            for (start, end) in [(start, end), (end, start)] {
                let points = outline_arc_points(start, mid, end, 5.0).unwrap();
                assert_eq!(
                    points.first(),
                    Some(&(start.0.round() as i64, start.1.round() as i64))
                );
                assert_eq!(
                    points.last(),
                    Some(&(end.0.round() as i64, end.1.round() as i64))
                );
                let distance = points
                    .windows(2)
                    .map(|edge| {
                        point_segment_distance(
                            mid,
                            (edge[0].0 as f64, edge[0].1 as f64),
                            (edge[1].0 as f64, edge[1].1 as f64),
                        )
                    })
                    .fold(f64::INFINITY, f64::min);
                assert!(distance <= 5.0, "arc misses its midpoint by {distance} um");
            }
        }
    }

    const D_OUTLINE: &str = include_str!("../tests/fixtures/specctra_d_outline.kicad_pcb");
    const D_OUTLINE_ARC: &str = "(gr_arc\n\t\t(start 90 25)\n\t\t(mid 115 50)\n\t\t(end 90 75)";

    #[test]
    fn exporter_accepts_a_two_edge_d_shaped_outline() {
        let export = export_dsn(Path::new("board.kicad_pcb"), D_OUTLINE, &rules()).unwrap();
        let native = include_str!("../tests/fixtures/specctra_d_outline.native-kicad-10.dsn");
        // KiCad's native export splits the 25 mm half circle into 80 chords.
        // The Rust flattener keeps each chord within max_error minus the DSN
        // grid allowance, which needs 85 at this radius (the small corners of
        // the rounded fixture happen to give equal counts).
        let (center, radius) = ((90_000.0, -50_000.0), 25_000.0);
        let radial_error = |p: (f64, f64)| ((p.0 - center.0).hypot(p.1 - center.1) - radius).abs();
        for (points, expected_chords) in [
            (boundary_points(&export.dsn), 85),
            (boundary_points(native), 80),
        ] {
            assert_eq!(points.first(), points.last());
            let (arc_edges, straight_edges): (Vec<_>, Vec<_>) =
                points.windows(2).partition(|edge| {
                    let midpoint = ((edge[0].0 + edge[1].0) / 2.0, (edge[0].1 + edge[1].1) / 2.0);
                    radial_error(midpoint) <= 5.0
                });
            assert_eq!(arc_edges.len(), expected_chords);
            for edge in arc_edges {
                assert!(radial_error(edge[0]) <= 5.0, "chord {edge:?}");
                assert!(radial_error(edge[1]) <= 5.0, "chord {edge:?}");
                // The half circle bulges through (115, 50), not to x < 90 mm.
                assert!(edge.iter().all(|p| p.0 >= 89_999.5), "chord {edge:?}");
            }
            // The straight edge is the line x = 90 mm from y = 25 mm to 75 mm.
            assert_eq!(straight_edges.len(), 1);
            let line = straight_edges[0];
            assert!(line.iter().all(|p| (p.0 - 90_000.0).abs() < 0.5));
            assert!(((line[0].1 - line[1].1).abs() - 50_000.0).abs() < 0.5);
        }
    }

    #[test]
    fn two_edge_outlines_without_area_are_refused() {
        let two_lines = D_OUTLINE.replace(
            D_OUTLINE_ARC,
            "(gr_line\n\t\t(start 90 75)\n\t\t(end 90 25)",
        );
        let same_arc_twice = D_OUTLINE.replace(
            "(gr_line\n\t\t(start 90 25)\n\t\t(end 90 75)",
            D_OUTLINE_ARC,
        );
        for source in [two_lines, same_arc_twice] {
            assert_ne!(source, D_OUTLINE);
            let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
                .unwrap_err()
                .to_string();
            assert!(error.contains("enclose no area"), "{error}");
        }
    }

    #[test]
    fn board_without_edge_cuts_is_refused() {
        let source = D_OUTLINE.replace("(layer \"Edge.Cuts\")", "(layer \"F.SilkS\")");
        assert_ne!(source, D_OUTLINE);
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("no Edge.Cuts lines or arcs"), "{error}");
    }

    #[test]
    fn multiple_outline_loops_are_refused() {
        let source = include_str!("../tests/fixtures/specctra_multiple_outline_loops.kicad_pcb");
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than one loop"), "{error}");
    }

    #[test]
    fn outline_arc_error_uses_project_max_error_when_available() {
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("board.kicad_pcb");
        let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
        let mut project: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/specctra_rounded_outline.kicad_pro"
        ))
        .unwrap();
        project["board"]["design_settings"]["rules"]["max_error"] = serde_json::json!(0.01);
        std::fs::write(
            board_path.with_extension("kicad_pro"),
            serde_json::to_vec(&project).unwrap(),
        )
        .unwrap();
        assert_eq!(outline_arc_error_um(&board_path, &tree).unwrap(), 10.0);
        let outline =
            simple_closed_outline(&tree, outline_arc_error_um(&board_path, &tree).unwrap())
                .unwrap();
        assert_eq!(outline.len(), 9 * 3 + 13 + 4 + 1);
    }

    #[test]
    fn malformed_outline_project_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("board.kicad_pcb");
        for (project, expected) in [
            ("{", "parse project"),
            (
                r#"{"board":{"design_settings":{"rules":{"max_error":"bad"}}}}"#,
                "max_error is not a number",
            ),
        ] {
            std::fs::write(board_path.with_extension("kicad_pro"), project).unwrap();
            let error = export_dsn(&board_path, ROUNDED_OUTLINE, &rules())
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn nonzero_pad_shape_offset_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb").replacen(
            "(size 0.6 0.5)",
            "(size 0.6 0.5)\n\t\t\t(offset 0.1 0)",
            1,
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("shape offset"), "{error}");
    }

    const ROTATED: &str = include_str!("../tests/fixtures/specctra_rotated_footprints.kicad_pcb");
    const ROTATED_NATIVE: &str =
        include_str!("../tests/fixtures/specctra_rotated_footprints.native-kicad-10.dsn");

    /// Each placed pin's `(rotate ...)`, keyed by reference and pin number,
    /// with an absent rotate read as 0. Read straight off the S-expression so
    /// KiCad's sub-micrometre pin positions (#841) don't stop the comparison.
    pub(crate) fn pin_rotations(dsn: &str) -> BTreeMap<(String, String), String> {
        let root = parse_dsn_sexp(dsn).unwrap();
        let library = root.find("library").unwrap();
        let mut rotations = BTreeMap::new();
        for component in root.find("placement").unwrap().find_all("component") {
            let image_name = component.get(1).and_then(SexpNode::as_str).unwrap();
            let image = library
                .find_all("image")
                .into_iter()
                .find(|image| image.get(1).and_then(SexpNode::as_str) == Some(image_name))
                .unwrap();
            for place in component.find_all("place") {
                let reference = place.get(1).and_then(SexpNode::as_str).unwrap();
                for pin in image.find_all("pin") {
                    let number = pin.children().unwrap()[2..]
                        .iter()
                        .find_map(SexpNode::as_str)
                        .unwrap();
                    let rotate = pin
                        .find("rotate")
                        .and_then(|rotate| rotate.get(1))
                        .and_then(SexpNode::as_str)
                        .unwrap_or("0");
                    rotations.insert(
                        (reference.to_string(), number.to_string()),
                        rotate.to_string(),
                    );
                }
            }
        }
        rotations
    }

    /// A pad's file angle includes its footprint's, and `(place ...)` turns
    /// the image again, so a pin's `(rotate ...)` must be the pad's angle
    /// within the footprint: what KiCad's own exporter writes (#840).
    #[test]
    fn rotated_footprint_pins_carry_the_rotation_kicad_writes() {
        let source = include_str!("../tests/fixtures/specctra_rotated_footprints_45.kicad_pcb");
        let native =
            include_str!("../tests/fixtures/specctra_rotated_footprints_45.native-kicad-10.dsn");
        let baseline = native_baseline(source);

        let expected = [
            ("R1", "1", "12.3457"),
            ("R1", "2", "12.3457"),
            ("R2", "1", "0"),
            ("R2", "2", "0"),
            ("R3", "1", "0"),
            ("R3", "2", "0"),
            ("R4", "1", "90"),
            ("R4", "2", "270"),
        ]
        .into_iter()
        .map(|(reference, pin, rotate)| {
            ((reference.to_string(), pin.to_string()), rotate.to_string())
        })
        .collect::<BTreeMap<_, _>>();
        assert_eq!(pin_rotations(native), expected);
        assert_eq!(pin_rotations(&baseline.dsn), expected);
    }

    /// `12.3 - 12.2` is `0.10000000000000142` in f64.
    #[test]
    fn pin_rotation_drops_float_noise() {
        assert_eq!(pin_rotation(12.3, 12.2), 0.1);
    }

    #[test]
    fn native_dsn_of_rotated_footprints_is_adopted() {
        let adopted =
            adopt_native_dsn(native_baseline(ROTATED), ROTATED_NATIVE.to_string()).unwrap();
        assert_eq!(adopted.dsn, ROTATED_NATIVE);
    }

    #[test]
    fn native_dsn_that_turns_a_pin_is_refused() {
        let native = ROTATED_NATIVE.replacen("(rotate 270) ", "", 1);
        let error = adopt_native_dsn(native_baseline(ROTATED), native)
            .unwrap_err()
            .to_string();
        assert!(error.contains("pin rotation"), "{error}");
    }

    /// KiCad's stock-header boards, whose J1 pad 1 has a 2 mm circle on
    /// `B.Cu` under its 1.7 mm square front (#842).
    #[test]
    fn per_layer_padstacks_are_refused() {
        for (mode, source) in [
            (
                "front_inner_back",
                include_str!("../tests/fixtures/specctra_padstack_front_inner_back.kicad_pcb"),
            ),
            (
                "custom",
                include_str!("../tests/fixtures/specctra_padstack_custom.kicad_pcb"),
            ),
        ] {
            let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("pad J1-1 has a '{mode}' padstack")),
                "{error}"
            );
        }
    }

    /// KiCad always writes a mode; a block without one is refused all the same.
    #[test]
    fn padstack_without_a_mode_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_padstack_custom.kicad_pcb").replacen(
            "(mode custom)",
            "",
            1,
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("pad J1-1 has a padstack with no mode"),
            "{error}"
        );
    }

    /// The same board with no padstack block, as KiCad saves a normal pad.
    #[test]
    fn stock_header_with_normal_pads_exports() {
        let source = include_str!("../tests/fixtures/specctra_padstack_normal.kicad_pcb");
        let export = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
        assert_eq!(export.pad_count, 4);
        assert!(
            export.dsn.contains("(rect F.Cu -850 -850 850 850)")
                && export.dsn.contains("(rect B.Cu -850 -850 850 850)"),
            "{}",
            export.dsn
        );
    }

    /// KiCad's locked-via board, its via given a 1 mm `B.Cu` size under a
    /// 0.6 mm front (#842).
    #[test]
    fn locked_via_with_a_per_layer_padstack_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_padstack_locked_via.kicad_pcb");
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("locked via at (105, 50) mm has a 'front_inner_back' padstack"),
            "{error}"
        );
    }

    /// KiCad 10's four-layer copy of the locked two-resistor board, with a
    /// stock through-hole header and a locked track on `In1.Cu`. See its README.
    const FOUR_LAYERS: &str = include_str!("../tests/fixtures/specctra_four_layers.kicad_pcb");
    const FOUR_LAYERS_NATIVE_FILE: &str =
        include_str!("../tests/fixtures/specctra_four_layers.native-kicad-10.dsn");

    /// The native DSN without the header's courtyard polygon, which the DSN
    /// parser does not read yet, as the odd oval-pad test does too.
    fn four_layers_native() -> String {
        FOUR_LAYERS_NATIVE_FILE
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    fn copper_of(table: &str) -> Result<Vec<String>> {
        copper_layers(&parse_sexp(&format!("(kicad_pcb (layers {table}))")).unwrap())
    }

    #[test]
    fn copper_layers_run_top_to_bottom_whatever_the_file_order() {
        assert_eq!(
            copper_of(r#"(0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user)"#).unwrap(),
            ["F.Cu", "B.Cu"]
        );
        // KiCad 10 writes the table top to bottom, as in the four-layer fixture.
        assert_eq!(
            copper_of(
                r#"(0 "F.Cu" signal) (4 "In1.Cu" power) (6 "In2.Cu" mixed) (2 "B.Cu" signal)"#
            )
            .unwrap(),
            ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
        );
        // An order KiCad does not write in the table gives the same stack.
        assert_eq!(
            copper_of(
                r#"(0 "F.Cu" signal) (2 "B.Cu" signal) (4 "In1.Cu" power) (6 "In2.Cu" mixed)"#
            )
            .unwrap(),
            ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
        );
        for table in [
            r#"(0 "F.Cu" signal) (6 "In2.Cu" signal) (2 "B.Cu" signal)"#,
            r#"(0 "F.Cu" signal) (4 "In1.Cu" signal) (2 "B.Cu" signal)"#,
            r#"(0 "F.Cu" signal) (4 "In1.Cu" signal) (6 "In2.Cu" signal)"#,
            r#"(0 "F.Cu" signal) (0 "F.Cu" signal) (2 "B.Cu" signal)"#,
            r#"(0 "F.Cu" signal)"#,
            r#"(9 "F.Adhes" user)"#,
        ] {
            let error = copper_of(table).unwrap_err().to_string();
            assert!(
                error.contains("copper layers must be F.Cu, In1.Cu..In<N-2>.Cu and B.Cu"),
                "{table}: {error}"
            );
        }
    }

    #[test]
    fn four_layer_board_exports_every_copper_layer() {
        let export = export_dsn(
            Path::new("board.kicad_pcb"),
            FOUR_LAYERS,
            &native_fixture_rules(),
        )
        .unwrap();
        assert_eq!(export.copper_layer_count, 4);
        let tree = parse_dsn_sexp(&export.dsn).unwrap();
        let identity = dsn_identity(&tree).unwrap();
        assert_eq!(
            identity.layers.values().collect::<Vec<_>>(),
            ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
        );
        // The header's through-hole pads and the via cover all four layers.
        for name in ["In1.Cu", "In2.Cu"] {
            assert!(
                export
                    .dsn
                    .contains(&format!("(rect {name} -850 -850 850 850)")),
                "{}",
                export.dsn
            );
            assert!(
                export.dsn.contains(&format!("(circle {name} 600)")),
                "{}",
                export.dsn
            );
        }
        assert!(
            export
                .dsn
                .contains("(path In1.Cu 250 105000 -50000 105000 -55000)"),
            "{}",
            export.dsn
        );
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(manifest["supported_profile"]["copper_layers"], 4);
        assert_eq!(manifest["supported_profile"]["locked_track_count"], 2);
        assert_eq!(manifest["supported_profile"]["locked_via_count"], 1);
        assert_eq!(
            manifest["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|layer| (
                    layer["index"].as_u64().unwrap(),
                    layer["kicad_name"].as_str().unwrap()
                ))
                .collect::<Vec<_>>(),
            [(0, "F.Cu"), (1, "In1.Cu"), (2, "In2.Cu"), (3, "B.Cu")]
        );
        let via = manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|padstack| padstack["purpose"] == "via")
            .unwrap();
        assert_eq!(
            via["layers"],
            serde_json::json!(["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"])
        );
    }

    /// KiCad's own export of the same board passes the semantic comparison
    /// layer by layer, pad by pad: both paths describe one routing job.
    #[test]
    fn four_layer_native_dsn_matches_the_rust_export() {
        let baseline = native_baseline(FOUR_LAYERS);
        let adopted = adopt_native_dsn(baseline, four_layers_native()).unwrap();
        assert_eq!(adopted.dsn, four_layers_native());
        assert_ne!(adopted.dsn, FOUR_LAYERS_NATIVE_FILE);
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
        assert!(manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|padstack| padstack["name"] == "Via[0-3]_600:300_um"));
    }

    #[test]
    fn four_layer_through_hole_pad_must_cover_every_copper_layer() {
        let two_layer_pad = FOUR_LAYERS.replacen(
            r#"(layers "*.Cu" "*.Mask")"#,
            r#"(layers "F.Cu" "B.Cu" "*.Mask")"#,
            1,
        );
        assert_ne!(two_layer_pad, FOUR_LAYERS);
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &two_layer_pad,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("unsupported through-hole pad J1-1: copper layers are [F.Cu, B.Cu]"),
            "{error}"
        );
        // KiCad saves a pad on every copper layer as `*.Cu`; a list in full
        // is accepted in the ordinal order of pad lists or top to bottom.
        for list in [
            r#""F.Cu" "B.Cu" "In1.Cu" "In2.Cu""#,
            r#""F.Cu" "In1.Cu" "In2.Cu" "B.Cu""#,
        ] {
            let listed = FOUR_LAYERS.replacen(
                r#"(layers "*.Cu" "*.Mask")"#,
                &format!(r#"(layers {list} "*.Mask")"#),
                1,
            );
            assert_ne!(listed, FOUR_LAYERS);
            export_dsn(
                Path::new("board.kicad_pcb"),
                &listed,
                &native_fixture_rules(),
            )
            .unwrap_or_else(|error| panic!("{list}: {error}"));
        }
        // Every layer named, but one of them twice, or one missing.
        for list in [
            r#""F.Cu" "B.Cu" "In1.Cu" "In2.Cu" "In2.Cu""#,
            r#""F.Cu" "B.Cu" "In1.Cu" "In1.Cu""#,
        ] {
            let repeated = FOUR_LAYERS.replacen(
                r#"(layers "*.Cu" "*.Mask")"#,
                &format!(r#"(layers {list} "*.Mask")"#),
                1,
            );
            let error = export_dsn(
                Path::new("board.kicad_pcb"),
                &repeated,
                &native_fixture_rules(),
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("unsupported through-hole pad J1-1: copper layers are"),
                "{list}: {error}"
            );
        }
    }

    #[test]
    fn four_layer_locked_via_must_be_a_through_via() {
        let blind_span = FOUR_LAYERS.replacen(
            "(drill 0.3)\n\t\t(layers \"F.Cu\" \"B.Cu\")",
            "(drill 0.3)\n\t\t(layers \"F.Cu\" \"In1.Cu\")",
            1,
        );
        assert_ne!(blind_span, FOUR_LAYERS);
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &blind_span,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(
                "unsupported locked via layer span [F.Cu, In1.Cu]; expected [F.Cu, B.Cu]"
            ),
            "{error}"
        );
    }

    /// Freerouting 2.3.0 routed KiCad's native four-layer export on F.Cu,
    /// In1.Cu and In2.Cu with two vias; the strict planner accepts the session, inner
    /// layers included.
    #[test]
    fn freerouting_routes_four_layers_and_the_planner_accepts_it() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_four_layers.kicad_pcb");
        std::fs::write(&board, FOUR_LAYERS).unwrap();
        let baseline = export_dsn(&board, FOUR_LAYERS, &native_fixture_rules()).unwrap();
        let adopted = adopt_native_dsn(baseline, four_layers_native()).unwrap();
        let ses = include_str!(
            "../tests/fixtures/specctra_four_layers.native-kicad-10.freerouting-2.3.0.ses"
        );
        let plan =
            crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &adopted.manifest, ses)
                .unwrap();
        let layers = plan
            .tracks
            .iter()
            .map(|track| track.layer.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(layers, BTreeSet::from(["F.Cu", "In1.Cu", "In2.Cu"]));
        assert_eq!(plan.vias.len(), 2);

        // A session that names a layer the manifest does not map is refused.
        let stray = ses.replacen("(path In2.Cu", "(path In3.Cu", 1);
        assert_ne!(stray, ses);
        let error =
            crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &adopted.manifest, &stray)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("SES path uses unknown layer 'In3.Cu'"),
            "{error}"
        );
    }

    /// The manifest's layer mappings are the count it declares, top to bottom.
    #[test]
    fn manifest_layer_mappings_must_match_its_copper_layer_count() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_four_layers.kicad_pcb");
        std::fs::write(&board, FOUR_LAYERS).unwrap();
        let export = export_dsn(&board, FOUR_LAYERS, &native_fixture_rules()).unwrap();
        let ses = include_str!(
            "../tests/fixtures/specctra_four_layers.native-kicad-10.freerouting-2.3.0.ses"
        );
        let adopted = adopt_native_dsn(export, four_layers_native()).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
        let mapping = "manifest layer mappings do not match its";
        let profile = "manifest does not describe the supported first routing profile";
        type ManifestEdit = fn(&mut serde_json::Value);
        let cases: [(&str, &str, ManifestEdit); 4] = [
            ("count", mapping, |m| {
                m["supported_profile"]["copper_layers"] = 2.into()
            }),
            // Three layers mapped as F.Cu, In1.Cu, B.Cu: only the count is wrong.
            ("odd", profile, |m| {
                m["supported_profile"]["copper_layers"] = 3.into();
                m["layers"].as_array_mut().unwrap().remove(2);
                m["layers"][2]["index"] = 2.into();
            }),
            // B.Cu unmapped: the session never routes on it.
            ("dropped", mapping, |m| {
                m["layers"].as_array_mut().unwrap().remove(3);
            }),
            ("swapped", mapping, |m| {
                m["layers"][1]["kicad_name"] = "In2.Cu".into();
                m["layers"][2]["kicad_name"] = "In1.Cu".into();
            }),
        ];
        for (name, expected, edit) in cases {
            let mut edited = manifest.clone();
            edit(&mut edited);
            let error = crate::specctra_ses::parse_import_plan(
                &board,
                FOUR_LAYERS,
                &edited.to_string(),
                ses,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
        manifest["schema_version"] = 2.into();
        crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &manifest.to_string(), ses)
            .unwrap();
    }

    /// The four-layer board with a GND plane on In1.Cu, a VCC plane on In2.Cu
    /// and a GND pour on F.Cu and B.Cu, filled by KiCad. See its README.
    const ZONES: &str = include_str!("../tests/fixtures/specctra_zones.kicad_pcb");
    const ZONES_NATIVE_FILE: &str =
        include_str!("../tests/fixtures/specctra_zones.native-kicad-10.dsn");
    const ZONES_SES: &str =
        include_str!("../tests/fixtures/specctra_zones.native-kicad-10.freerouting-2.3.0.ses");

    /// Without the header's courtyard polygon, as for the four-layer board.
    fn zones_native() -> String {
        ZONES_NATIVE_FILE
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    fn zone_counts(manifest: &serde_json::Value) -> Vec<(String, String, u64)> {
        manifest["zones"]
            .as_array()
            .unwrap()
            .iter()
            .map(|zone| {
                (
                    zone["layer"].as_str().unwrap().to_string(),
                    zone["net"].as_str().unwrap().to_string(),
                    zone["count"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn copper_zones_export_as_planes_on_each_layer_they_cover() {
        let export = native_baseline(ZONES);
        assert_eq!(export.copper_zone_count, 3);
        let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
        let square = vec![
            (81000, -31000),
            (129000, -31000),
            (129000, -69000),
            (81000, -69000),
            (81000, -31000),
        ];
        let pour = vec![
            (82000, -60000),
            (95000, -60000),
            (95000, -68000),
            (82000, -68000),
            (82000, -60000),
        ];
        assert_eq!(
            identity.planes,
            [
                ("GND".to_string(), "B.Cu".to_string(), pour.clone()),
                ("GND".to_string(), "F.Cu".to_string(), pour),
                ("GND".to_string(), "In1.Cu".to_string(), square.clone()),
                ("VCC".to_string(), "In2.Cu".to_string(), square),
            ]
        );
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(manifest["schema_version"], 3);
        assert_eq!(manifest["supported_profile"]["copper_zones"], true);
        let count = |layer: &str, net: &str| (layer.to_string(), net.to_string(), 1);
        assert_eq!(
            zone_counts(&manifest),
            [
                count("B.Cu", "GND"),
                count("F.Cu", "GND"),
                count("In1.Cu", "GND"),
                count("In2.Cu", "VCC"),
            ]
        );
        // A board without zones writes neither planes nor an inventory.
        let plain = native_baseline(FOUR_LAYERS);
        assert_eq!(plain.copper_zone_count, 0);
        assert!(!plain.dsn.contains("(plane "), "{}", plain.dsn);
        let manifest: serde_json::Value = serde_json::from_str(&plain.manifest).unwrap();
        assert_eq!(manifest["supported_profile"]["copper_zones"], false);
        assert!(manifest.get("zones").is_none(), "{manifest}");
    }

    /// KiCad's own export writes the same planes, in another order.
    #[test]
    fn zone_native_dsn_matches_the_rust_export() {
        let adopted = adopt_native_dsn(native_baseline(ZONES), zones_native()).unwrap();
        assert_eq!(adopted.dsn, zones_native());
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
        assert_eq!(zone_counts(&manifest).len(), 4);

        let moved = zones_native().replacen("95000 -60000", "95000 -61000", 1);
        assert_ne!(moved, zones_native());
        let renetted = zones_native().replacen("(plane VCC", "(plane GND", 1);
        let mut dropped = zones_native();
        let start = dropped.find("    (plane VCC").unwrap();
        let end = start + dropped[start..].find("))\n").unwrap() + 3;
        dropped.replace_range(start..end, "");
        for (name, native) in [
            ("moved", moved),
            ("renetted", renetted),
            ("dropped", dropped),
        ] {
            let error = adopt_native_dsn(native_baseline(ZONES), native)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("native DSN changed the copper-zone planes"),
                "{name}: {error}"
            );
        }
    }

    /// Freerouting 2.3.0 reaches the planes through vias; the planner takes
    /// the session and carries the zone inventory to the import's response.
    #[test]
    fn freerouting_routes_to_the_planes_and_the_planner_keeps_the_zones() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_zones.kicad_pcb");
        std::fs::write(&board, ZONES).unwrap();
        let baseline = export_dsn(&board, ZONES, &native_fixture_rules()).unwrap();
        let adopted = adopt_native_dsn(baseline, zones_native()).unwrap();
        let plan =
            crate::specctra_ses::parse_import_plan(&board, ZONES, &adopted.manifest, ZONES_SES)
                .unwrap();
        assert!(!plan.vias.is_empty());
        assert_eq!(
            plan.zones
                .iter()
                .map(|zone| (zone.layer.as_str(), zone.net.as_str(), zone.count))
                .collect::<Vec<_>>(),
            [
                ("B.Cu", "GND", 1),
                ("F.Cu", "GND", 1),
                ("In1.Cu", "GND", 1),
                ("In2.Cu", "VCC", 1)
            ]
        );
    }

    /// The manifest's zone inventory is the live board's, from schema 3.
    #[test]
    fn manifest_zone_inventory_must_match_the_live_board() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("specctra_zones.kicad_pcb");
        std::fs::write(&board, ZONES).unwrap();
        let adopted = adopt_native_dsn(
            export_dsn(&board, ZONES, &native_fixture_rules()).unwrap(),
            zones_native(),
        )
        .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
        let inventory = "manifest copper-zone inventory does not match the live board snapshot";
        let flag = "manifest copper-zone flag does not match its zone inventory";
        type ManifestEdit = fn(&mut serde_json::Value);
        let cases: [(&str, &str, ManifestEdit); 6] = [
            ("count", inventory, |m| m["zones"][0]["count"] = 2.into()),
            ("net", inventory, |m| m["zones"][3]["net"] = "GND".into()),
            ("layer", inventory, |m| {
                m["zones"][2]["layer"] = "In2.Cu".into()
            }),
            ("dropped", inventory, |m| {
                m["zones"].as_array_mut().unwrap().remove(1);
            }),
            ("unflagged", flag, |m| {
                m["supported_profile"]["copper_zones"] = false.into()
            }),
            ("schema 2", flag, |m| m["schema_version"] = 2.into()),
        ];
        for (name, expected, edit) in cases {
            let mut edited = manifest.clone();
            edit(&mut edited);
            let error = crate::specctra_ses::parse_import_plan(
                &board,
                ZONES,
                &edited.to_string(),
                ZONES_SES,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
        // A zone-free manifest against a board with zones.
        let mut emptied = manifest.clone();
        emptied.as_object_mut().unwrap().remove("zones");
        emptied["supported_profile"]["copper_zones"] = false.into();
        let error =
            crate::specctra_ses::parse_import_plan(&board, ZONES, &emptied.to_string(), ZONES_SES)
                .unwrap_err()
                .to_string();
        assert!(error.contains(inventory), "{error}");
    }

    /// What the planes cannot say exactly is refused, not approximated.
    #[test]
    fn zones_the_planes_cannot_describe_are_refused() {
        let plane = "(layer \"In1.Cu\")\n\t\t(uuid";
        let outline = "(polygon\n\t\t\t(pts\n\t\t\t\t(xy 81 31)";
        let cases = [
            (
                "rule area",
                ZONES.replacen("(name \"gnd_plane\")", "(name \"gnd_plane\") (keepout (tracks not_allowed))", 1),
                "board contains a rule area",
            ),
            (
                "no net",
                ZONES.replacen("(net \"VCC\")\n\t\t(layer \"In2.Cu\")", "(net \"\")\n\t\t(layer \"In2.Cu\")", 1),
                "copper zone 'vcc_plane' has no net",
            ),
            (
                "cutout",
                ZONES.replacen(
                    outline,
                    "(polygon (pts (xy 100 40) (xy 110 40) (xy 110 45)))\n\t\t(polygon\n\t\t\t(pts\n\t\t\t\t(xy 81 31)",
                    1,
                ),
                "copper zone 'gnd_plane' on net 'GND' has 2 outlines",
            ),
            (
                "arc",
                ZONES.replacen(
                    "(xy 81 31) (xy 129 31)",
                    "(xy 81 31) (arc (start 100 31) (mid 105 30) (end 110 31)) (xy 129 31)",
                    1,
                ),
                "outline has a 'arc' point",
            ),
            (
                "not copper",
                ZONES.replacen(plane, "(layer \"F.SilkS\")\n\t\t(uuid", 1),
                "copper zone 'gnd_plane' is on 'F.SilkS', which is not a copper layer of the board",
            ),
            (
                "padless net",
                ZONES.replacen("(net \"VCC\")\n\t\t(layer \"In2.Cu\")", "(net \"SPARE\")\n\t\t(layer \"In2.Cu\")", 1),
                "copper zone on net 'SPARE', which has no pad",
            ),
            (
                "footprint zone",
                ZONES.replacen(
                    "(property \"Reference\" \"J1\"",
                    "(zone (net \"GND\") (layer \"F.Cu\") (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))\n\t\t(property \"Reference\" \"J1\"",
                    1,
                ),
                "footprint J1 contains a zone",
            ),
        ];
        for (name, board, expected) in cases {
            assert_ne!(board, ZONES, "{name}");
            let error = export_dsn(
                Path::new("board.kicad_pcb"),
                &board,
                &native_fixture_rules(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
        // `*.Cu` covers every copper layer, top to bottom.
        let all = ZONES.replacen(plane, "(layers \"*.Cu\")\n\t\t(uuid", 1);
        let export =
            export_dsn(Path::new("board.kicad_pcb"), &all, &native_fixture_rules()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(
            zone_counts(&manifest)
                .iter()
                .filter(|(_, net, _)| net == "GND")
                .map(|(layer, _, count)| (layer.as_str(), *count))
                .collect::<Vec<_>>(),
            [("B.Cu", 2), ("F.Cu", 2), ("In1.Cu", 1), ("In2.Cu", 1)]
        );
    }

    /// A copper layer the board marks `power` is a DSN `power` layer, where
    /// Freerouting lays no track, as KiCad 10.0.8 writes it (`mixed` stays
    /// `signal`); the native DSN must agree.
    #[test]
    fn power_layers_export_as_power_layers() {
        let board = ZONES
            .replacen("(4 \"In1.Cu\" signal)", "(4 \"In1.Cu\" power)", 1)
            .replacen("(6 \"In2.Cu\" signal)", "(6 \"In2.Cu\" mixed)", 1);
        assert_ne!(board, ZONES);
        let export = native_baseline(&board);
        let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
        assert_eq!(
            identity.layer_types.values().collect::<Vec<_>>(),
            ["signal", "power", "signal", "signal"]
        );
        let power_native = zones_native().replacen(
            "(layer In1.Cu\n      (type signal)",
            "(layer In1.Cu\n      (type power)",
            1,
        );
        assert_ne!(power_native, zones_native());
        adopt_native_dsn(native_baseline(&board), power_native.clone()).unwrap();
        for (name, baseline, native) in [
            (
                "signal written as power",
                native_baseline(ZONES),
                power_native,
            ),
            (
                "power written as signal",
                native_baseline(&board),
                zones_native(),
            ),
        ] {
            let error = adopt_native_dsn(baseline, native).unwrap_err().to_string();
            assert!(
                error.contains("native DSN changed a copper layer's signal or power type"),
                "{name}: {error}"
            );
        }
    }
}
