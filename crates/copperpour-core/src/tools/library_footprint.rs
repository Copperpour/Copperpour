//! What a library footprint carries besides its pads, drawings and fields,
//! read into the typed messages KiCad's API takes.
//!
//! Shared by `update_pcb_from_schematic`, which places new footprints, and
//! `update_footprints_from_library`, which refreshes placed ones, so both put
//! the same library data on the board. Before #789 only the refresh read it,
//! and every footprint the sync placed reached the board with no mounting
//! style, no exclusion flags, no description or keywords and no 3D model.

use super::library::PadFabricationProperty;
use super::solder_margins::Margin;
use anyhow::{bail, Context, Result};
use copperpour_ipc::gen::kiapi;
use copperpour_sexp::layers::{flip_layer, CopperStack};
use copperpour_sexp::SexpNode;
use prost::Message;
use std::collections::{BTreeMap, BTreeSet};

/// The instance attributes a library footprint's `(attr …)` declares.
///
/// A footprint with no `(attr …)` gets KiCad's default: no mounting style,
/// which KiCad treats as unspecified. 17 of the 15,451 footprints KiCad 10.0.5
/// ships have none. An unknown token is refused rather than dropped, because
/// it would change what the board says about the part.
pub(crate) fn attributes(root: &SexpNode) -> Result<kiapi::board::types::FootprintAttributes> {
    use kiapi::board::types::FootprintMountingStyle;

    let mut attributes = kiapi::board::types::FootprintAttributes::default();
    let Some(attr) = root.find("attr") else {
        return Ok(attributes);
    };
    for value in attr.children().unwrap_or_default().iter().skip(1) {
        match value
            .as_str()
            .context("footprint attr contains a non-atom")?
        {
            "smd" => attributes.mounting_style = FootprintMountingStyle::FmsSmd as i32,
            "through_hole" => {
                attributes.mounting_style = FootprintMountingStyle::FmsThroughHole as i32
            }
            "board_only" => attributes.not_in_schematic = true,
            "exclude_from_pos_files" => attributes.exclude_from_position_files = true,
            "exclude_from_bom" => attributes.exclude_from_bill_of_materials = true,
            "allow_missing_courtyard" => attributes.exempt_from_courtyard_requirement = true,
            "dnp" => attributes.do_not_populate = true,
            "allow_soldermask_bridges" => attributes.allow_soldermask_bridges = true,
            unsupported => bail!("footprint attribute '{unsupported}' is not supported"),
        }
    }
    Ok(attributes)
}

/// The library description and keywords. KiCad keeps these on the footprint
/// definition, not the instance, in the same message type as the attributes.
pub(crate) fn description_and_keywords(
    root: &SexpNode,
) -> kiapi::board::types::FootprintAttributes {
    kiapi::board::types::FootprintAttributes {
        description: root.find_str("descr").unwrap_or_default().to_string(),
        keywords: root.find_str("tags").unwrap_or_default().to_string(),
        ..Default::default()
    }
}

/// Each pad's fabrication property, from the footprint's `pad` nodes, which
/// `extract_pad_definitions` reads as `pads` pads in the same order.
pub(crate) fn pad_properties(
    pad_nodes: &[&SexpNode],
    pads: usize,
) -> Result<Vec<PadFabricationProperty>> {
    if pad_nodes.len() != pads {
        bail!("read {} pad properties for {pads} pads", pad_nodes.len());
    }
    Ok(pad_nodes
        .iter()
        .copied()
        .map(PadFabricationProperty::of_pad)
        .collect())
}

/// KiCad's API value for a pad's fabrication property, as
/// `pcbnew/api/api_pcb_enums.cpp` maps `PAD_PROP`.
pub(crate) fn fab_property(
    property: PadFabricationProperty,
) -> kiapi::board::types::PadFabricationProperty {
    use kiapi::board::types::PadFabricationProperty as Proto;
    match property {
        PadFabricationProperty::None => Proto::PfpNone,
        PadFabricationProperty::Bga => Proto::PfpBga,
        PadFabricationProperty::FiducialGlobal => Proto::PfpFiducialGlobal,
        PadFabricationProperty::FiducialLocal => Proto::PfpFiducialLocal,
        PadFabricationProperty::Testpoint => Proto::PfpTestpoint,
        PadFabricationProperty::Heatsink => Proto::PfpHeatsink,
        PadFabricationProperty::Castellated => Proto::PfpCastellated,
        PadFabricationProperty::Mechanical => Proto::PfpMechanical,
        PadFabricationProperty::Pressfit => Proto::PfpPressfit,
    }
}

/// Whether `version`'s API carries pad fabrication properties and reference
/// points: both arrived in KiCad 10.0.7.
pub(crate) fn carries_kicad_10_0_7_fields(version: &copperpour_ipc::IpcKiCadVersion) -> bool {
    (version.major, version.minor, version.patch) >= (10, 0, 7)
}

/// Whether `version`'s API keeps a footprint's net-tie pad groups. Every
/// KiCad 10 release sends a group as its pad numbers, split at commas and
/// whitespace (`FOOTPRINT::Serialize`), and joins them again when it takes a
/// footprint (`FOOTPRINT::Deserialize`): 10.0.1 and later with ", ", as its
/// own files write a group, but 10.0.0 with a space. KiCad reads a group as
/// the pad numbers between its commas (`MapPadNumbersToNetTieGroups`), so
/// 10.0.0 would hold "1 2" as one pad named "1 2" and tie nothing, while its
/// readback still showed pads 1 and 2.
pub(crate) fn carries_net_ties(version: &copperpour_ipc::IpcKiCadVersion) -> bool {
    (version.major, version.minor, version.patch) >= (10, 0, 1)
}

/// What a placement sends of the library data that not every KiCad 10
/// release takes over its API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CarriedByKiCad {
    /// Pad fabrication properties and reference points (KiCad 10.0.7+).
    pub(crate) fields_10_0_7: bool,
    /// Net-tie pad groups (KiCad 10.0.1+, [`carries_net_ties`]).
    pub(crate) net_ties: bool,
}

impl CarriedByKiCad {
    /// Both, as KiCad 10.0.7 and later take them.
    #[cfg(test)]
    pub(crate) const ALL: Self = Self {
        fields_10_0_7: true,
        net_ties: true,
    };
}

/// The fabrication property KiCad reports for a pad, or `None` for
/// `PFP_NONE` and for `PFP_UNKNOWN`, which KiCad before 10.0.7 sends for
/// every pad.
pub(crate) fn reported_fab_property(value: i32) -> Option<PadFabricationProperty> {
    use kiapi::board::types::PadFabricationProperty as Proto;
    match Proto::try_from(value).ok()? {
        Proto::PfpUnknown | Proto::PfpNone => None,
        Proto::PfpBga => Some(PadFabricationProperty::Bga),
        Proto::PfpFiducialGlobal => Some(PadFabricationProperty::FiducialGlobal),
        Proto::PfpFiducialLocal => Some(PadFabricationProperty::FiducialLocal),
        Proto::PfpTestpoint => Some(PadFabricationProperty::Testpoint),
        Proto::PfpHeatsink => Some(PadFabricationProperty::Heatsink),
        Proto::PfpCastellated => Some(PadFabricationProperty::Castellated),
        Proto::PfpMechanical => Some(PadFabricationProperty::Mechanical),
        Proto::PfpPressfit => Some(PadFabricationProperty::Pressfit),
    }
}

/// Every `(model …)`, with its offset, scale, rotation, opacity and
/// visibility.
///
/// A hidden model is written two ways: the bare `hide` atom, and the
/// `(hide yes)` list KiCad 10 writes. 16 models in KiCad 10.0.5's own library
/// use the list form. Both are read; any other clause is refused.
///
/// The vector values go into the `*_nm` fields unconverted, because that is
/// how KiCad's own serializer packs a 3D model's millimetre offset, scale and
/// degrees. An absent opacity is KiCad's default, fully opaque; a present one
/// that is not a number is refused rather than read as that default.
pub(crate) fn models(root: &SexpNode) -> Result<Vec<kiapi::board::types::Footprint3DModel>> {
    root.find_all("model")
        .into_iter()
        .map(|model| {
            let mut hidden = false;
            for child in model.children().unwrap_or_default().iter().skip(2) {
                let Some(tag) = child.head() else {
                    if child.as_str() == Some("hide") {
                        hidden = true;
                        continue;
                    }
                    bail!("3D model contains an unsupported atom");
                };
                match tag {
                    "offset" | "scale" | "rotate" | "opacity" => {}
                    "hide" => {
                        hidden = match child.get(1).and_then(SexpNode::as_str) {
                            Some("yes") => true,
                            Some("no") => false,
                            _ => bail!("3D model hide must be yes or no"),
                        }
                    }
                    _ => bail!("3D model clause '{tag}' is not supported"),
                }
            }
            let vector = |tag: &str, default: [f64; 3]| -> Result<kiapi::common::types::Vector3D> {
                let Some(wrapper) = model.find(tag) else {
                    return Ok(kiapi::common::types::Vector3D {
                        x_nm: default[0],
                        y_nm: default[1],
                        z_nm: default[2],
                    });
                };
                let xyz = wrapper
                    .find("xyz")
                    .with_context(|| format!("3D model {tag} is missing xyz"))?;
                Ok(kiapi::common::types::Vector3D {
                    x_nm: xyz
                        .get_f64(1)
                        .with_context(|| format!("3D model {tag}.x is invalid"))?,
                    y_nm: xyz
                        .get_f64(2)
                        .with_context(|| format!("3D model {tag}.y is invalid"))?,
                    z_nm: xyz
                        .get_f64(3)
                        .with_context(|| format!("3D model {tag}.z is invalid"))?,
                })
            };
            Ok(kiapi::board::types::Footprint3DModel {
                filename: model
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .context("3D model is missing its filename")?
                    .to_string(),
                scale: Some(vector("scale", [1.0, 1.0, 1.0])?),
                rotation: Some(vector("rotate", [0.0, 0.0, 0.0])?),
                offset: Some(vector("offset", [0.0, 0.0, 0.0])?),
                visible: !hidden,
                opacity: match model.find("opacity") {
                    None => 1.0,
                    Some(opacity) => opacity
                        .get_f64(1)
                        .context("3D model opacity is not a number")?,
                },
            })
        })
        .collect()
}

/// A library footprint's properties as KiCad's typed fields, in the footprint's
/// own coordinates.
///
/// The mandatory four keep their own slots; every other property is a custom
/// field. A mandatory field missing its position, layer or effects has no
/// typed form, and only its value is kept.
#[derive(Debug, Clone, Default)]
pub(crate) struct LibraryProperties {
    pub(crate) reference: Option<kiapi::board::types::Field>,
    pub(crate) value: Option<kiapi::board::types::Field>,
    pub(crate) datasheet: Option<kiapi::board::types::Field>,
    pub(crate) description: Option<kiapi::board::types::Field>,
    pub(crate) datasheet_value: Option<String>,
    pub(crate) description_value: Option<String>,
    pub(crate) custom: Vec<kiapi::board::types::Field>,
}

/// Read every `(property …)` of a library footprint into [`LibraryProperties`].
///
/// Every clause is either represented or refused: accepting a property while
/// dropping part of its authored presentation would place it differently from
/// the library. A name that appears twice is refused too, because KiCad's own
/// reader keeps one of them and which one is not something to guess.
pub(crate) fn properties(root: &SexpNode) -> Result<LibraryProperties> {
    let mut names = BTreeSet::new();
    let mut properties = LibraryProperties::default();
    for node in root.find_all("property") {
        let name = node
            .get(1)
            .and_then(SexpNode::as_str)
            .context("property is missing its name")?;
        if !names.insert(name.to_string()) {
            bail!("property '{name}' appears more than once in the library footprint");
        }

        // Mandatory and custom properties share one lossless clause validator.
        // The mandatory values keep their existing first-class IPC fields; the
        // shared parser proves that none of their authored clauses would be
        // silently ignored without requiring a typed custom Field.
        let mandatory = matches!(name, "Reference" | "Value" | "Datasheet" | "Description");
        let parsed = property(node, !mandatory)?;
        let value = node
            .get(2)
            .and_then(SexpNode::as_str)
            .with_context(|| format!("property '{name}' is missing its value"))?;
        match name {
            "Reference" => properties.reference = parsed,
            "Value" => properties.value = parsed,
            "Datasheet" => {
                properties.datasheet = parsed;
                properties.datasheet_value = Some(value.to_string());
            }
            "Description" => {
                properties.description = parsed;
                properties.description_value = Some(value.to_string());
            }
            _ => properties
                .custom
                .push(parsed.context("custom property did not produce a typed field")?),
        }
    }
    Ok(properties)
}

/// Convert a footprint property into the typed `Field` shape carried by
/// KiCad's IPC model. Unknown clauses refuse here: accepting a property while
/// dropping part of its authored presentation would place it differently from
/// the library even when its value survived.
///
/// `(unlocked yes)` is KiCad's keep-upright clause, not a lock: KiCad keeps
/// every footprint text upright unless the clause says yes, and writes the
/// clause exactly when a text is not kept upright. 7,047 of the 49,803
/// properties in KiCad 10.0.5's stock footprints carry it. A field's lock is a
/// separate `(locked …)` clause, which no stock footprint or demo board writes
/// on a property; it is refused like any other clause this reader does not
/// carry, and a field is sent unlocked, as KiCad creates one.
///
/// With `require_typed_field` off, a property missing its position, layer or
/// effects is accepted with no typed form.
fn property(
    property: &SexpNode,
    require_typed_field: bool,
) -> Result<Option<kiapi::board::types::Field>> {
    use kiapi::common::types::LockedState;

    let name = property
        .get(1)
        .and_then(SexpNode::as_str)
        .context("property is missing its name")?;
    let value = property
        .get(2)
        .and_then(SexpNode::as_str)
        .with_context(|| format!("property '{name}' is missing its value"))?;
    let mut position = None;
    let mut rotation = 0.0;
    let mut layer = None;
    let mut hidden = None;
    let mut knockout = None;
    let mut unlocked = None;
    let mut attributes = None;
    let mut identifier = None;

    for clause in property.children().unwrap_or_default().iter().skip(3) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' contains an unsupported atom"))?;
        match tag {
            "at" => {
                if position.is_some() {
                    bail!("property '{name}' contains duplicate 'at' clauses");
                }
                let count = clause.children().map_or(0, |children| children.len());
                if !matches!(count, 3 | 4) {
                    bail!("property '{name}' 'at' must contain x, y, and optional rotation");
                }
                let x = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' has an invalid X position"))?;
                let y = clause
                    .get_f64(2)
                    .with_context(|| format!("property '{name}' has an invalid Y position"))?;
                rotation = clause.get_f64(3).unwrap_or(0.0);
                if !x.is_finite() || !y.is_finite() || !rotation.is_finite() {
                    bail!("property '{name}' position and rotation must be finite");
                }
                position = Some(copperpour_ipc::builders::vec2(x, y));
            }
            "layer" => {
                if layer.is_some() {
                    bail!("property '{name}' contains duplicate 'layer' clauses");
                }
                let layer_name = clause
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .filter(|name| !name.is_empty())
                    .with_context(|| format!("property '{name}' has no layer name"))?;
                if clause.children().map_or(0, |children| children.len()) != 2 {
                    bail!("property '{name}' 'layer' must name exactly one layer");
                }
                layer = Some(
                    copperpour_ipc::builders::try_layer_from_name(layer_name)
                        .with_context(|| format!("property '{name}' has an unsupported layer"))?
                        as i32,
                );
            }
            "hide" => {
                if hidden.is_some() {
                    bail!("property '{name}' contains duplicate 'hide' clauses");
                }
                hidden = Some(yes_no(clause, name, "hide")?);
            }
            "knockout" => {
                if knockout.is_some() {
                    bail!("property '{name}' contains duplicate 'knockout' clauses");
                }
                knockout = Some(yes_no(clause, name, "knockout")?);
            }
            "unlocked" => {
                if unlocked.is_some() {
                    bail!("property '{name}' contains duplicate 'unlocked' clauses");
                }
                unlocked = Some(yes_no(clause, name, "unlocked")?);
            }
            "uuid" | "tstamp" => {
                if let Some(previous) = identifier {
                    bail!(
                        "property '{name}' contains multiple identifier clauses ('{previous}' and '{tag}')"
                    );
                }
                identifier = Some(tag);
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("property '{name}' '{tag}' must contain exactly one identifier");
                }
            }
            "effects" => {
                if attributes.is_some() {
                    bail!("property '{name}' contains duplicate 'effects' clauses");
                }
                attributes = Some(property_effects(clause, name)?);
            }
            unsupported => {
                bail!("property '{name}' clause '{unsupported}' is not supported losslessly")
            }
        }
    }

    if !require_typed_field && (position.is_none() || layer.is_none() || attributes.is_none()) {
        return Ok(None);
    }

    let position =
        position.with_context(|| format!("property '{name}' is missing its 'at' clause"))?;
    let layer =
        layer.with_context(|| format!("property '{name}' is missing its 'layer' clause"))?;
    let mut attributes =
        attributes.with_context(|| format!("property '{name}' is missing its 'effects' clause"))?;
    attributes.angle = Some(kiapi::common::types::Angle {
        value_degrees: rotation,
    });
    attributes.keep_upright = !unlocked.unwrap_or(false);
    Ok(Some(kiapi::board::types::Field {
        id: None,
        name: name.to_string(),
        text: Some(kiapi::board::types::BoardText {
            // A library child's UUID is definition-local and cannot be reused
            // across placed instances. Let KiCad assign the board child ID.
            id: None,
            text: Some(kiapi::common::types::Text {
                position: Some(position),
                attributes: Some(attributes),
                text: value.to_string(),
                hyperlink: String::new(),
            }),
            layer,
            knockout: knockout.unwrap_or(false),
            locked: LockedState::LsUnlocked as i32,
            parent: None,
        }),
        visible: !hidden.unwrap_or(false),
    }))
}

fn yes_no(clause: &SexpNode, name: &str, tag: &str) -> Result<bool> {
    if clause.children().map_or(0, |children| children.len()) != 2 {
        bail!("property '{name}' '{tag}' must contain exactly one yes/no value");
    }
    match clause.get(1).and_then(SexpNode::as_str) {
        Some("yes") => Ok(true),
        Some("no") => Ok(false),
        _ => bail!("property '{name}' '{tag}' must be yes or no"),
    }
}

fn property_effects(
    effects: &SexpNode,
    name: &str,
) -> Result<kiapi::common::types::TextAttributes> {
    use kiapi::common::types::{HorizontalAlignment, VerticalAlignment};

    let mut font = None;
    let mut horizontal = HorizontalAlignment::HaCenter;
    let mut vertical = VerticalAlignment::VaCenter;
    let mut mirrored = false;
    for clause in effects.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' effects contain an unsupported atom"))?;
        match tag {
            "font" => {
                if font.replace(clause).is_some() {
                    bail!("property '{name}' contains duplicate font clauses");
                }
            }
            "justify" => {
                for value in clause.children().unwrap_or_default().iter().skip(1) {
                    match value.as_str().with_context(|| {
                        format!("property '{name}' justify contains a non-atom")
                    })? {
                        "left" if horizontal == HorizontalAlignment::HaCenter => {
                            horizontal = HorizontalAlignment::HaLeft
                        }
                        "right" if horizontal == HorizontalAlignment::HaCenter => {
                            horizontal = HorizontalAlignment::HaRight
                        }
                        "top" if vertical == VerticalAlignment::VaCenter => {
                            vertical = VerticalAlignment::VaTop
                        }
                        "bottom" if vertical == VerticalAlignment::VaCenter => {
                            vertical = VerticalAlignment::VaBottom
                        }
                        "mirror" if !mirrored => mirrored = true,
                        "left" | "right" => bail!(
                            "property '{name}' has conflicting horizontal justification"
                        ),
                        "top" | "bottom" => {
                            bail!("property '{name}' has conflicting vertical justification")
                        }
                        "mirror" => bail!("property '{name}' repeats mirrored justification"),
                        unsupported => bail!(
                            "property '{name}' justification '{unsupported}' is not supported losslessly"
                        ),
                    }
                }
            }
            unsupported => bail!(
                "property '{name}' effects clause '{unsupported}' is not supported losslessly"
            ),
        }
    }
    let font =
        font.with_context(|| format!("property '{name}' effects are missing the font clause"))?;

    let mut font_name = String::new();
    let mut size = None;
    let mut thickness = None;
    let mut bold = None;
    let mut italic = None;
    let mut line_spacing = None;
    for clause in font.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' font contains an unsupported atom"))?;
        match tag {
            "face" => {
                if !font_name.is_empty() {
                    bail!("property '{name}' contains duplicate font face clauses");
                }
                font_name = clause
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .filter(|face| !face.is_empty())
                    .with_context(|| format!("property '{name}' font face is invalid"))?
                    .to_string();
            }
            "size" => {
                if size.is_some() {
                    bail!("property '{name}' contains duplicate font size clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 3 {
                    bail!("property '{name}' font size must contain width and height");
                }
                let width = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' font width is invalid"))?;
                let height = clause
                    .get_f64(2)
                    .with_context(|| format!("property '{name}' font height is invalid"))?;
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    bail!("property '{name}' font size must be finite and positive");
                }
                size = Some((width, height));
            }
            "thickness" => {
                if thickness.is_some() {
                    bail!("property '{name}' contains duplicate font thickness clauses");
                }
                let value = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' font thickness is invalid"))?;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !value.is_finite()
                    || value <= 0.0
                {
                    bail!("property '{name}' font thickness must be finite and positive");
                }
                thickness = Some(value);
            }
            "bold" => {
                if bold.is_some() {
                    bail!("property '{name}' contains duplicate font 'bold' clauses");
                }
                bold = Some(yes_no(clause, name, "font bold")?);
            }
            "italic" => {
                if italic.is_some() {
                    bail!("property '{name}' contains duplicate font 'italic' clauses");
                }
                italic = Some(yes_no(clause, name, "font italic")?);
            }
            "line_spacing" => {
                if line_spacing.is_some() {
                    bail!("property '{name}' contains duplicate font 'line_spacing' clauses");
                }
                let value = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' line spacing is invalid"))?;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !value.is_finite()
                    || value <= 0.0
                {
                    bail!("property '{name}' line spacing must be finite and positive");
                }
                line_spacing = Some(value);
            }
            unsupported => {
                bail!("property '{name}' font clause '{unsupported}' is not supported losslessly")
            }
        }
    }
    let (width, height) =
        size.with_context(|| format!("property '{name}' font is missing its size"))?;
    Ok(kiapi::common::types::TextAttributes {
        font_name,
        horizontal_alignment: horizontal as i32,
        vertical_alignment: vertical as i32,
        angle: None,
        line_spacing: line_spacing.unwrap_or(1.0),
        stroke_width: Some(copperpour_ipc::builders::distance(
            thickness.unwrap_or(width * 0.15),
        )),
        italic: italic.unwrap_or(false),
        bold: bold.unwrap_or(false),
        underlined: false,
        visible: true,
        mirrored,
        // As KiCad loads a footprint property.
        multiline: true,
        // The caller sets this from the property's `(unlocked …)` clause.
        keep_upright: true,
        size: Some(copperpour_ipc::builders::vec2(width, height)),
    })
}

/// A library footprint's `(point …)`, in footprint-local millimetres.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LibraryPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) size: f64,
    pub(crate) layer: String,
}

/// A library footprint's `(point (at x y) (size s) (layer "L") (uuid …))`, the only
/// clauses KiCad's `parsePCB_POINT` reads.
pub(crate) fn library_point(point: &SexpNode) -> Result<LibraryPoint> {
    let at = point
        .find("at")
        .context("footprint point is missing (at …)")?;
    Ok(LibraryPoint {
        x: at.get_f64(1).context("footprint point has an invalid X")?,
        y: at.get_f64(2).context("footprint point has an invalid Y")?,
        size: point
            .find_f64("size")
            .context("footprint point is missing (size …)")?,
        layer: point
            .find("layer")
            .and_then(|layer| layer.get(1))
            .and_then(SexpNode::as_str)
            .context("footprint point is missing (layer …)")?
            .to_string(),
    })
}

/// All `(point …)` nodes of a footprint.
pub(crate) fn points(root: &SexpNode) -> Result<Vec<LibraryPoint>> {
    root.children()
        .unwrap_or_default()
        .iter()
        .filter(|child| child.head() == Some("point"))
        .map(library_point)
        .collect()
}

/// A library point as KiCad places it: mirrored top to bottom on the back,
/// like the pads, then turned about the anchor. A point has no angle of its
/// own (`PCB_POINT::Rotate` turns only its position).
pub(crate) fn build_reference_point(
    point: &LibraryPoint,
    footprint_position: &kiapi::common::types::Vector2,
    footprint_rotation: f64,
    mirror: Option<CopperStack>,
) -> Result<kiapi::board::types::ReferencePoint> {
    let local_y = if mirror.is_some() { -point.y } else { point.y };
    let (x, y) = copperpour_sexp::geometry::transform_pad(
        point.x,
        local_y,
        copperpour_ipc::builders::nm_to_mm(footprint_position.x_nm),
        copperpour_ipc::builders::nm_to_mm(footprint_position.y_nm),
        footprint_rotation,
    );
    let layer = match mirror {
        Some(stack) => flip_layer(&point.layer, stack)?,
        None => point.layer.clone(),
    };
    Ok(kiapi::board::types::ReferencePoint {
        id: None,
        layer: copperpour_ipc::builders::try_layer_from_name(&layer)? as i32,
        position: Some(copperpour_ipc::builders::vec2(x, y)),
        size: Some(copperpour_ipc::builders::distance(point.size)),
        locked: kiapi::common::types::LockedState::LsUnlocked as i32,
        parent: None,
    })
}

/// A library footprint's net-tie pad groups as KiCad's parser reads them, in
/// the library's order, each with its pad numbers in its own order: every
/// string of `(net_tie_pad_groups "1, 2" …)`, a repeated clause adding its
/// groups after the first's, and, in a file of format 20220815 or older
/// whose keywords start with "net tie", one more group of every pad
/// (`LEGACY_NET_TIES`, `parseFOOTPRINT_unchecked`).
///
/// Placement and the refresh send each group as its pad numbers
/// (`NetTieDefinition`), and KiCad 10.0.1 and later store them joined with
/// ", " ([`carries_net_ties`]). A group that would not come back as the
/// library writes it, or that KiCad could not tie as written, is refused
/// before anything is sent:
/// - a clause holding anything but quoted strings, or nothing;
/// - a group that names no pad, or has an empty pad number between commas,
///   which KiCad's API leaves out;
/// - a backslash, which KiCad reads as an escape and its API does not;
/// - a pad number with whitespace in it, at which KiCad's API splits it;
/// - a group not written as its pad numbers joined with ", ", such as "1,2":
///   KiCad would store "1, 2", and its library check compares the text
///   (`drc_test_provider_library_parity.cpp`);
/// - a pad number the footprint has no pad for;
/// - a pad number named twice, in one group or in two.
pub(crate) fn net_ties(root: &SexpNode) -> Result<Vec<kiapi::board::types::NetTieDefinition>> {
    let pads = root
        .find_all("pad")
        .into_iter()
        .map(|pad| pad.get(1).and_then(SexpNode::as_str).unwrap_or_default())
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    for clause in root.find_all("net_tie_pad_groups") {
        let values = clause.children().unwrap_or_default();
        if values.len() < 2 {
            bail!("net_tie_pad_groups names no pad group");
        }
        for value in &values[1..] {
            match value {
                SexpNode::Str(group) => groups.push(group.clone()),
                SexpNode::Atom(atom) => bail!(
                    "net_tie_pad_groups holds the unquoted '{atom}'; KiCad writes each pad \
                     group as a quoted string"
                ),
                SexpNode::List(_) => bail!(
                    "net_tie_pad_groups holds a list; KiCad writes each pad group as a \
                     quoted string"
                ),
            }
        }
    }
    if super::solder_margins::file_version(root) <= super::library_upgrade::LEGACY_NET_TIES
        && root
            .find_str("tags")
            .is_some_and(|tags| tags.starts_with("net tie"))
    {
        let every_pad = pads.join(", ");
        if !every_pad.is_empty() {
            groups.push(every_pad);
        }
    }

    let known = pads.iter().copied().collect::<BTreeSet<&str>>();
    let mut named = BTreeMap::<String, usize>::new();
    let mut net_ties = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        if group.contains('\\') {
            bail!(
                "net-tie pad group \"{group}\" holds a backslash, which KiCad reads as an \
                 escape and its API does not"
            );
        }
        let numbers = group.split(',').map(str::trim).collect::<Vec<&str>>();
        if numbers.iter().all(|number| number.is_empty()) {
            bail!("net-tie pad group \"{group}\" names no pad");
        }
        if numbers.iter().any(|number| number.is_empty()) {
            bail!(
                "net-tie pad group \"{group}\" has an empty pad number, which KiCad's API \
                 leaves out"
            );
        }
        if let Some(number) = numbers
            .iter()
            .find(|number| number.contains(char::is_whitespace))
        {
            bail!(
                "pad number '{number}' in net-tie pad group \"{group}\" holds whitespace, at \
                 which KiCad's API splits it"
            );
        }
        let written = numbers.join(", ");
        if written != *group {
            bail!(
                "net-tie pad group \"{group}\" is not written as KiCad writes one: KiCad would \
                 keep it as \"{written}\", which its library check reports as differing"
            );
        }
        for number in &numbers {
            if !known.contains(number) {
                bail!(
                    "net-tie pad group \"{group}\" names pad '{number}', which the footprint \
                     does not have"
                );
            }
            if let Some(first) = named.insert(number.to_string(), index) {
                if first == index {
                    bail!("net-tie pad group \"{group}\" names pad '{number}' twice");
                }
                bail!(
                    "pad '{number}' is in two net-tie pad groups, \"{}\" and \"{group}\"",
                    groups[first]
                );
            }
        }
        net_ties.push(kiapi::board::types::NetTieDefinition {
            pad_number: numbers.iter().map(|number| number.to_string()).collect(),
        });
    }
    Ok(net_ties)
}

/// The `not_carried` entry for net-tie pad groups a KiCad before 10.0.1
/// cannot keep ([`carries_net_ties`]), or `None` when there are none or
/// `kicad` keeps them.
pub(crate) fn net_ties_not_carried_reason(
    net_ties: &[kiapi::board::types::NetTieDefinition],
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Option<String> {
    if net_ties.is_empty() || carries_net_ties(kicad) {
        return None;
    }
    let groups = net_ties
        .iter()
        .map(|group| format!("\"{}\"", group.pad_number.join(", ")))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "net-tie pad groups ({groups}) require KiCad 10.0.1 or newer (connected KiCad is {})",
        kicad.full_version
    ))
}

/// A library pad's zone connection and thermal relief clauses, as KiCad's
/// `parsePAD` reads them. Each is `None` when the pad doesn't set it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct PadZoneSettings {
    /// `(zone_connect N)`.
    pub(crate) connection: Option<kiapi::board::types::ZoneConnectionStyle>,
    /// `(thermal_bridge_width w)`, or the legacy `(thermal_width w)`, in mm.
    pub(crate) spoke_width: Option<f64>,
    /// `(thermal_gap g)`, in mm.
    pub(crate) gap: Option<f64>,
    /// `(thermal_bridge_angle a)`, in degrees.
    pub(crate) spoke_angle: Option<f64>,
}

impl PadZoneSettings {
    /// Read a library pad's clauses. A clause given twice keeps its last
    /// value, as in KiCad's parser.
    pub(crate) fn of_pad(pad: &SexpNode) -> Result<Self> {
        let mut settings = Self::default();
        for child in pad.children().unwrap_or_default().iter().skip(4) {
            let Some(tag) = child.head() else {
                continue;
            };
            if !matches!(
                tag,
                "zone_connect"
                    | "thermal_bridge_width"
                    | "thermal_width"
                    | "thermal_gap"
                    | "thermal_bridge_angle"
            ) {
                continue;
            }
            let text = match child.children() {
                Some([_, value]) => value.as_str(),
                _ => None,
            }
            .with_context(|| format!("pad clause '{tag}' must hold exactly one number"))?;
            if tag == "zone_connect" {
                settings.connection = Some(zone_connection(text)?);
                continue;
            }
            let value = text
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .with_context(|| format!("pad clause '{tag}' has an invalid value '{text}'"))?;
            match tag {
                "thermal_bridge_width" | "thermal_width" => settings.spoke_width = Some(value),
                "thermal_gap" => settings.gap = Some(value),
                _ => settings.spoke_angle = Some(value),
            }
        }
        Ok(settings)
    }

    /// The pad stack's zone settings for a pad of `shape`, as KiCad holds
    /// them once it has loaded the pad from a file: the pad's connection, or
    /// inherited, and always a spoke angle, the pad's or the parser's default
    /// for the shape. KiCad's `PADSTACK::Deserialize` sets the spoke angle
    /// from any `thermal_spokes` it receives, reading a missing angle as 0°,
    /// and a pad stack built without `thermal_spokes` keeps its constructor's
    /// 45°, so the angle is always sent.
    pub(crate) fn zone_connection_settings(
        &self,
        shape: i32,
        anchor: i32,
    ) -> kiapi::board::types::ZoneConnectionSettings {
        kiapi::board::types::ZoneConnectionSettings {
            zone_connection: self
                .connection
                .unwrap_or(kiapi::board::types::ZoneConnectionStyle::ZcsInherited)
                as i32,
            thermal_spokes: Some(kiapi::board::types::ThermalSpokeSettings {
                width: self.spoke_width.map(copperpour_ipc::builders::distance),
                angle: Some(kiapi::common::types::Angle {
                    value_degrees: self
                        .spoke_angle
                        .unwrap_or_else(|| default_thermal_spoke_angle(shape, anchor)),
                }),
                gap: self.gap.map(copperpour_ipc::builders::distance),
            }),
        }
    }
}

/// Each pad's zone connection and thermal relief settings, from `pad_nodes`.
pub(crate) fn pad_zones(pad_nodes: &[&SexpNode]) -> Result<Vec<PadZoneSettings>> {
    pad_nodes
        .iter()
        .copied()
        .map(PadZoneSettings::of_pad)
        .collect()
}

/// KiCad's API value for `(zone_connect N)`. KiCad's parser casts N to
/// `ZONE_CONNECTION` (`pcbnew/zones.h`), mapped as `api_pcb_enums.cpp` maps
/// it.
fn zone_connection(text: &str) -> Result<kiapi::board::types::ZoneConnectionStyle> {
    use kiapi::board::types::ZoneConnectionStyle as Style;
    Ok(match text.parse::<i64>() {
        Ok(-1) => Style::ZcsInherited,
        Ok(0) => Style::ZcsNone,
        Ok(1) => Style::ZcsThermal,
        Ok(2) => Style::ZcsFull,
        Ok(3) => Style::ZcsPthThermal,
        _ => bail!("pad zone_connect '{text}' is not a zone connection KiCad defines"),
    })
}

/// A library pad's or footprint's own solder mask and paste margins, as
/// KiCad's parser reads them. Each is `None` when the clause is absent, or
/// when it is 0 in a file KiCad reads 0 in as inherited.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SolderMargins {
    /// `(solder_mask_margin m)`, in mm.
    pub(crate) mask: Option<f64>,
    /// `(solder_paste_margin m)`, in mm.
    pub(crate) paste: Option<f64>,
    /// `(solder_paste_margin_ratio r)`, or a footprint's legacy
    /// `(solder_paste_ratio r)`.
    pub(crate) paste_ratio: Option<f64>,
}

impl SolderMargins {
    /// Read a library pad's margins from a footprint file of `file_version`.
    fn of_pad(pad: &SexpNode, file_version: u32) -> Result<Self> {
        Self::read(pad, false, file_version)
    }

    /// Read a library footprint's own margins.
    pub(crate) fn of_footprint(root: &SexpNode) -> Result<Self> {
        Self::read(root, true, super::solder_margins::file_version(root))
    }

    /// A clause given twice keeps its last value, as in KiCad's parser.
    /// `solder_margins::read` reports a margin to the library tools and takes
    /// a clause it cannot read as inherited; placement refuses that clause,
    /// as KiCad's parser does, so it reads the clauses here.
    fn read(node: &SexpNode, footprint: bool, file_version: u32) -> Result<Self> {
        use super::solder_margins::ZERO_IS_INHERITED;
        let owner = if footprint { "footprint" } else { "pad" };
        let mut margins = Self::default();
        for child in node.children().unwrap_or_default() {
            let Some(tag) = child.head() else {
                continue;
            };
            let Some(margin) = Margin::ALL.into_iter().find(|margin| {
                if footprint {
                    margin.footprint_tokens().contains(&tag)
                } else {
                    margin.name() == tag
                }
            }) else {
                continue;
            };
            let text = match child.children() {
                Some([_, value]) => value.as_str(),
                _ => None,
            }
            .with_context(|| format!("{owner} clause '{tag}' must hold exactly one number"))?;
            let value = text
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .with_context(|| format!("{owner} clause '{tag}' has an invalid value '{text}'"))?;
            // In a file up to ZERO_IS_INHERITED, KiCad reads 0 as inherited.
            let value = (value != 0.0 || file_version > ZERO_IS_INHERITED).then_some(value);
            match margin {
                Margin::Mask => margins.mask = value,
                Margin::Paste => margins.paste = value,
                Margin::PasteRatio => margins.paste_ratio = value,
            }
        }
        Ok(margins)
    }

    pub(crate) fn mask_overrides(&self) -> Option<kiapi::board::types::SolderMaskOverrides> {
        self.mask
            .map(|mask| kiapi::board::types::SolderMaskOverrides {
                solder_mask_margin: Some(copperpour_ipc::builders::distance(mask)),
                ..Default::default()
            })
    }

    pub(crate) fn paste_overrides(&self) -> Option<kiapi::board::types::SolderPasteOverrides> {
        (self.paste.is_some() || self.paste_ratio.is_some()).then(|| {
            kiapi::board::types::SolderPasteOverrides {
                solder_paste_margin: self.paste.map(copperpour_ipc::builders::distance),
                solder_paste_margin_ratio: self
                    .paste_ratio
                    .map(|value| kiapi::common::types::Ratio { value }),
            }
        })
    }
}

/// Each pad's own margins, from `pad_nodes` in the footprint `root`.
pub(crate) fn pad_margins(root: &SexpNode, pad_nodes: &[&SexpNode]) -> Result<Vec<SolderMargins>> {
    let file_version = super::solder_margins::file_version(root);
    pad_nodes
        .iter()
        .map(|pad| SolderMargins::of_pad(pad, file_version))
        .collect()
}

/// Give a placed footprint the library footprint's own margins, which KiCad
/// reads from the instance's overrides (`FOOTPRINT::Deserialize`, 10.0.8).
pub(crate) fn apply_footprint_margins(
    footprint: &mut kiapi::board::types::FootprintInstance,
    margins: &SolderMargins,
) {
    if *margins == SolderMargins::default() {
        return;
    }
    let overrides = footprint.overrides.get_or_insert_with(Default::default);
    overrides.solder_mask = margins.mask_overrides();
    overrides.solder_paste = margins.paste_overrides();
}

/// Give a built pad's `stack` a library pad's own margins. KiCad's parser
/// sets a pad's one margin on both sides (the `PAD::SetLocalSolder*`
/// setters, `pcbnew/pad.h`).
pub(crate) fn apply_pad_margins(
    stack: &mut kiapi::board::types::PadStack,
    margins: &SolderMargins,
) {
    if *margins == SolderMargins::default() {
        return;
    }
    for outer in [&mut stack.front_outer_layers, &mut stack.back_outer_layers] {
        let outer = outer.get_or_insert_with(Default::default);
        outer.solder_mask_settings = margins.mask_overrides();
        outer.solder_paste_settings = margins.paste_overrides();
    }
}

/// The spoke angle KiCad's parser gives a pad that doesn't set one
/// (`parsePAD`, `pcb_io_kicad_sexpr_parser.cpp`, 10.0.8): 90° for ovals and
/// rectangles, rounded or chamfered, and for a custom pad with a rect anchor;
/// 45° for circles and a custom pad with a circle anchor (in files from KiCad
/// 6 on). `PADSTACK::DefaultThermalSpokeAngleForShape` gives every custom pad
/// 45°, but a pad loaded from a file holds the parser's angle, and KiCad
/// reports that over IPC (`custom_pads_ipc.README.md`: 90° on the rect-anchored
/// pads, 45° on the others).
pub(crate) fn default_thermal_spoke_angle(shape: i32, anchor: i32) -> f64 {
    use kiapi::board::types::PadStackShape;
    if shape == PadStackShape::PssCustom as i32 {
        return if anchor == PadStackShape::PssCircle as i32 {
            45.0
        } else {
            90.0
        };
    }
    if [
        PadStackShape::PssOval,
        PadStackShape::PssRectangle,
        PadStackShape::PssRoundrect,
        PadStackShape::PssChamferedrect,
    ]
    .into_iter()
    .any(|candidate| candidate as i32 == shape)
    {
        90.0
    } else {
        45.0
    }
}

/// Apply library `pad_properties` (when `carry_10_0_7_fields` is true),
/// `pad_zones` and `pad_margins` to every `Pad` in `definition.items`. The
/// builder emits pads in the library's order.
pub(crate) fn apply_pad_settings(
    definition: &mut kiapi::board::types::Footprint,
    pad_properties: &[PadFabricationProperty],
    pad_zones: &[PadZoneSettings],
    pad_margins: &[SolderMargins],
    carry_10_0_7_fields: bool,
) -> Result<()> {
    let mut pairs = pad_properties.iter().zip(pad_zones).zip(pad_margins);
    for item in &mut definition.items {
        if copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad") {
            let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice())?;
            let ((property, zone), margins) = pairs
                .next()
                .context("typed footprint builder returned more pads than the library has")?;
            if carry_10_0_7_fields {
                pad.fab_property = fab_property(*property) as i32;
            }
            let stack = pad
                .pad_stack
                .as_mut()
                .context("typed footprint builder returned a pad without a pad stack")?;
            let layer = stack
                .copper_layers
                .first()
                .context("typed footprint builder returned a pad without a copper layer")?;
            let (shape, anchor) = (layer.shape, layer.custom_anchor_shape);
            stack.zone_settings = Some(zone.zone_connection_settings(shape, anchor));
            apply_pad_margins(stack, margins);
            *item = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
        }
    }
    if pairs.next().is_some() {
        bail!("typed footprint builder returned fewer pads than the library has");
    }
    Ok(())
}

/// Append `ReferencePoint` items for each library `(point …)` to
/// `definition.items`.
pub(crate) fn apply_reference_points(
    definition: &mut kiapi::board::types::Footprint,
    points: &[LibraryPoint],
    footprint_position: &kiapi::common::types::Vector2,
    footprint_rotation: f64,
    mirror: Option<CopperStack>,
) -> Result<()> {
    for point in points {
        definition.items.push(copperpour_ipc::builders::pack_any(
            &build_reference_point(point, footprint_position, footprint_rotation, mirror)?,
            "kiapi.board.types.ReferencePoint",
        ));
    }
    Ok(())
}

/// Whether a footprint has pad fabrication properties or reference points
/// that KiCad's API carries only from 10.0.7.
pub(crate) fn needs_kicad_10_0_7(
    pad_properties: &[PadFabricationProperty],
    points: &[LibraryPoint],
) -> bool {
    pad_properties
        .iter()
        .any(|property| *property != PadFabricationProperty::None)
        || !points.is_empty()
}

/// Describe what a footprint has that KiCad before 10.0.7 cannot carry, or
/// `None` when it has neither.
pub(crate) fn needs_kicad_10_0_7_summary(
    pad_properties: &[PadFabricationProperty],
    points: &[LibraryPoint],
) -> Option<&'static str> {
    let has_props = pad_properties
        .iter()
        .any(|property| *property != PadFabricationProperty::None);
    match (has_props, !points.is_empty()) {
        (false, false) => None,
        (true, false) => Some("pad fabrication properties"),
        (false, true) => Some("a reference point"),
        (true, true) => Some("pad fabrication properties and a reference point"),
    }
}

/// Pad fabrication properties for a message, one entry per pad number and
/// property, bounded to avoid flooding diagnostics on large BGAs.
pub(crate) fn listed_pad_properties<'a>(
    pads: impl IntoIterator<Item = (&'a str, PadFabricationProperty)>,
) -> String {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for (number, property) in pads {
        *counts.entry((number, property.as_str())).or_default() += 1;
    }
    let formatted = counts
        .into_iter()
        .map(|((number, property), count)| {
            if count == 1 {
                format!("pad {number}: {property}")
            } else {
                format!("pad {number}: {property} ({count} pads)")
            }
        })
        .collect::<Vec<_>>();
    const LIMIT: usize = 6;
    if formatted.len() <= LIMIT {
        formatted.join(", ")
    } else {
        format!(
            "{} and {} more",
            formatted[..LIMIT].join(", "),
            formatted.len() - LIMIT
        )
    }
}

/// Human-readable `not_carried` entries for pad fabrication properties and
/// reference points when connected to a KiCad older than 10.0.7.
pub(crate) fn not_carried_10_0_7_reasons(
    pad_properties: &[PadFabricationProperty],
    pads: &[copperpour_ipc::IpcPadDefinition],
    points: &[LibraryPoint],
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let fab_pads = listed_pad_properties(
        pads.iter()
            .zip(pad_properties)
            .filter(|(_, property)| **property != PadFabricationProperty::None)
            .map(|(pad, property)| (pad.number.as_str(), *property)),
    );
    if !fab_pads.is_empty() {
        reasons.push(format!(
            "pad fabrication properties ({fab_pads}) require KiCad 10.0.7 or newer \
             (connected KiCad is {})",
            kicad.full_version
        ));
    }
    if !points.is_empty() {
        let count = points.len();
        let noun = if count == 1 {
            "reference point"
        } else {
            "reference points"
        };
        reasons.push(format!(
            "{count} {noun} require KiCad 10.0.7 or newer (connected KiCad is {})",
            kicad.full_version
        ));
    }
    reasons
}

/// Refuses any library footprint child or clause that typed library refresh
/// cannot reproduce losslessly.
pub(crate) fn validate_supported_children(root: &SexpNode) -> Result<()> {
    check_supported_children(root, &mut |_placed, refresh| bail!("{refresh}"))
}

/// Lists every library footprint child or clause that typed live placement
/// leaves out, deduplicated in order of appearance.
pub(crate) fn unsupported_features(root: &SexpNode) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    check_supported_children(root, &mut |placed, _refresh| {
        if seen.insert(placed.to_string()) {
            out.push(placed.to_string());
        }
        Ok(())
    })?;
    Ok(out)
}

fn check_supported_children(
    root: &SexpNode,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    for child in root.children().unwrap_or_default().iter().skip(2) {
        let Some(tag) = child.head() else {
            continue;
        };
        match tag {
            "version" | "generator" | "generator_version" | "layer" | "descr" | "tags" | "attr"
            | "property" | "fp_text" | "fp_line" | "fp_rect" | "fp_circle" | "fp_arc"
            | "fp_poly" | "pad" | "model" | "point" => {}
            // Placement and the refresh carry the groups; `net_ties` reads
            // them and refuses what they cannot carry (#85).
            "net_tie_pad_groups" => {}
            // KiCad 10 writes these two flags into every footprint it saves
            // (all 15,428 official library files carry them, every one at its
            // default). At the default the typed rebuild loses nothing;
            // a non-default value carries semantics it cannot represent.
            "embedded_fonts" | "duplicate_pad_numbers_are_jumpers" => {
                let value = child.get(1).and_then(SexpNode::as_str);
                if value != Some("no") {
                    let val = value.unwrap_or("");
                    on_unsupported(
                        &format!("footprint '{tag} {val}' is not carried by typed placement"),
                        &format!(
                            "footprint '{tag} {val}' is not supported by typed library refresh"
                        ),
                    )?;
                }
            }
            // The footprint's own margins, which placement and the refresh
            // carry.
            tag if Margin::ALL
                .iter()
                .any(|margin| margin.footprint_tokens().contains(&tag)) => {}
            unsupported => {
                on_unsupported(
                    &format!("footprint child '{unsupported}' is not carried by typed placement"),
                    &format!(
                        "footprint child '{unsupported}' is not supported by typed library refresh"
                    ),
                )?;
            }
        }
        match tag {
            "pad" => check_pad(child, on_unsupported)?,
            "point" => check_point(child, on_unsupported)?,
            "fp_line" | "fp_rect" | "fp_circle" | "fp_arc" | "fp_poly" => {
                check_graphic(child, tag, on_unsupported)?
            }
            "fp_text" => {
                let kind = child
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .context("fp_text is missing its kind")?;
                match kind {
                    "reference" | "value" => {}
                    "user" => check_user_text(child, on_unsupported)?,
                    _ => {
                        on_unsupported(
                            &format!("fp_text kind '{kind}' is not carried by typed placement"),
                            &format!(
                                "fp_text kind '{kind}' is not supported losslessly by typed library refresh"
                            ),
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_point(
    point: &SexpNode,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    for child in point.children().unwrap_or_default().iter().skip(1) {
        let Some(tag) = child.head() else {
            bail!("footprint point has an unexpected atom");
        };
        if !matches!(tag, "at" | "size" | "layer" | "uuid") {
            on_unsupported(
                &format!("footprint point clause '{tag}' is not carried by typed placement"),
                &format!(
                    "footprint point clause '{tag}' is not supported by typed library refresh"
                ),
            )?;
        }
    }
    Ok(())
}

fn check_user_text(
    text: &SexpNode,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    text.get(2)
        .and_then(SexpNode::as_str)
        .context("fp_text user is missing its text")?;

    let mut saw_at = false;
    let mut saw_layer = false;
    let mut saw_effects = false;
    let mut saw_unlocked = false;
    let mut identifier = None;
    for clause in text.children().unwrap_or_default().iter().skip(3) {
        let tag = clause
            .head()
            .context("fp_text user contains an unsupported atom")?;
        match tag {
            "unlocked" => {
                if saw_unlocked {
                    bail!("fp_text user contains duplicate 'unlocked' clauses");
                }
                saw_unlocked = true;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !matches!(clause.get(1).and_then(SexpNode::as_str), Some("yes" | "no"))
                {
                    bail!("fp_text user 'unlocked' must be 'yes' or 'no'");
                }
            }
            "at" => {
                if saw_at {
                    bail!("fp_text user contains duplicate 'at' clauses");
                }
                saw_at = true;
                let count = clause.children().map_or(0, |children| children.len());
                if !matches!(count, 3 | 4) {
                    bail!("fp_text user 'at' must contain x, y, and optional rotation");
                }
                for index in 1..count {
                    let value = clause
                        .get_f64(index)
                        .with_context(|| format!("fp_text user 'at' value {index} is invalid"))?;
                    if !value.is_finite() {
                        bail!("fp_text user 'at' values must be finite");
                    }
                }
            }
            "layer" => {
                if saw_layer {
                    bail!("fp_text user contains duplicate 'layer' clauses");
                }
                saw_layer = true;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("fp_text user 'layer' must name exactly one layer");
                }
            }
            "uuid" | "tstamp" => {
                if let Some(previous) = identifier {
                    bail!(
                        "fp_text user contains multiple identifier clauses ('{previous}' and '{tag}')"
                    );
                }
                identifier = Some(tag);
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("fp_text user '{tag}' must contain exactly one identifier");
                }
            }
            "effects" => {
                if saw_effects {
                    bail!("fp_text user contains duplicate 'effects' clauses");
                }
                saw_effects = true;
                check_user_text_effects(clause, on_unsupported)?;
            }
            unsupported => {
                on_unsupported(
                    &format!(
                        "fp_text user clause '{unsupported}' is not carried by typed placement"
                    ),
                    &format!("fp_text user clause '{unsupported}' is not supported losslessly"),
                )?;
            }
        }
    }

    if !saw_at {
        bail!("fp_text user is missing its 'at' clause");
    }
    if !saw_layer {
        bail!("fp_text user is missing its 'layer' clause");
    }
    if !saw_effects {
        bail!("fp_text user is missing its 'effects' clause");
    }
    Ok(())
}

fn check_user_text_effects(
    effects: &SexpNode,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let mut font = None;
    let mut justify = None;
    for clause in effects.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .context("fp_text user effects contain an unsupported atom")?;
        match tag {
            "font" => {
                if font.replace(clause).is_some() {
                    bail!("fp_text user contains duplicate font clauses");
                }
            }
            // KiCad's API carries both alignments and the mirror flag.
            "justify" => {
                if justify.replace(clause).is_some() {
                    bail!("fp_text user contains duplicate 'justify' clauses");
                }
                check_user_text_justify(clause)?;
            }
            _ => {
                on_unsupported(
                    &format!(
                        "fp_text user effects clause '{tag}' is not carried by typed placement"
                    ),
                    &format!("fp_text user effects clause '{tag}' is not supported losslessly"),
                )?;
            }
        }
    }
    let font = font.context("fp_text user effects are missing the font clause")?;

    let mut size = None;
    let mut thickness = None;
    let mut bold = false;
    let mut italic = false;
    let mut line_spacing = false;
    for clause in font.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .context("fp_text user font contains an unsupported atom")?;
        match tag {
            "size" => {
                if size.is_some() {
                    bail!("fp_text user contains duplicate font size clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 3 {
                    bail!("fp_text user font size must contain height and width");
                }
                // KiCad writes `(size height width)`; KiCad's API carries
                // both, so a non-square size needs nothing more.
                let height = clause
                    .get_f64(1)
                    .context("fp_text user font height is invalid")?;
                let width = clause
                    .get_f64(2)
                    .context("fp_text user font width is invalid")?;
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    bail!("fp_text user font size must be finite and positive");
                }
                size = Some(height);
            }
            "bold" | "italic" => {
                let seen = if tag == "bold" {
                    &mut bold
                } else {
                    &mut italic
                };
                if std::mem::replace(seen, true) {
                    bail!("fp_text user contains duplicate font '{tag}' clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !matches!(clause.get(1).and_then(SexpNode::as_str), Some("yes" | "no"))
                {
                    bail!("fp_text user font '{tag}' must be 'yes' or 'no'");
                }
            }
            "line_spacing" => {
                if std::mem::replace(&mut line_spacing, true) {
                    bail!("fp_text user contains duplicate font 'line_spacing' clauses");
                }
                let value = clause
                    .get_f64(1)
                    .context("fp_text user font line spacing is invalid")?;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !value.is_finite()
                    || value <= 0.0
                {
                    bail!("fp_text user font line spacing must be finite and positive");
                }
            }
            "thickness" => {
                if thickness.is_some() {
                    bail!("fp_text user contains duplicate font thickness clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 2 {
                    bail!("fp_text user font thickness must contain exactly one value");
                }
                let value = clause
                    .get_f64(1)
                    .context("fp_text user font thickness is invalid")?;
                if !value.is_finite() || value <= 0.0 {
                    bail!("fp_text user font thickness must be finite and positive");
                }
                thickness = Some(value);
            }
            unsupported => {
                on_unsupported(
                    &format!(
                        "fp_text user font clause '{unsupported}' is not carried by typed placement"
                    ),
                    &format!(
                        "fp_text user font clause '{unsupported}' is not supported losslessly"
                    ),
                )?;
            }
        }
    }
    size.context("fp_text user font is missing its size")?;
    thickness.context("fp_text user font is missing its thickness")?;
    Ok(())
}

/// `(justify …)` names at most one horizontal and one vertical side and
/// `mirror` at most once, as KiCad writes it.
fn check_user_text_justify(justify: &SexpNode) -> Result<()> {
    let mut horizontal = false;
    let mut vertical = false;
    let mut mirror = false;
    for value in justify.children().unwrap_or_default().iter().skip(1) {
        let value = value
            .as_str()
            .context("fp_text user justify contains a non-atom")?;
        let seen = match value {
            "left" | "right" => &mut horizontal,
            "top" | "bottom" => &mut vertical,
            "mirror" => &mut mirror,
            unsupported => {
                bail!("fp_text user justification '{unsupported}' is not supported losslessly")
            }
        };
        if std::mem::replace(seen, true) {
            bail!("fp_text user justification repeats or conflicts at '{value}'");
        }
    }
    Ok(())
}

fn check_graphic(
    graphic: &SexpNode,
    kind: &str,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let allowed: &[&str] = match kind {
        "fp_line" => &["start", "end", "stroke", "layer", "uuid", "tstamp"],
        "fp_rect" => &["start", "end", "stroke", "fill", "layer", "uuid", "tstamp"],
        "fp_circle" => &["center", "end", "stroke", "fill", "layer", "uuid", "tstamp"],
        "fp_arc" => &["start", "mid", "end", "stroke", "layer", "uuid", "tstamp"],
        "fp_poly" => &["pts", "stroke", "fill", "layer", "uuid", "tstamp"],
        _ => bail!("unsupported footprint graphic '{kind}'"),
    };
    for child in graphic.children().unwrap_or_default().iter().skip(1) {
        let Some(tag) = child.head() else {
            bail!("{kind} contains an unsupported atom");
        };
        if !allowed.contains(&tag) {
            on_unsupported(
                &format!("{kind} clause '{tag}' is not carried by typed placement"),
                &format!("{kind} clause '{tag}' is not supported by typed library refresh"),
            )?;
            continue;
        }
        match tag {
            "stroke" => {
                for stroke_child in child.children().unwrap_or_default().iter().skip(1) {
                    let stroke_tag = stroke_child
                        .head()
                        .context("graphic stroke contains an unsupported atom")?;
                    match stroke_tag {
                        "width" => {}
                        "type" => {
                            let stroke_type = stroke_child
                                .get(1)
                                .and_then(SexpNode::as_str)
                                .context("graphic stroke type is missing")?;
                            if !matches!(stroke_type, "solid" | "default") {
                                on_unsupported(
                                    &format!(
                                        "graphic stroke type '{stroke_type}' is not carried by typed placement"
                                    ),
                                    &format!(
                                        "graphic stroke type '{stroke_type}' is not supported losslessly"
                                    ),
                                )?;
                            }
                        }
                        unsupported => {
                            on_unsupported(
                                &format!(
                                    "graphic stroke clause '{unsupported}' is not carried by typed placement"
                                ),
                                &format!(
                                    "graphic stroke clause '{unsupported}' is not supported losslessly"
                                ),
                            )?;
                        }
                    }
                }
            }
            "fill" => {
                let fill = child
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .context("graphic fill is missing")?;
                if !matches!(fill, "none" | "no" | "solid" | "yes") {
                    on_unsupported(
                        &format!("graphic fill '{fill}' is not carried by typed placement"),
                        &format!("graphic fill '{fill}' is not supported losslessly"),
                    )?;
                }
            }
            "pts"
                if child
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .skip(1)
                    .any(|point| point.head() != Some("xy")) =>
            {
                bail!("fp_poly contains a non-xy point");
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_pad(
    pad: &SexpNode,
    on_unsupported: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let shape = pad
        .get(3)
        .and_then(SexpNode::as_str)
        .context("footprint pad is missing its shape")?;
    // Placement, Update PCB and the refresh build and compare custom pads
    // through the shared model (#679); what it cannot carry is refused where
    // the pads are read (`extract_pad_definitions`).
    let custom = shape == "custom";
    if !custom && !matches!(shape, "circle" | "rect" | "oval" | "roundrect") {
        on_unsupported(
            &format!("pad shape '{shape}' is not carried by typed placement"),
            &format!("pad shape '{shape}' is not supported by typed library refresh"),
        )?;
    }
    for child in pad.children().unwrap_or_default().iter().skip(4) {
        let Some(tag) = child.head() else {
            continue;
        };
        // A custom pad's shape, built from the shared model.
        if custom && matches!(tag, "options" | "primitives") {
            continue;
        }
        // A pad's own margins, which placement and the refresh carry.
        if Margin::ALL.iter().any(|margin| margin.name() == tag) {
            continue;
        }
        if !matches!(
            tag,
            "at" | "size"
                | "layers"
                | "drill"
                | "roundrect_rratio"
                | "uuid"
                | "tstamp"
                | "remove_unused_layers"
                | "property"
                | "zone_connect"
                | "thermal_bridge_width"
                | "thermal_width"
                | "thermal_gap"
                | "thermal_bridge_angle"
        ) {
            on_unsupported(
                &format!("pad clause '{tag}' is not carried by typed placement"),
                &format!("pad clause '{tag}' is not supported by typed library refresh"),
            )?;
            continue;
        }
        if tag == "remove_unused_layers" {
            let value = child.get(1).and_then(SexpNode::as_str);
            if value != Some("no") {
                on_unsupported(
                    "pad 'remove_unused_layers yes' is not carried by typed placement",
                    "pad 'remove_unused_layers yes' is not supported by typed library refresh",
                )?;
            }
        }
        if tag == "drill" {
            for nested in child.children().unwrap_or_default().iter().skip(1) {
                if nested.head().is_some() {
                    on_unsupported(
                        "nested drill clauses are not carried by typed placement",
                        "nested drill clauses are not supported by typed library refresh",
                    )?;
                    break;
                }
            }
        }
    }
    Ok(())
}

/// What the placement tests read back from a built footprint's margins, and
/// #667's four footprints with what each carries.
#[cfg(test)]
pub(crate) mod test_margins {
    use super::kiapi;
    use super::kiapi::board::types::{FootprintInstance, Pad};
    use prost::Message;

    /// A footprint's own, or a pad side's, mask margin and paste margin in
    /// nm, and its paste ratio.
    pub(crate) type Margins = (Option<i64>, Option<i64>, Option<f64>);
    pub(crate) const NO_MARGINS: Margins = (None, None, None);

    /// #667's four KiCad-written footprints (`solder_margins_kicad10.README.md`):
    /// each one's name, text, own margins and pad 9's.
    pub(crate) const SOLDER_MARGIN_FOOTPRINTS: [(&str, &str, Margins, Margins); 4] = [
        (
            "ep",
            include_str!("../../tests/fixtures/solder_margins_ep_kicad10.kicad_mod"),
            NO_MARGINS,
            (Some(-50_000), Some(-100_000), Some(-0.2)),
        ),
        (
            "ep_zero",
            include_str!("../../tests/fixtures/solder_margins_ep_zero_kicad10.kicad_mod"),
            NO_MARGINS,
            (Some(0), Some(0), Some(0.0)),
        ),
        (
            "footprint",
            include_str!("../../tests/fixtures/solder_margins_footprint_kicad10.kicad_mod"),
            (Some(50_000), Some(-30_000), Some(-0.1)),
            NO_MARGINS,
        ),
        (
            "footprint_zero",
            include_str!("../../tests/fixtures/solder_margins_footprint_zero_kicad10.kicad_mod"),
            (Some(0), Some(0), Some(0.0)),
            NO_MARGINS,
        ),
    ];

    fn margins(
        mask: Option<&kiapi::board::types::SolderMaskOverrides>,
        paste: Option<&kiapi::board::types::SolderPasteOverrides>,
    ) -> Margins {
        (
            mask.and_then(|overrides| overrides.solder_mask_margin.as_ref())
                .map(|margin| margin.value_nm),
            paste
                .and_then(|overrides| overrides.solder_paste_margin.as_ref())
                .map(|margin| margin.value_nm),
            paste
                .and_then(|overrides| overrides.solder_paste_margin_ratio.as_ref())
                .map(|ratio| ratio.value),
        )
    }

    /// A built footprint's own margins, and each pad's that has any, its two
    /// outer layers required to agree.
    pub(crate) fn placed_margins(
        footprint: &FootprintInstance,
    ) -> (Margins, Vec<(String, Margins)>) {
        let side = |outer: Option<&kiapi::board::types::PadStackOuterLayer>| {
            margins(
                outer.and_then(|outer| outer.solder_mask_settings.as_ref()),
                outer.and_then(|outer| outer.solder_paste_settings.as_ref()),
            )
        };
        let overrides = footprint.overrides.unwrap_or_default();
        let pads = footprint
            .definition
            .as_ref()
            .expect("a built footprint has a definition")
            .items
            .iter()
            .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
            .map(|item| {
                let pad = Pad::decode(item.value.as_slice()).unwrap();
                let stack = pad.pad_stack.unwrap();
                let front = side(stack.front_outer_layers.as_ref());
                assert_eq!(
                    front,
                    side(stack.back_outer_layers.as_ref()),
                    "pad '{}'",
                    pad.number
                );
                (pad.number, front)
            })
            .filter(|(_, margins)| *margins != NO_MARGINS)
            .collect();
        (
            margins(
                overrides.solder_mask.as_ref(),
                overrides.solder_paste.as_ref(),
            ),
            pads,
        )
    }

    /// What `placed_margins` gives for a footprint with `own` margins and
    /// `pad_9`'s.
    pub(crate) fn expected_margins(
        own: Margins,
        pad_9: Margins,
    ) -> (Margins, Vec<(String, Margins)>) {
        let pads = if pad_9 == NO_MARGINS {
            vec![]
        } else {
            vec![("9".to_string(), pad_9)]
        };
        (own, pads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiapi::board::types::FootprintMountingStyle;

    const C_0603: &str = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");

    fn root(source: &str) -> SexpNode {
        copperpour_sexp::parse_sexp(source).unwrap()
    }

    /// Custom pads are carried by live placement and the typed refresh
    /// (#679): a footprint with them lists none of its shape, options or
    /// primitives as not carried, and the refresh accepts it.
    #[test]
    fn custom_pads_are_carried_by_placement_and_the_refresh() {
        let vqfn = root(include_str!(
            "../../tests/fixtures/pad_primitives_vqfn20_kicad10.kicad_mod"
        ));
        assert_eq!(unsupported_features(&vqfn).unwrap(), Vec::<String>::new());
        validate_supported_children(&vqfn).unwrap();
    }

    /// Solder margins are carried by live placement and the typed refresh, so
    /// a footprint with its own or its pad's lists none as not carried, and
    /// the refresh refuses neither.
    #[test]
    fn solder_margins_are_carried_by_placement_and_the_refresh() {
        for source in [
            include_str!("../../tests/fixtures/solder_margins_ep_kicad10.kicad_mod"),
            include_str!("../../tests/fixtures/solder_margins_footprint_kicad10.kicad_mod"),
        ] {
            let margins = root(source);
            assert_eq!(
                unsupported_features(&margins).unwrap(),
                Vec::<String>::new()
            );
            let mut refusals = Vec::new();
            check_supported_children(&margins, &mut |_placed, refresh| {
                refusals.push(refresh.to_string());
                Ok(())
            })
            .unwrap();
            assert_eq!(refusals, Vec::<String>::new());
            validate_supported_children(&margins).unwrap();
        }
    }

    /// Inline because KiCad writes no such clause: a margin clause that is not
    /// one finite number refuses the footprint, as a malformed `zone_connect`
    /// does.
    #[test]
    fn a_malformed_solder_margin_is_refused() {
        for clause in [
            "(solder_mask_margin)",
            "(solder_mask_margin 0.1 0.2)",
            "(solder_paste_margin x)",
            "(solder_paste_margin_ratio inf)",
        ] {
            let pad = root(&format!("(pad \"1\" smd rect {clause})"));
            assert!(SolderMargins::of_pad(&pad, 20260206).is_err(), "{clause}");
            let footprint = root(&format!("(footprint \"F\" (version 20260206) {clause})"));
            assert!(SolderMargins::of_footprint(&footprint).is_err(), "{clause}");
        }
        let legacy = root("(footprint \"F\" (version 20260206) (solder_paste_ratio x))");
        assert!(SolderMargins::of_footprint(&legacy).is_err());
    }

    /// Each margin travels alone: one with only a mask margin, a paste margin
    /// or a paste ratio sends that one and leaves the others unset.
    #[test]
    fn each_solder_margin_travels_alone() {
        for (margins, mask, paste, ratio) in [
            (
                SolderMargins {
                    mask: Some(0.1),
                    ..Default::default()
                },
                Some(100_000),
                None,
                None,
            ),
            (
                SolderMargins {
                    paste: Some(-0.05),
                    ..Default::default()
                },
                None,
                Some(-50_000),
                None,
            ),
            (
                SolderMargins {
                    paste_ratio: Some(-0.2),
                    ..Default::default()
                },
                None,
                None,
                Some(-0.2),
            ),
        ] {
            let mut footprint = kiapi::board::types::FootprintInstance::default();
            apply_footprint_margins(&mut footprint, &margins);
            let overrides = footprint.overrides.unwrap();
            assert_eq!(
                overrides.solder_mask.is_some(),
                mask.is_some(),
                "{margins:?}"
            );
            assert_eq!(
                overrides.solder_paste.is_some(),
                paste.is_some() || ratio.is_some(),
                "{margins:?}"
            );
            assert_eq!(
                overrides
                    .solder_mask
                    .and_then(|settings| settings.solder_mask_margin)
                    .map(|margin| margin.value_nm),
                mask,
                "{margins:?}"
            );
            assert_eq!(
                overrides
                    .solder_paste
                    .and_then(|settings| settings.solder_paste_margin)
                    .map(|margin| margin.value_nm),
                paste,
                "{margins:?}"
            );
            assert_eq!(
                overrides
                    .solder_paste
                    .and_then(|settings| settings.solder_paste_margin_ratio)
                    .map(|fraction| fraction.value),
                ratio,
                "{margins:?}"
            );
        }
    }

    /// Inline because KiCad writes each clause once: a clause given twice
    /// keeps its last value, as KiCad's parser does, and a footprint's older
    /// `solder_paste_ratio` and `solder_paste_margin_ratio` set one ratio, the
    /// later winning. Placement carries the older token like the other; a
    /// pad knows only the current one, so on a pad the older is no margin.
    #[test]
    fn the_last_of_a_repeated_solder_margin_wins() {
        let pad = root("(pad \"1\" smd rect (solder_mask_margin 0.1) (solder_mask_margin 0.2))");
        assert_eq!(
            SolderMargins::of_pad(&pad, 20260206).unwrap().mask,
            Some(0.2)
        );
        let pad = root("(pad \"1\" smd rect (solder_paste_ratio x))");
        assert_eq!(
            SolderMargins::of_pad(&pad, 20260206).unwrap(),
            SolderMargins::default()
        );
        for (clauses, ratio) in [
            (
                "(solder_paste_ratio -0.1) (solder_paste_margin_ratio -0.3)",
                -0.3,
            ),
            (
                "(solder_paste_margin_ratio -0.3) (solder_paste_ratio -0.1)",
                -0.1,
            ),
        ] {
            let footprint = root(&format!("(footprint \"F\" (version 20260206) {clauses})"));
            assert_eq!(
                SolderMargins::of_footprint(&footprint).unwrap().paste_ratio,
                Some(ratio),
                "{clauses}"
            );
            assert_eq!(
                unsupported_features(&footprint).unwrap(),
                Vec::<String>::new(),
                "{clauses}"
            );
        }
    }

    /// Inline because KiCad 10 writes neither: a margin of 0 in a file up to
    /// format 20240201 is inherited, as KiCad's parser reads it, and a
    /// footprint's legacy `solder_paste_ratio` is its paste ratio.
    #[test]
    fn solder_margins_read_as_kicad_reads_older_files() {
        let footprint = |version: u32| {
            root(&format!(
                "(footprint \"F\" (version {version}) (solder_mask_margin 0) \
                 (solder_paste_ratio -0.1) (pad \"1\" smd rect (solder_paste_margin 0)))"
            ))
        };
        let older = footprint(20240201);
        assert_eq!(
            SolderMargins::of_footprint(&older).unwrap(),
            SolderMargins {
                paste_ratio: Some(-0.1),
                ..Default::default()
            }
        );
        assert_eq!(
            pad_margins(&older, &older.find_all("pad")).unwrap(),
            [SolderMargins::default()]
        );
        let newer = footprint(20240202);
        assert_eq!(SolderMargins::of_footprint(&newer).unwrap().mask, Some(0.0));
        assert_eq!(
            pad_margins(&newer, &newer.find_all("pad")).unwrap()[0].paste,
            Some(0.0)
        );
    }

    /// The KiCad-written 0603 capacitor: `(attr smd)`, its description and
    /// tags, and one visible model with identity transforms.
    #[test]
    fn a_kicad_library_footprint_reads_whole() {
        let root = root(C_0603);

        let attributes = attributes(&root).unwrap();
        assert_eq!(
            attributes.mounting_style,
            FootprintMountingStyle::FmsSmd as i32
        );
        assert!(!attributes.exclude_from_position_files);
        assert!(!attributes.do_not_populate);

        let text = description_and_keywords(&root);
        assert!(text
            .description
            .starts_with("Capacitor SMD 0603 (1608 Metric)"));
        assert_eq!(text.keywords, "capacitor");

        let models = models(&root).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].filename,
            "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"
        );
        assert!(models[0].visible);
        assert_eq!(models[0].opacity, 1.0);
        let scale = models[0].scale.as_ref().unwrap();
        assert_eq!((scale.x_nm, scale.y_nm, scale.z_nm), (1.0, 1.0, 1.0));
    }

    /// Where KiCad's own footprint libraries are installed, if they are.
    fn installed_footprint_dir() -> Option<std::path::PathBuf> {
        ["KICAD10_FOOTPRINT_DIR", "KICAD_FOOTPRINT_DIR"]
            .iter()
            .filter_map(std::env::var_os)
            .map(std::path::PathBuf::from)
            .chain(
                [
                    r"C:\KiCad\10.0\share\kicad\footprints",
                    r"C:\Program Files\KiCad\10.0\share\kicad\footprints",
                    "/usr/share/kicad/footprints",
                    "/usr/local/share/kicad/footprints",
                    "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints",
                ]
                .iter()
                .map(std::path::PathBuf::from),
            )
            .find(|dir| dir.is_dir())
    }

    /// KiCad 10 hides a model with `(hide yes)`. Read from the installed
    /// library rather than committed, because the library is CC BY-SA (the
    /// same choice as #738's demo sheet). Skips without an installed KiCad.
    #[test]
    fn a_model_hidden_the_kicad_10_way_reads_as_hidden() {
        let Some(dir) = installed_footprint_dir() else {
            eprintln!("SKIP: no installed KiCad footprint library");
            return;
        };
        let path = dir.join("Package_SO.pretty/Texas_S-PDSO-G8_3x3mm_P0.65mm.kicad_mod");
        let Ok(source) = std::fs::read_to_string(&path) else {
            eprintln!("SKIP: {} is not installed", path.display());
            return;
        };
        let models = models(&root(&source)).unwrap();
        assert_eq!(models.len(), 1);
        assert!(
            !models[0].visible,
            "{} is hidden in the library",
            models[0].filename
        );
    }

    /// Opacity defaults only when the library leaves it out. A value that is
    /// there and readable is kept; one that is there and unreadable refuses the
    /// model, where it used to become full opacity without a word.
    #[test]
    fn opacity_defaults_only_when_absent() {
        let with = |opacity: &str| {
            root(&format!(
                "(footprint \"X\" (model \"m.step\" (offset (xyz 0 0 0)) {opacity}))"
            ))
        };
        assert_eq!(models(&with("")).unwrap()[0].opacity, 1.0);
        assert_eq!(models(&with("(opacity 0.4)")).unwrap()[0].opacity, 0.4);
        let refusal = models(&with("(opacity dim)")).unwrap_err();
        assert!(
            format!("{refusal:#}").contains("3D model opacity is not a number"),
            "{refusal:#}"
        );
        assert!(models(&with("(opacity)")).is_err());
    }

    /// The KiCad-written 0603's properties: Reference and Value with the
    /// library's own layer, visibility and font, and its one custom property.
    /// None carries `(unlocked …)`, so each is kept upright, as KiCad reads
    /// it, and each is sent unlocked, as KiCad creates a field.
    #[test]
    fn a_kicad_library_footprint_reads_its_properties_whole() {
        use kiapi::board::types::BoardLayer;
        use kiapi::common::types::LockedState;

        let properties = properties(&root(C_0603)).unwrap();

        let layer_visible_locked = |field: &kiapi::board::types::Field| {
            let text = field.text.as_ref().unwrap();
            let upright = text
                .text
                .as_ref()
                .and_then(|text| text.attributes.as_ref())
                .unwrap()
                .keep_upright;
            assert!(upright, "{} is kept upright", field.name);
            (text.layer, field.visible, text.locked)
        };
        let reference = properties.reference.as_ref().expect("a typed Reference");
        assert_eq!(
            layer_visible_locked(reference),
            (
                BoardLayer::BlFSilkS as i32,
                true,
                LockedState::LsUnlocked as i32
            )
        );
        let value = properties.value.as_ref().expect("a typed Value");
        assert_eq!(
            layer_visible_locked(value),
            (
                BoardLayer::BlFFab as i32,
                true,
                LockedState::LsUnlocked as i32
            )
        );
        let font = value.text.as_ref().unwrap().text.as_ref().unwrap();
        let attributes = font.attributes.as_ref().unwrap();
        assert_eq!(attributes.stroke_width.as_ref().unwrap().value_nm, 150_000);
        assert_eq!(attributes.line_spacing, 1.0);

        assert_eq!(properties.custom.len(), 1);
        let generator = &properties.custom[0];
        assert_eq!(generator.name, "KiLib_Generator");
        assert_eq!(
            generator.text.as_ref().unwrap().text.as_ref().unwrap().text,
            "SMD_2terminal_chip_molded"
        );
        assert_eq!(
            layer_visible_locked(generator),
            (
                BoardLayer::BlFSilkS as i32,
                false,
                LockedState::LsUnlocked as i32
            )
        );
        assert!(properties.datasheet.is_none() && properties.description.is_none());
    }

    /// `(unlocked yes)` is KiCad's keep-upright clause: a field carrying it is
    /// not kept upright, and one without it, or with `(unlocked no)`, is. It
    /// never locks the field. A `(locked …)` clause, which no KiCad library
    /// file writes on a property, is refused rather than dropped, and so is
    /// any value but yes or no. @davidtdab found the clause read as a lock in
    /// #820's first version.
    #[test]
    fn unlocked_is_read_as_keep_upright_not_as_a_lock() {
        use kiapi::common::types::LockedState;

        let with = |clause: &str| {
            C_0603.replace(
                "(property \"KiLib_Generator\" \"SMD_2terminal_chip_molded\"",
                &format!("(property \"KiLib_Generator\" \"SMD_2terminal_chip_molded\" {clause}"),
            )
        };
        let upright_and_lock = |source: &str| {
            let properties = properties(&root(source))?;
            let text = properties.custom[0].text.clone().unwrap();
            let upright = text.text.unwrap().attributes.unwrap().keep_upright;
            Ok::<_, anyhow::Error>((upright, text.locked))
        };
        let unlocked = LockedState::LsUnlocked as i32;
        assert_ne!(with("(unlocked yes)"), C_0603);
        assert_eq!(
            upright_and_lock(&with("(unlocked yes)")).unwrap(),
            (false, unlocked)
        );
        assert_eq!(
            upright_and_lock(&with("(unlocked no)")).unwrap(),
            (true, unlocked)
        );
        assert_eq!(upright_and_lock(C_0603).unwrap(), (true, unlocked));
        let refusal = upright_and_lock(&with("(unlocked maybe)")).unwrap_err();
        assert!(format!("{refusal:#}").contains("unlocked"), "{refusal:#}");
        let refusal = upright_and_lock(&with("(locked yes)")).unwrap_err();
        assert!(
            format!("{refusal:#}").contains("clause 'locked' is not supported"),
            "{refusal:#}"
        );
    }

    #[test]
    fn hide_no_is_visible_and_anything_else_is_refused() {
        let with = |hide: &str| {
            root(&format!(
                "(footprint \"X\" (model \"m.step\" (offset (xyz 0 0 0)) {hide}))"
            ))
        };
        assert!(models(&with("(hide no)")).unwrap()[0].visible);
        assert!(!models(&with("(hide yes)")).unwrap()[0].visible);
        assert!(!models(&with("hide")).unwrap()[0].visible);
        assert!(models(&with("(hide maybe)")).is_err());
    }

    // ─── #85: net-tie pad groups ────────────────────────────────────────────

    /// KiCad 10.0.8's stock net ties (`net_tie_kicad10.README.md`).
    const NET_TIE_2_SMD: &str =
        include_str!("../../tests/fixtures/net_tie_2_smd_pad0.5mm_kicad10.kicad_mod");
    const NET_TIE_3_THT: &str =
        include_str!("../../tests/fixtures/net_tie_3_tht_pad0.3mm_kicad10.kicad_mod");
    const NET_TIE_4_SMD: &str =
        include_str!("../../tests/fixtures/net_tie_4_smd_pad0.5mm_kicad10.kicad_mod");
    /// `NET_TIE_3_THT`'s group, as KiCad writes it.
    const NET_TIE_3_CLAUSE: &str = "(net_tie_pad_groups \"1, 2, 3\")";

    /// Each group's pad numbers.
    fn groups(net_ties: &[kiapi::board::types::NetTieDefinition]) -> Vec<Vec<&str>> {
        net_ties
            .iter()
            .map(|group| group.pad_number.iter().map(String::as_str).collect())
            .collect()
    }

    /// `NET_TIE_3_THT` with `clause` for its group.
    fn net_tie_3_with(clause: &str) -> String {
        assert!(
            NET_TIE_3_THT.contains(NET_TIE_3_CLAUSE),
            "the edit must land"
        );
        NET_TIE_3_THT.replace(NET_TIE_3_CLAUSE, clause)
    }

    /// Each stock net tie reads as its one group of every pad, in pad order,
    /// and neither placement nor the refresh lists or refuses the clause.
    #[test]
    fn stock_net_ties_read_as_their_one_pad_group() {
        for (source, expected) in [
            (NET_TIE_2_SMD, vec!["1", "2"]),
            (NET_TIE_3_THT, vec!["1", "2", "3"]),
            (NET_TIE_4_SMD, vec!["1", "2", "3", "4"]),
        ] {
            let footprint = root(source);
            assert_eq!(
                groups(&net_ties(&footprint).unwrap()),
                std::slice::from_ref(&expected),
                "{expected:?}"
            );
            assert_eq!(
                unsupported_features(&footprint).unwrap(),
                Vec::<String>::new(),
                "{expected:?}"
            );
            validate_supported_children(&footprint).unwrap();
        }
        assert!(net_ties(&root(C_0603)).unwrap().is_empty());
    }

    /// KiCad 10.0.8's report of the stock Antenova antenna it placed, whose
    /// library writes `(net_tie_pad_groups "1, 2")` (`issue_300_ipc.README.md`),
    /// holds the groups the reader sends.
    #[test]
    fn kicad_reports_a_placed_net_tie_as_the_reader_reads_it() {
        use prost::Message as _;
        let library = net_ties(&root(include_str!(
            "../../tests/fixtures/issue_300_antenova_sr4g013_kicad10.kicad_mod"
        )))
        .unwrap();
        assert_eq!(groups(&library), [vec!["1", "2"]]);
        let reported = kiapi::board::types::FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_300_u8.ipc.bin").as_slice(),
        )
        .unwrap();
        assert_eq!(reported.definition.unwrap().net_ties, library);
    }

    /// KiCad gives a footprint of format 20220815 or older whose keywords
    /// start with "net tie" one group of every pad, and KiCad 10.0.8's own
    /// upgrade of the hand-written KiCad 6 net tie writes that group
    /// (`library_upgrade/README.md`). From format 20220818 only the clause
    /// makes a net tie.
    #[test]
    fn an_older_net_tie_takes_its_group_from_the_keywords() {
        let legacy = net_ties(&root(include_str!(
            "../../tests/fixtures/library_upgrade/kicad5.pretty/NetTie_Legacy.kicad_mod"
        )))
        .unwrap();
        assert_eq!(groups(&legacy), [vec!["1", "2"]]);
        let upgraded = net_ties(&root(include_str!(
            "../../tests/fixtures/library_upgrade/kicad10.pretty/NetTie_Legacy.kicad_mod"
        )))
        .unwrap();
        assert_eq!(legacy, upgraded);

        let untied = NET_TIE_2_SMD.replace("\t(net_tie_pad_groups \"1, 2\")\n", "");
        assert_ne!(untied, NET_TIE_2_SMD, "the edit must land");
        assert!(untied.contains("(tags \"net tie\")"));
        assert!(net_ties(&root(&untied)).unwrap().is_empty());
    }

    /// Inline because KiCad writes none of them: a group KiCad's API would
    /// not give back as the library writes it, or that KiCad could not tie,
    /// refuses the footprint, saying why.
    #[test]
    fn a_net_tie_group_that_would_not_come_back_is_refused() {
        for (clause, expected) in [
            ("(net_tie_pad_groups 1)", "holds the unquoted '1'"),
            ("(net_tie_pad_groups (group \"1, 2\"))", "holds a list"),
            ("(net_tie_pad_groups)", "names no pad group"),
            ("(net_tie_pad_groups \"\")", "group \"\" names no pad"),
            ("(net_tie_pad_groups \" , \")", "names no pad"),
            ("(net_tie_pad_groups \"1, , 2\")", "has an empty pad number"),
            ("(net_tie_pad_groups \"1, 2,\")", "has an empty pad number"),
            (r#"(net_tie_pad_groups "1\\, 2")"#, "holds a backslash"),
            (
                "(net_tie_pad_groups \"1 2\")",
                "pad number '1 2' in net-tie pad group \"1 2\" holds whitespace",
            ),
            ("(net_tie_pad_groups \"1\t2\")", "holds whitespace"),
            (
                "(net_tie_pad_groups \"1,2\")",
                "KiCad would keep it as \"1, 2\"",
            ),
            (
                "(net_tie_pad_groups \"1 , 2\")",
                "KiCad would keep it as \"1, 2\"",
            ),
            (
                "(net_tie_pad_groups \"1, 4\")",
                "names pad '4', which the footprint does not have",
            ),
            ("(net_tie_pad_groups \"1, 2, 1\")", "names pad '1' twice"),
            (
                "(net_tie_pad_groups \"1, 2\" \"2, 3\")",
                "pad '2' is in two net-tie pad groups, \"1, 2\" and \"2, 3\"",
            ),
            (
                "(net_tie_pad_groups \"1, 2\") (net_tie_pad_groups \"3, 1\")",
                "pad '1' is in two net-tie pad groups",
            ),
        ] {
            let refusal = net_ties(&root(&net_tie_3_with(clause)))
                .unwrap_err()
                .to_string();
            assert!(refusal.contains(expected), "{clause}: {refusal}");
        }
    }

    /// Inline because KiCad writes every group in one clause: a second clause
    /// adds its groups after the first's, as KiCad's parser reads it, and
    /// each group keeps its own pad order.
    #[test]
    fn groups_keep_their_order_in_one_clause_or_two() {
        for clause in [
            "(net_tie_pad_groups \"3, 1\" \"2\")",
            "(net_tie_pad_groups \"3, 1\") (net_tie_pad_groups \"2\")",
        ] {
            assert_eq!(
                groups(&net_ties(&root(&net_tie_3_with(clause))).unwrap()),
                [vec!["3", "1"], vec!["2"]],
                "{clause}"
            );
        }
    }

    /// KiCad 10.0.0 joins a group's pad numbers with a space, so only 10.0.1
    /// and later carry net ties, and placement names what it leaves out.
    #[test]
    fn net_ties_are_carried_from_kicad_10_0_1() {
        let kicad = |major, minor, patch| copperpour_ipc::IpcKiCadVersion {
            major,
            minor,
            patch,
            full_version: format!("{major}.{minor}.{patch}"),
        };
        assert!(!carries_net_ties(&kicad(10, 0, 0)));
        for version in [
            kicad(10, 0, 1),
            kicad(10, 0, 6),
            kicad(10, 0, 8),
            kicad(10, 99, 0),
        ] {
            assert!(carries_net_ties(&version), "{}", version.full_version);
        }
        let ties = net_ties(&root(NET_TIE_3_THT)).unwrap();
        assert_eq!(net_ties_not_carried_reason(&ties, &kicad(10, 0, 1)), None);
        assert_eq!(net_ties_not_carried_reason(&[], &kicad(10, 0, 0)), None);
        assert_eq!(
            net_ties_not_carried_reason(&ties, &kicad(10, 0, 0)).as_deref(),
            Some(
                "net-tie pad groups (\"1, 2, 3\") require KiCad 10.0.1 or newer (connected \
                 KiCad is 10.0.0)"
            )
        );
    }
}
