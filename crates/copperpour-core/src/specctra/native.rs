//! Adopt KiCad-generated DSN by correlating it with the validated board.

use super::outline::closed_outline;
use super::writer::{oval_spine, roundrect_outline};
use super::{
    class_name, ExportBundle, Manifest, ManifestPadstack, PadShape, RoutingSnapshot, RuleAreaKind,
    I64_UPPER_EXCLUSIVE, PIN_POSITION_TOLERANCE_UM,
};
use anyhow::{bail, Context, Result};
use copperpour_sexp::{parse_sexp, SexpNode};
use specctra::read::{ListTokenizer, ReadDsn};
use specctra::structure as dsn;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Cursor};

impl RoutingSnapshot {
    pub(crate) fn adopt_native(&self, native_dsn: String) -> Result<ExportBundle> {
        let identity = self.identity()?;
        let baseline = self.bundle(
            String::new(),
            "KiCad-native board boundary; source revision bound, not flattened by the Rust writer",
        )?;
        adopt_native_with_identity(baseline, identity, native_dsn)
    }

    /// The same identities the existing native correlator checks, constructed
    /// from the validated board model rather than a Rust-generated DSN.
    pub(super) fn identity(&self) -> Result<DsnIdentity> {
        let mut padstacks = BTreeMap::new();
        for (key, name) in &self.padstack_names {
            padstacks.insert(name.clone(), key.layers.iter().map(|layer| {
                Ok(match key.shape {
                    PadShape::Circle => DsnShapeIdentity::Circle { layer: layer.clone(), diameter: key.size_x_um },
                    PadShape::Rect => {
                        if key.size_x_um % 2 != 0 || key.size_y_um % 2 != 0 {
                            bail!("native correlation requires integer-micrometre rectangle corners");
                        }
                        DsnShapeIdentity::Rect { layer: layer.clone(), x1: -key.size_x_um/2, y1: -key.size_y_um/2, x2: key.size_x_um/2, y2: key.size_y_um/2 }
                    },
                    PadShape::RoundRect => DsnShapeIdentity::Polygon {
                        layer: layer.clone(), points: roundrect_outline(key.size_x_um, key.size_y_um, key.corner_radius_um.unwrap_or(0)).into_iter()
                            .map(|(x,y)| Ok((OrderedF64::new(x)?, OrderedF64::new(y)?))).collect::<Result<_>>()?,
                    },
                    PadShape::Oval => DsnShapeIdentity::Path {
                        layer: layer.clone(), width: OrderedF64::new(key.size_x_um.min(key.size_y_um) as f64)?,
                        points: oval_spine(key.size_x_um, key.size_y_um).into_iter()
                            .map(|(x,y)| Ok((OrderedF64::new(x)?, OrderedF64::new(y)?))).collect::<Result<_>>()?,
                    },
                })
            }).collect::<Result<_>>()?);
        }
        for (rule, name) in &self.via_names {
            padstacks.insert(
                name.clone(),
                self.copper_layers
                    .iter()
                    .map(|layer| DsnShapeIdentity::Circle {
                        layer: layer.clone(),
                        diameter: rule.via_diameter_um,
                    })
                    .collect(),
            );
        }
        let mut components = BTreeMap::new();
        for footprint in &self.footprints {
            let pins = footprint
                .pads
                .iter()
                .map(|pad| {
                    Ok((
                        pad.pin.clone(),
                        DsnPinIdentity {
                            padstack: self.padstack_names[&pad.padstack].clone(),
                            rotation: OrderedF64::new(pad.rotation_degrees)?,
                            x: OrderedF64::new(pad.x_um)?,
                            y: OrderedF64::new(pad.y_um)?,
                        },
                    ))
                })
                .collect::<Result<_>>()?;
            components.insert(
                footprint.reference.clone(),
                DsnPlacementIdentity {
                    image_name: footprint.image_name.clone(),
                    x: footprint.x_um,
                    y: footprint.y_um,
                    side: footprint.side.name().into(),
                    rotation: OrderedF64::new(footprint.rotation_degrees)?,
                    pins,
                },
            );
        }
        let mut planes = self
            .zones
            .iter()
            .flat_map(|zone| {
                zone.layers.iter().map(|layer| {
                    (
                        zone.net.clone(),
                        layer.clone(),
                        closed_outline(&zone.outline),
                    )
                })
            })
            .collect::<Vec<_>>();
        planes.sort();
        let mut keepouts = self
            .rule_areas
            .iter()
            .flat_map(|area| {
                area.layers
                    .iter()
                    .map(|layer| (area.kind, layer.clone(), closed_outline(&area.outline)))
            })
            .collect::<Vec<_>>();
        keepouts.sort();
        Ok(DsnIdentity {
            layers: self.copper_layers.iter().cloned().enumerate().collect(),
            layer_types: self
                .copper_layers
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    (
                        i,
                        if self.power_layers.contains(name) {
                            "power"
                        } else {
                            "signal"
                        }
                        .into(),
                    )
                })
                .collect(),
            components,
            nets: self
                .net_pins
                .iter()
                .map(|(n, p)| (n.clone(), p.iter().cloned().collect()))
                .collect(),
            classes: self
                .class_nets
                .iter()
                .enumerate()
                .map(|(i, (rule, nets))| {
                    (
                        class_name(i),
                        DsnClassIdentity {
                            nets: nets.iter().cloned().collect(),
                            via_name: self.via_names[rule].clone(),
                            width: rule.track_width_um,
                            clearance: rule.clearance_um,
                        },
                    )
                })
                .collect(),
            padstacks,
            planes,
            keepouts,
        })
    }
}

/// Compatibility test entry point for the existing Rust/native differential
/// corpus. Production adopts against RoutingSnapshot::identity, without first
/// generating a Rust DSN or checking its outline.
#[cfg(test)]
pub(crate) fn adopt_native_dsn(baseline: ExportBundle, native_dsn: String) -> Result<ExportBundle> {
    let tree = parse_dsn_sexp(&baseline.dsn).context("parse Copperpour baseline DSN")?;
    adopt_native_with_identity(baseline, dsn_identity(&tree)?, native_dsn)
}

fn adopt_native_with_identity(
    mut baseline: ExportBundle,
    baseline_identity: DsnIdentity,
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
    let native_tree = parse_dsn_sexp(&native_dsn).context("parse KiCad native DSN")?;
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
    refuse_merged_padstacks(&manifest, &padstack_names)?;
    validate_native_nets(&baseline_identity, &native_identity)?;
    if baseline_identity.planes != native_identity.planes {
        bail!("native DSN changed the copper-zone planes");
    }
    if baseline_identity.keepouts != native_identity.keepouts {
        bail!("native DSN changed the rule-area keep-outs");
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    x: OrderedF64,
    y: OrderedF64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct OrderedF64(u64);

impl OrderedF64 {
    pub(super) fn new(value: f64) -> Result<Self> {
        if !value.is_finite() {
            bail!("DSN contains a non-finite number");
        }
        Ok(Self(value.to_bits()))
    }

    fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DsnClassIdentity {
    nets: BTreeSet<String>,
    via_name: String,
    width: i64,
    clearance: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DsnShapeIdentity {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DsnIdentity {
    pub(super) layers: BTreeMap<usize, String>,
    /// Each layer's `(type …)`, by index.
    pub(super) layer_types: BTreeMap<usize, String>,
    components: BTreeMap<String, DsnPlacementIdentity>,
    nets: BTreeMap<String, BTreeSet<String>>,
    classes: BTreeMap<String, DsnClassIdentity>,
    pub(super) padstacks: BTreeMap<String, Vec<DsnShapeIdentity>>,
    /// Every `plane`, sorted: the two exporters need not list them in the
    /// same order.
    pub(super) planes: Vec<DsnPlaneIdentity>,
    /// Every rule-area keep-out, sorted.
    pub(super) keepouts: Vec<DsnKeepoutIdentity>,
}

/// `(net, layer, closed outline)` of one DSN `plane`.
type DsnPlaneIdentity = (String, String, Vec<(i64, i64)>);

/// `(kind, layer, closed outline)` of one DSN `keepout`, `wire_keepout`, or `via_keepout`.
type DsnKeepoutIdentity = (RuleAreaKind, String, Vec<(i64, i64)>);

fn dsn_integer_polygon(polygon: &SexpNode, label: &str) -> Result<(String, Vec<(i64, i64)>)> {
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
                    .with_context(|| format!("{label} coordinate is not a supported integer"))
            };
            Ok((integer(*x)?, integer(*y)?))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((layer, points))
}

pub(super) fn validate_dsn_syntax(source: &str, label: &str) -> Result<()> {
    let normalized = normalize_courtyard_outline_polygons(source);
    let cursor = Cursor::new(normalized.as_bytes());
    let mut tokenizer = ListTokenizer::new(BufReader::new(cursor));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .map_err(|error| anyhow::anyhow!("{label} failed Specctra parser validation: {error}"))?;
    Ok(())
}

/// `topola_specctra` 0.1.0's `Outline` grammar only accepts `(outline (path ...))`,
/// whereas KiCad writes footprint `fp_rect` courtyards as
/// `(outline (polygon <layer> <width> <coords...>))`. Both carry the same
/// arguments, so rewrite `polygon` to `path   ` (same byte length to keep line
/// and column numbers) in a temporary copy for syntax validation.
pub(super) fn normalize_courtyard_outline_polygons(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_quotes = false;
    while i < bytes.len() {
        if !in_quotes && bytes[i..].starts_with(b"(string_quote \")") {
            out.extend_from_slice(b"(string_quote \")");
            i += b"(string_quote \")".len();
            continue;
        }
        if bytes[i] == b'"' {
            in_quotes = !in_quotes;
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        if in_quotes && bytes[i] == b'\\' && i + 1 < bytes.len() {
            out.push(bytes[i]);
            out.push(bytes[i + 1]);
            i += 2;
            continue;
        }
        if !in_quotes && bytes[i..].starts_with(b"(outline") {
            let mut j = i + b"(outline".len();
            if j < bytes.len() && (bytes[j].is_ascii_whitespace() || bytes[j] == b'(') {
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if bytes[j..].starts_with(b"polygon") {
                        let after = j + b"polygon".len();
                        if after < bytes.len() && bytes[after].is_ascii_whitespace() {
                            out.extend_from_slice(&bytes[i..j]);
                            out.extend_from_slice(b"path   ");
                            i = after;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).expect("UTF-8 input stays valid UTF-8")
}

pub(super) fn parse_dsn_sexp(source: &str) -> Result<SexpNode> {
    // Specctra represents the quote delimiter as the bare token
    // `(string_quote ")`. KiCad's S-expression parser correctly treats the
    // opening quote as a string delimiter, so give only this metadata token an
    // escaped representation for structural inspection. The original DSN is
    // retained byte-for-byte and independently validated by Topola's parser.
    let compatible = source.replace("(string_quote \")", "(string_quote \"\\\"\")");
    parse_sexp(&compatible).context("parse normalized Specctra S-expression")
}

pub(super) fn dsn_identity(root: &SexpNode) -> Result<DsnIdentity> {
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
        let (layer, points) = dsn_integer_polygon(polygon, "DSN plane")?;
        planes.push((net, layer, points));
    }
    planes.sort();

    let mut keepouts = Vec::new();
    for node in structure.children().unwrap_or(&[]) {
        let kind = match node.head() {
            Some("keepout") => RuleAreaKind::Keepout,
            Some("wire_keepout") => RuleAreaKind::WireKeepout,
            Some("via_keepout") => RuleAreaKind::ViaKeepout,
            Some(other @ ("place_keepout" | "bend_keepout" | "elongate_keepout")) => {
                bail!("DSN contains unsupported keep-out kind '{other}'");
            }
            _ => continue,
        };
        let head = node.head().unwrap_or("keepout");
        let _id = dsn_atom(node, 1, &format!("{head} id"))?;
        let children = node.children().map_or(0, <[SexpNode]>::len);
        let polygon = node
            .get(2)
            .filter(|polygon| polygon.head() == Some("polygon") && children == 3)
            .with_context(|| format!("DSN {head} is not one polygon"))?;
        // KiCad's native DSN exporter encodes internal board cutouts (`Edge.Cuts`
        // holes/circles) as `(keepout "" (polygon signal 0 ...))` with flattened
        // floating-point points rather than rule-area keep-outs on copper layers.
        if kind == RuleAreaKind::Keepout
            && dsn_atom(polygon, 1, "polygon layer").ok() == Some("signal")
        {
            let _ = dsn_polygon(polygon)?;
            continue;
        }
        let (layer, points) = dsn_integer_polygon(polygon, &format!("DSN {head}"))?;
        keepouts.push((kind, layer, points));
    }
    keepouts.sort();

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
                x: OrderedF64::new(dsn_number(pin, at + 1, "pin x")?)?,
                y: OrderedF64::new(dsn_number(pin, at + 2, "pin y")?)?,
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
        keepouts,
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
            if (baseline_pin.x.value() - native_pin.x.value()).abs() > PIN_POSITION_TOLERANCE_UM
                || (baseline_pin.y.value() - native_pin.y.value()).abs() > PIN_POSITION_TOLERANCE_UM
            {
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

/// KiCad names a pad's padstack after its copper only, so two pads that share
/// a shape but not a drill get one native padstack. The reverse manifest would
/// then hold that name twice, and the importer refuses such a manifest only
/// after routing. Refuse the adoption instead, naming the pads on each side.
fn refuse_merged_padstacks(
    manifest: &Manifest,
    padstack_names: &BTreeMap<String, String>,
) -> Result<()> {
    let mut by_native: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (baseline, native) in padstack_names {
        by_native.entry(native).or_default().push(baseline);
    }
    let Some((native, merged)) = by_native.iter().find(|(_, merged)| merged.len() > 1) else {
        return Ok(());
    };
    let sides = merged
        .iter()
        .map(|baseline| {
            let pads = manifest
                .components
                .iter()
                .flat_map(|component| {
                    component
                        .pads
                        .iter()
                        .filter(|pin| pin.padstack_name == *baseline)
                        .map(move |pin| format!("{}-{}", component.reference, pin.pad_number))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let drill = match manifest
                .padstacks
                .iter()
                .find(|padstack| padstack.name == *baseline)
                .and_then(|padstack| padstack.drill_um)
            {
                Some(drill_um) => format!("drill {drill_um} um"),
                None => "no drill".to_string(),
            };
            format!("{pads} ({drill})")
        })
        .collect::<Vec<_>>()
        .join(" and ");
    bail!(
        "KiCad native DSN gives one padstack '{native}' to pads that the Rust export keeps apart: {sides}"
    );
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
pub(super) fn polygons_trace_roundrect(
    shapes: &[DsnShapeIdentity],
    model: &ManifestPadstack,
) -> bool {
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
