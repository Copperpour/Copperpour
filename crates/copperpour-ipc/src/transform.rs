//! Rigid-body transforms for `FootprintInstance` round-trips (issue #23).
//!
//! KiCAD 10 serializes every child of a footprint (pads, fields, texts,
//! graphics, zones) in ABSOLUTE board coordinates — despite the proto comment
//! on `Pad.position` claiming footprint-relative coordinates — and
//! `FOOTPRINT::Deserialize` clears all children and re-creates them at exactly
//! the coordinates carried in the message. Round-tripping an instance with
//! only `position` or `orientation` changed therefore leaves the copper and
//! silkscreen at the old location, detached from the anchor. Before sending
//! `UpdateItems`, every child must get the same rigid-body transform KiCAD
//! would apply natively (`FOOTPRINT::SetPosition` / `SetOrientation`).

use crate::client::{PreflightCause, PreflightRefusal};
use crate::gen::kiapi;
use anyhow::Result;
use kiapi::board::types as board;
use kiapi::common::types as common;
use prost::Message;

/// The transform to apply to a footprint's children.
#[derive(Clone, Copy)]
pub enum Xform {
    /// Shift every child by (dx, dy) nanometers.
    Translate { dx_nm: i64, dy_nm: i64 },
    /// Rotate every child by `delta_deg` around (cx, cy), matching KiCAD's
    /// convention (Y axis down; positive angles counterclockwise on screen).
    Rotate {
        cx_nm: i64,
        cy_nm: i64,
        delta_deg: f64,
    },
}

/// KiCad's `RotatePoint` on a point in nanometres, about the origin
/// (`libs/kimath/src/trigo.cpp`): the angle normalised to [0, 360), the four
/// right angles exact, and otherwise each coordinate rounded half away from
/// zero, with `EDA_ANGLE`'s `M_SQRT1_2` sine and cosine at the odd multiples
/// of 45°.
pub fn rotate_point(x: i64, y: i64, degrees: f64) -> (i64, i64) {
    let angle = within_a_turn(degrees);
    if angle == 0.0 {
        return (x, y);
    }
    if angle == 90.0 {
        return (y, -x);
    }
    if angle == 180.0 {
        return (-x, -y);
    }
    if angle == 270.0 {
        return (-y, x);
    }
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let (sin, cos) = match angle {
        45.0 => (half, half),
        135.0 => (half, -half),
        225.0 => (-half, -half),
        315.0 => (-half, half),
        _ => {
            let radians = angle * (std::f64::consts::PI / 180.0);
            (radians.sin(), radians.cos())
        }
    };
    let (x, y) = (x as f64, y as f64);
    (
        (y * sin + x * cos).round() as i64,
        (y * cos - x * sin).round() as i64,
    )
}

impl Xform {
    pub(crate) fn point(&self, x: i64, y: i64) -> (i64, i64) {
        match *self {
            Xform::Translate { dx_nm, dy_nm } => (x + dx_nm, y + dy_nm),
            Xform::Rotate {
                cx_nm,
                cy_nm,
                delta_deg,
            } => {
                // KiCad's RotatePoint about a centre: subtract it, turn, add it.
                let (x, y) = rotate_point(x - cx_nm, y - cy_nm, delta_deg);
                (cx_nm + x, cy_nm + y)
            }
        }
    }

    fn angle_delta_deg(&self) -> f64 {
        match *self {
            Xform::Translate { .. } => 0.0,
            Xform::Rotate { delta_deg, .. } => delta_deg,
        }
    }

    /// True when the rotation keeps axis-aligned shapes axis-aligned.
    fn is_cardinal(&self) -> bool {
        let r = self.angle_delta_deg().rem_euclid(90.0);
        r.abs() < 1e-9 || (90.0 - r).abs() < 1e-9
    }
}

/// `degrees` in [0, 360), as `EDA_ANGLE::Normalize` puts an angle
/// (`libs/kimath/include/geometry/eda_angle.h:229-238` at 10.0.6).
pub fn within_a_turn(degrees: f64) -> f64 {
    let turn = degrees.rem_euclid(360.0);
    if turn >= 360.0 {
        turn - 360.0
    } else {
        turn
    }
}

/// Normalize to (-180, 180], the range KiCAD's `EDA_ANGLE::Normalize180` uses.
pub fn normalize_deg(deg: f64) -> f64 {
    let mut d = deg % 360.0;
    if d <= -180.0 {
        d += 360.0;
    } else if d > 180.0 {
        d -= 360.0;
    }
    d
}

fn xform_vec2(v: &mut Option<common::Vector2>, xf: &Xform) {
    if let Some(p) = v.as_mut() {
        let (x, y) = xf.point(p.x_nm, p.y_nm);
        p.x_nm = x;
        p.y_nm = y;
    }
}

fn xform_angle(a: &mut Option<common::Angle>, xf: &Xform) {
    let delta = xf.angle_delta_deg();
    if delta != 0.0 {
        let base = a.as_ref().map(|x| x.value_degrees).unwrap_or(0.0);
        *a = Some(common::Angle {
            value_degrees: normalize_deg(base + delta),
        });
    }
}

fn xform_text(t: &mut Option<common::Text>, xf: &Xform) {
    if let Some(text) = t.as_mut() {
        xform_vec2(&mut text.position, xf);
        if let Some(attrs) = text.attributes.as_mut() {
            xform_angle(&mut attrs.angle, xf);
        }
    }
}

fn xform_board_text(bt: &mut board::BoardText, xf: &Xform) {
    xform_text(&mut bt.text, xf);
}

fn xform_field(f: &mut Option<board::Field>, xf: &Xform) {
    if let Some(field) = f.as_mut() {
        if let Some(text) = field.text.as_mut() {
            xform_board_text(text, xf);
        }
    }
}

fn xform_polyline(pl: &mut common::PolyLine, xf: &Xform) {
    for node in pl.nodes.iter_mut() {
        match node.geometry.as_mut() {
            Some(common::poly_line_node::Geometry::Point(p)) => {
                let (x, y) = xf.point(p.x_nm, p.y_nm);
                p.x_nm = x;
                p.y_nm = y;
            }
            Some(common::poly_line_node::Geometry::Arc(arc)) => {
                xform_vec2(&mut arc.start, xf);
                xform_vec2(&mut arc.mid, xf);
                xform_vec2(&mut arc.end, xf);
            }
            None => {}
        }
    }
}

fn xform_polyset(ps: &mut common::PolySet, xf: &Xform) {
    for poly in ps.polygons.iter_mut() {
        if let Some(outline) = poly.outline.as_mut() {
            xform_polyline(outline, xf);
        }
        for hole in poly.holes.iter_mut() {
            xform_polyline(hole, xf);
        }
    }
}

/// The layer of `gs` when it is a rounded rectangle `xf` turns off axis.
/// KiCAD always sends `corner_radius`, 0 for a square corner, so only a
/// positive radius is rounded. KiCAD approximates a rotated rounded rectangle
/// with a segmented polygon; refuse rather than guess its arc tolerance.
fn rounded_off_axis(gs: &board::BoardGraphicShape, xf: &Xform) -> Option<&'static str> {
    use common::graphic_shape::Geometry;
    let Some(Geometry::Rectangle(rect)) = gs.shape.as_ref().and_then(|s| s.geometry.as_ref())
    else {
        return None;
    };
    let layer = gs.layer();
    (!xf.is_cardinal() && rect.corner_radius.is_some_and(|r| r.value_nm > 0))
        .then(|| crate::builders::layer_name(layer).unwrap_or(layer.as_str_name()))
}

fn xform_graphic_shape(
    gs: &mut board::BoardGraphicShape,
    xf: &Xform,
    reference: &str,
) -> Result<()> {
    use common::graphic_shape::Geometry;

    if let Some(layer) = rounded_off_axis(gs, xf) {
        return Err(rotation_refusal(
            reference,
            format!(
                "cannot rotate footprint '{reference}' by a non-90° angle: its \
                 rounded-rectangle graphic on {layer} has no off-axis form; use a \
                 multiple of 90°"
            ),
        ));
    }
    let Some(shape) = gs.shape.as_mut() else {
        return Ok(());
    };
    match shape.geometry.as_mut() {
        Some(Geometry::Segment(seg)) => {
            xform_vec2(&mut seg.start, xf);
            xform_vec2(&mut seg.end, xf);
        }
        Some(Geometry::Rectangle(rect)) => {
            if xf.is_cardinal() {
                xform_vec2(&mut rect.top_left, xf);
                xform_vec2(&mut rect.bottom_right, xf);
            } else {
                // Mirror KiCAD's EDA_SHAPE::Rotate: an off-axis rectangle
                // becomes a closed polygon of its four rotated corners.
                let (tl, br) = (
                    rect.top_left.unwrap_or_default(),
                    rect.bottom_right.unwrap_or_default(),
                );
                let corners = [
                    (tl.x_nm, tl.y_nm),
                    (br.x_nm, tl.y_nm),
                    (br.x_nm, br.y_nm),
                    (tl.x_nm, br.y_nm),
                ];
                let nodes = corners
                    .iter()
                    .map(|&(x, y)| {
                        let (nx, ny) = xf.point(x, y);
                        common::PolyLineNode {
                            geometry: Some(common::poly_line_node::Geometry::Point(
                                common::Vector2 { x_nm: nx, y_nm: ny },
                            )),
                        }
                    })
                    .collect();
                shape.geometry = Some(Geometry::Polygon(common::PolySet {
                    polygons: vec![common::PolygonWithHoles {
                        outline: Some(common::PolyLine {
                            nodes,
                            closed: true,
                        }),
                        holes: vec![],
                    }],
                }));
            }
        }
        Some(Geometry::Arc(arc)) => {
            xform_vec2(&mut arc.start, xf);
            xform_vec2(&mut arc.mid, xf);
            xform_vec2(&mut arc.end, xf);
        }
        Some(Geometry::Circle(circle)) => {
            xform_vec2(&mut circle.center, xf);
            xform_vec2(&mut circle.radius_point, xf);
        }
        Some(Geometry::Polygon(polyset)) => {
            xform_polyset(polyset, xf);
        }
        Some(Geometry::Bezier(bez)) => {
            xform_vec2(&mut bez.start, xf);
            xform_vec2(&mut bez.control1, xf);
            xform_vec2(&mut bez.control2, xf);
            xform_vec2(&mut bez.end, xf);
        }
        None => {}
    }
    Ok(())
}

/// The refusal of a rotation one of footprint `reference`'s children cannot
/// follow: Copperpour's own, raised before the footprint's update is sent
/// (#240).
fn rotation_refusal(reference: &str, reason: String) -> anyhow::Error {
    PreflightRefusal::new(
        PreflightCause::Rotation {
            reference: reference.to_string(),
        },
        reason,
    )
    .into()
}

/// A text box's corners and angle carried by `xf`, a right angle or a move.
fn xform_text_box(tb: &mut board::BoardTextBox, xf: &Xform) {
    if let Some(textbox) = tb.textbox.as_mut() {
        xform_vec2(&mut textbox.top_left, xf);
        xform_vec2(&mut textbox.bottom_right, xf);
        if let Some(attrs) = textbox.attributes.as_mut() {
            xform_angle(&mut attrs.angle, xf);
        }
    }
}

/// A zone's outline and holes carried by `xf`, its cached fill with them as
/// it is: the fill is moved, not rebuilt.
pub fn transform_zone(zone: &mut board::Zone, xf: &Xform) {
    if let Some(outline) = zone.outline.as_mut() {
        xform_polyset(outline, xf);
    }
    for filled in zone.filled_polygons.iter_mut() {
        if let Some(shapes) = filled.shapes.as_mut() {
            xform_polyset(shapes, xf);
        }
    }
}

fn xform_dimension(dim: &mut board::Dimension, xf: &Xform) {
    use board::dimension::DimensionStyle;

    xform_text(&mut dim.text, xf);
    match dim.dimension_style.as_mut() {
        Some(DimensionStyle::Aligned(a)) => {
            xform_vec2(&mut a.start, xf);
            xform_vec2(&mut a.end, xf);
        }
        Some(DimensionStyle::Orthogonal(o)) => {
            xform_vec2(&mut o.start, xf);
            xform_vec2(&mut o.end, xf);
        }
        Some(DimensionStyle::Radial(r)) => {
            xform_vec2(&mut r.center, xf);
            xform_vec2(&mut r.radius_point, xf);
        }
        Some(DimensionStyle::Leader(l)) => {
            xform_vec2(&mut l.start, xf);
            xform_vec2(&mut l.end, xf);
        }
        Some(DimensionStyle::Center(c)) => {
            xform_vec2(&mut c.center, xf);
            xform_vec2(&mut c.end, xf);
        }
        None => {}
    }
}

/// Apply `xf` to every position-bearing child carried in `fp`: the four
/// mandatory fields plus all `definition.items`. Does NOT touch
/// `fp.position` / `fp.orientation` — the caller sets those.
pub fn transform_footprint_children(fp: &mut board::FootprintInstance, xf: &Xform) -> Result<()> {
    let reference = crate::builders::footprint_reference(fp).to_owned();
    xform_field(&mut fp.reference_field, xf);
    xform_field(&mut fp.value_field, xf);
    xform_field(&mut fp.datasheet_field, xf);
    xform_field(&mut fp.description_field, xf);

    let Some(def) = fp.definition.as_mut() else {
        return Ok(());
    };

    for item in def.items.iter_mut() {
        // Discriminate on the declared type, never on whether a decode
        // happens to succeed — see `builders::any_is` for why that distinction
        // cost every synced footprint its graphics (#244).
        let url = crate::builders::any_type_name(item);
        if url == "kiapi.board.types.Pad" {
            let mut pad = board::Pad::decode(item.value.as_slice())?;
            xform_vec2(&mut pad.position, xf);
            if let Some(ps) = pad.pad_stack.as_mut() {
                xform_angle(&mut ps.angle, xf);
            }
            item.value = pad.encode_to_vec();
        } else if url == "kiapi.board.types.BoardText" {
            let mut text = board::BoardText::decode(item.value.as_slice())?;
            xform_board_text(&mut text, xf);
            item.value = text.encode_to_vec();
        } else if url == "kiapi.board.types.BoardTextBox" {
            if !xf.is_cardinal() {
                return Err(rotation_refusal(
                    &reference,
                    format!(
                        "cannot rotate footprint '{reference}' by a non-90° angle: it \
                         contains a textbox; use a multiple of 90°"
                    ),
                ));
            }
            let mut tb = board::BoardTextBox::decode(item.value.as_slice())?;
            xform_text_box(&mut tb, xf);
            item.value = tb.encode_to_vec();
        } else if url == "kiapi.board.types.Field" {
            let mut field = board::Field::decode(item.value.as_slice())?;
            if let Some(text) = field.text.as_mut() {
                xform_board_text(text, xf);
            }
            item.value = field.encode_to_vec();
        } else if url == "kiapi.board.types.BoardGraphicShape" {
            let mut shape = board::BoardGraphicShape::decode(item.value.as_slice())?;
            xform_graphic_shape(&mut shape, xf, &reference)?;
            item.value = shape.encode_to_vec();
        } else if url == "kiapi.board.types.Zone" {
            let mut zone = board::Zone::decode(item.value.as_slice())?;
            transform_zone(&mut zone, xf);
            item.value = zone.encode_to_vec();
        } else if url == "kiapi.board.types.Dimension" {
            let mut dim = board::Dimension::decode(item.value.as_slice())?;
            xform_dimension(&mut dim, xf);
            item.value = dim.encode_to_vec();
        } else if url == "kiapi.board.types.ReferencePoint" {
            // A footprint's `(point …)`, sent since KiCad 10.0.7 (#270). It
            // has a position and no angle: `PCB_POINT::Rotate` turns only the
            // position about the centre.
            let mut point = board::ReferencePoint::decode(item.value.as_slice())?;
            xform_vec2(&mut point.position, xf);
            item.value = point.encode_to_vec();
        } else if url == "kiapi.board.types.Footprint3DModel" || url == "kiapi.board.types.Group" {
            // No board-space geometry of their own — nothing to shift.
        } else {
            // KiCAD would re-create this item wherever the stale message
            // says it is; refusing beats silently corrupting the board.
            return Err(PreflightRefusal::new(
                PreflightCause::UnsupportedItem {
                    reference: reference.clone(),
                    item_type: url.to_string(),
                },
                format!(
                    "footprint '{reference}' contains an item of unsupported type '{url}'; \
                     refusing to move/rotate it via IPC to avoid detaching it"
                ),
            )
            .into());
        }
    }
    Ok(())
}

/// The last step of KiCad's `FOOTPRINT::Rotate` (`pcbnew/footprint.cpp:2849-2856`
/// at 10.0.6), after its children are turned: `PCB_TEXT::KeepUpright`
/// (`pcbnew/pcb_text.cpp:379-397`) on each field and text child of `fp`. A
/// text with keep-upright set whose angle, taken into [0, 360), is 180° or
/// more turns half round and swaps left and right, and top and bottom; a
/// centred one stays centred. Text boxes are not texts there and are left.
pub fn keep_footprint_text_upright(fp: &mut board::FootprintInstance) -> Result<()> {
    for field in [
        &mut fp.reference_field,
        &mut fp.value_field,
        &mut fp.datasheet_field,
        &mut fp.description_field,
    ] {
        if let Some(text) = field.as_mut().and_then(|field| field.text.as_mut()) {
            keep_upright(&mut text.text);
        }
    }
    let Some(def) = fp.definition.as_mut() else {
        return Ok(());
    };
    for item in def.items.iter_mut() {
        let url = crate::builders::any_type_name(item);
        if url == "kiapi.board.types.Field" {
            let mut field = board::Field::decode(item.value.as_slice())?;
            if let Some(text) = field.text.as_mut() {
                keep_upright(&mut text.text);
            }
            item.value = field.encode_to_vec();
        } else if url == "kiapi.board.types.BoardText" {
            let mut text = board::BoardText::decode(item.value.as_slice())?;
            keep_upright(&mut text.text);
            item.value = text.encode_to_vec();
        }
    }
    Ok(())
}

fn keep_upright(text: &mut Option<common::Text>) {
    use common::{HorizontalAlignment as H, VerticalAlignment as V};
    let Some(attrs) = text.as_mut().and_then(|text| text.attributes.as_mut()) else {
        return;
    };
    let angle = within_a_turn(attrs.angle.map_or(0.0, |angle| angle.value_degrees));
    if !attrs.keep_upright || angle < 180.0 {
        return;
    }
    attrs.horizontal_alignment = match H::try_from(attrs.horizontal_alignment) {
        Ok(H::HaLeft) => H::HaRight as i32,
        Ok(H::HaRight) => H::HaLeft as i32,
        _ => attrs.horizontal_alignment,
    };
    attrs.vertical_alignment = match V::try_from(attrs.vertical_alignment) {
        Ok(V::VaTop) => V::VaBottom as i32,
        Ok(V::VaBottom) => V::VaTop as i32,
        _ => attrs.vertical_alignment,
    };
    attrs.angle = Some(common::Angle {
        value_degrees: within_a_turn(angle + 180.0),
    });
}

/// The transforms that carry a footprint's children from the pose KiCad
/// holds it at to `position` and `rotation_deg`: a rotation about the held
/// anchor by the change in orientation, then a translation by the change in
/// position. A step that changes nothing is left out.
///
/// `KiCadIpcClient::set_footprint_placements` moves the children by exactly
/// these, and the placement copper check (#96) moves KiCad's pad outlines by
/// them, so the check tests the copper where the placement will put it.
pub fn placement_xforms(
    fp: &board::FootprintInstance,
    position: common::Vector2,
    rotation_deg: f64,
) -> Vec<Xform> {
    let old_position = fp.position.unwrap_or_default();
    let old_rotation = fp
        .orientation
        .as_ref()
        .map(|angle| angle.value_degrees)
        .unwrap_or(0.0);
    let mut xforms = Vec::with_capacity(2);
    let rotation_delta = rotation_deg - old_rotation;
    if rotation_delta != 0.0 {
        xforms.push(Xform::Rotate {
            cx_nm: old_position.x_nm,
            cy_nm: old_position.y_nm,
            delta_deg: rotation_delta,
        });
    }
    let (dx_nm, dy_nm) = (
        position.x_nm - old_position.x_nm,
        position.y_nm - old_position.y_nm,
    );
    if dx_nm != 0 || dy_nm != 0 {
        xforms.push(Xform::Translate { dx_nm, dy_nm });
    }
    xforms
}

/// Move a polygon KiCad reported in board coordinates by `xf`, node by node,
/// as a footprint's own polygon children move: points directly, arcs by
/// their start, mid and end.
pub fn transform_polygon(polygon: &mut common::PolygonWithHoles, xf: &Xform) {
    if let Some(outline) = polygon.outline.as_mut() {
        xform_polyline(outline, xf);
    }
    for hole in polygon.holes.iter_mut() {
        xform_polyline(hole, xf);
    }
}

/// A board graphic carried by `xf` as a footprint's graphic is. A rounded
/// rectangle turned by an angle that is not a multiple of 90° is refused: it
/// has no exact form there.
pub fn transform_board_graphic(gs: &mut board::BoardGraphicShape, xf: &Xform) -> Result<()> {
    if let Some(layer) = rounded_off_axis(gs, xf) {
        anyhow::bail!(
            "its rounded rectangle on {layer} has no exact form at a turn that is not a \
             multiple of 90°"
        );
    }
    xform_graphic_shape(gs, xf, "")
}

/// A board text box carried by `xf` as a footprint's is. A turn that is not
/// a multiple of 90° is refused: KiCad 10.0.8 crashes on a text box set at
/// such an angle.
pub fn transform_board_text_box(tb: &mut board::BoardTextBox, xf: &Xform) -> Result<()> {
    anyhow::ensure!(
        xf.is_cardinal(),
        "KiCad cannot hold a text box turned by an angle that is not a multiple of 90°"
    );
    xform_text_box(tb, xf);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builders;

    fn pad_at(x_mm: f64, y_mm: f64, angle_deg: f64) -> board::Pad {
        board::Pad {
            position: Some(builders::vec2(x_mm, y_mm)),
            pad_stack: Some(board::PadStack {
                angle: Some(common::Angle {
                    value_degrees: angle_deg,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn field_at(x_mm: f64, y_mm: f64) -> board::Field {
        board::Field {
            text: Some(board::BoardText {
                text: Some(common::Text {
                    position: Some(builders::vec2(x_mm, y_mm)),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn instance_with(items: Vec<prost_types::Any>) -> board::FootprintInstance {
        board::FootprintInstance {
            position: Some(builders::vec2(100.0, 100.0)),
            reference_field: Some(field_at(100.0, 98.0)),
            definition: Some(board::Footprint {
                items,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn decode_pad(any: &prost_types::Any) -> board::Pad {
        board::Pad::decode(any.value.as_slice()).unwrap()
    }

    #[test]
    fn translate_shifts_pads_fields_and_graphics() {
        let pad = builders::pack_any(&pad_at(100.0, 100.0, 0.0), "kiapi.board.types.Pad");
        let seg = builders::pack_any(
            &builders::board_segment("F.SilkS", 0.1, 99.0, 99.0, 101.0, 99.0),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![pad, seg]);

        // Move 100,100 -> 50,50: delta is -50mm on both axes.
        let xf = Xform::Translate {
            dx_nm: builders::mm_to_nm(-50.0),
            dy_nm: builders::mm_to_nm(-50.0),
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let items = &fp.definition.as_ref().unwrap().items;
        let pad = decode_pad(&items[0]);
        assert_eq!(pad.position.unwrap(), builders::vec2(50.0, 50.0));

        let shape = board::BoardGraphicShape::decode(items[1].value.as_slice()).unwrap();
        match shape.shape.unwrap().geometry.unwrap() {
            common::graphic_shape::Geometry::Segment(s) => {
                assert_eq!(s.start.unwrap(), builders::vec2(49.0, 49.0));
                assert_eq!(s.end.unwrap(), builders::vec2(51.0, 49.0));
            }
            other => panic!("expected segment, got {other:?}"),
        }

        let ref_text = fp.reference_field.unwrap().text.unwrap().text.unwrap();
        assert_eq!(ref_text.position.unwrap(), builders::vec2(50.0, 48.0));
    }

    #[test]
    fn rotate_90_matches_kicad_convention() {
        // Pad 1mm to the right of the anchor, at pad angle 0.
        let pad = builders::pack_any(&pad_at(101.0, 100.0, 0.0), "kiapi.board.types.Pad");
        let mut fp = instance_with(vec![pad]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 90.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        // KiCAD positive rotation is counterclockwise on screen (Y down):
        // (+1, 0) relative -> (0, -1) relative.
        let pad = decode_pad(&fp.definition.as_ref().unwrap().items[0]);
        assert_eq!(pad.position.unwrap(), builders::vec2(100.0, 99.0));
        assert_eq!(pad.pad_stack.unwrap().angle.unwrap().value_degrees, 90.0);
    }

    #[test]
    fn rotate_angle_normalizes_to_plus_minus_180() {
        let pad = builders::pack_any(&pad_at(100.0, 100.0, 170.0), "kiapi.board.types.Pad");
        let mut fp = instance_with(vec![pad]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 20.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let pad = decode_pad(&fp.definition.as_ref().unwrap().items[0]);
        assert_eq!(pad.pad_stack.unwrap().angle.unwrap().value_degrees, -170.0);
    }

    #[test]
    fn non_cardinal_rotation_turns_rectangle_into_polygon() {
        let rect = builders::pack_any(
            &builders::board_rectangle("F.Fab", 0.1, 99.0, 99.0, 101.0, 101.0, false),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![rect]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 45.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let shape = board::BoardGraphicShape::decode(
            fp.definition.as_ref().unwrap().items[0].value.as_slice(),
        )
        .unwrap();
        match shape.shape.unwrap().geometry.unwrap() {
            common::graphic_shape::Geometry::Polygon(ps) => {
                let outline = ps.polygons[0].outline.as_ref().unwrap();
                assert!(outline.closed);
                assert_eq!(outline.nodes.len(), 4);
            }
            other => panic!("expected polygon after 45° rotation, got {other:?}"),
        }
    }

    /// A rectangle as KiCAD sends it: `EDA_SHAPE::Serialize` always sets
    /// `corner_radius`, to 0 for a square-cornered one (#224).
    fn kicad_rectangle(layer: &str, corner_radius_nm: i64) -> prost_types::Any {
        let mut rect = builders::board_rectangle(layer, 0.05, 99.0, 99.0, 101.0, 101.0, false);
        if let Some(common::graphic_shape::Geometry::Rectangle(r)) = rect
            .shape
            .as_mut()
            .and_then(|shape| shape.geometry.as_mut())
        {
            r.corner_radius = Some(common::Distance {
                value_nm: corner_radius_nm,
            });
        }
        builders::pack_any(&rect, "kiapi.board.types.BoardGraphicShape")
    }

    #[test]
    fn square_cornered_rectangle_with_zero_radius_rotates_to_polygon() {
        let mut fp = instance_with(vec![kicad_rectangle("F.CrtYd", 0)]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: -45.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let shape = board::BoardGraphicShape::decode(
            fp.definition.as_ref().unwrap().items[0].value.as_slice(),
        )
        .unwrap();
        let common::graphic_shape::Geometry::Polygon(ps) = shape.shape.unwrap().geometry.unwrap()
        else {
            panic!("expected a polygon after a -45° rotation");
        };
        let corners: Vec<_> = ps.polygons[0]
            .outline
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|node| match node.geometry {
                Some(common::poly_line_node::Geometry::Point(p)) => (p.x_nm, p.y_nm),
                ref other => panic!("expected a point, got {other:?}"),
            })
            .collect();
        // The 2 mm square around the anchor turns into a diamond whose
        // corners lie √2 mm (1 414 214 nm) from it.
        assert_eq!(
            corners,
            vec![
                (100_000_000, 98_585_786),
                (101_414_214, 100_000_000),
                (100_000_000, 101_414_214),
                (98_585_786, 100_000_000),
            ]
        );
    }

    #[test]
    fn rounded_rectangle_refusal_names_the_footprint_and_layer() {
        let mut fp = instance_with(vec![kicad_rectangle("F.Fab", 250_000)]);
        let reference = fp.reference_field.as_mut().unwrap().text.as_mut().unwrap();
        reference.text.as_mut().unwrap().text = "Y1".to_string();
        let before = fp.clone();

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 45.0,
        };
        let error = transform_footprint_children(&mut fp, &xf).unwrap_err();
        let err = error.to_string();
        assert!(err.contains("'Y1'") && err.contains("F.Fab"), "{err}");
        assert_eq!(fp.definition, before.definition);
        assert_eq!(
            refusal_cause(&error),
            PreflightCause::Rotation {
                reference: "Y1".to_string()
            }
        );
    }

    /// The typed refusal a caller classifies the error by (#240).
    fn refusal_cause(error: &anyhow::Error) -> PreflightCause {
        PreflightRefusal::from_error(error)
            .expect("Copperpour's own refusal, typed")
            .cause
            .clone()
    }

    #[test]
    fn cardinal_rotation_keeps_rectangle_a_rectangle() {
        let rect = builders::pack_any(
            &builders::board_rectangle("F.Fab", 0.1, 99.0, 99.0, 101.0, 101.0, false),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![rect]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 180.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let shape = board::BoardGraphicShape::decode(
            fp.definition.as_ref().unwrap().items[0].value.as_slice(),
        )
        .unwrap();
        assert!(matches!(
            shape.shape.unwrap().geometry.unwrap(),
            common::graphic_shape::Geometry::Rectangle(_)
        ));
    }

    #[test]
    fn textbox_refusal_names_the_footprint() {
        let textbox = builders::pack_any(
            &board::BoardTextBox::default(),
            "kiapi.board.types.BoardTextBox",
        );
        let mut fp = instance_with(vec![textbox]);
        let reference = fp.reference_field.as_mut().unwrap().text.as_mut().unwrap();
        reference.text.as_mut().unwrap().text = "U3".to_string();

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 30.0,
        };
        let error = transform_footprint_children(&mut fp, &xf).unwrap_err();
        let err = error.to_string();
        assert!(err.contains("'U3'") && err.contains("textbox"), "{err}");
        assert_eq!(
            refusal_cause(&error),
            PreflightCause::Rotation {
                reference: "U3".to_string()
            }
        );
    }

    #[test]
    fn unknown_item_type_is_rejected_not_corrupted() {
        let bogus = prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.SomethingNew".to_string(),
            value: vec![],
        };
        let mut fp = instance_with(vec![bogus]);
        let reference = fp.reference_field.as_mut().unwrap().text.as_mut().unwrap();
        reference.text.as_mut().unwrap().text = "J2".to_string();

        let xf = Xform::Translate {
            dx_nm: 1000,
            dy_nm: 0,
        };
        let error = transform_footprint_children(&mut fp, &xf).unwrap_err();
        let err = error.to_string();
        assert!(
            err.contains("SomethingNew") && err.contains("'J2'"),
            "{err}"
        );
        assert_eq!(
            refusal_cause(&error),
            PreflightCause::UnsupportedItem {
                reference: "J2".to_string(),
                item_type: "kiapi.board.types.SomethingNew".to_string(),
            }
        );
    }

    /// A reference point moves and turns with its footprint, like a pad,
    /// and keeps everything else it carries (#270).
    #[test]
    fn a_reference_point_follows_a_move_and_a_rotation() {
        let point = board::ReferencePoint {
            id: Some(common::Kiid {
                value: "09df91a3-71d6-4eb0-83b3-da5f6b670a55".to_string(),
            }),
            layer: board::BoardLayer::BlFFab as i32,
            position: Some(builders::vec2(103.81, 103.81)),
            size: Some(common::Distance {
                value_nm: 1_270_000,
            }),
            locked: common::LockedState::LsUnlocked as i32,
            parent: Some(common::Kiid {
                value: "83a1c1ab-7932-487b-8ebc-9a7a98d9cd20".to_string(),
            }),
        };
        let at = |position| board::ReferencePoint {
            position: Some(position),
            ..point.clone()
        };
        let carried = |xf: Xform| {
            let mut fp = instance_with(vec![builders::pack_any(
                &point,
                "kiapi.board.types.ReferencePoint",
            )]);
            transform_footprint_children(&mut fp, &xf).unwrap();
            let items = fp.definition.unwrap().items;
            board::ReferencePoint::decode(items[0].value.as_slice()).unwrap()
        };

        let moved = carried(Xform::Translate {
            dx_nm: builders::mm_to_nm(5.0),
            dy_nm: builders::mm_to_nm(-2.0),
        });
        assert_eq!(moved, at(builders::vec2(108.81, 101.81)));

        // (+3.81, +3.81) from the anchor turns to (+3.81, −3.81), KiCad's
        // counterclockwise on screen with Y down.
        let turned = carried(Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg: 90.0,
        });
        assert_eq!(turned, at(builders::vec2(103.81, 96.19)));
    }

    /// A text at `angle_deg` with its keep-upright flag and justification.
    fn text(angle_deg: f64, keep_upright: bool, h: i32, v: i32) -> common::Text {
        common::Text {
            attributes: Some(common::TextAttributes {
                angle: Some(common::Angle {
                    value_degrees: angle_deg,
                }),
                keep_upright,
                horizontal_alignment: h,
                vertical_alignment: v,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn field_with(text: common::Text) -> board::Field {
        board::Field {
            text: Some(board::BoardText {
                text: Some(text),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn keep_upright_text_past_180_turns_half_round_and_swaps_its_justification() {
        use common::{HorizontalAlignment as H, VerticalAlignment as V};
        let (left, right, h_centre) = (H::HaLeft as i32, H::HaRight as i32, H::HaCenter as i32);
        let (top, bottom, v_centre) = (V::VaTop as i32, V::VaBottom as i32, V::VaCenter as i32);
        let textbox = builders::pack_any(
            &board::BoardTextBox::default(),
            "kiapi.board.types.BoardTextBox",
        );
        let mut fp = instance_with(vec![
            builders::pack_any(
                &board::BoardText {
                    text: Some(text(-90.0, true, h_centre, v_centre)),
                    ..Default::default()
                },
                "kiapi.board.types.BoardText",
            ),
            builders::pack_any(
                &field_with(text(90.0, true, left, bottom)),
                "kiapi.board.types.Field",
            ),
            textbox.clone(),
        ]);
        fp.reference_field = Some(field_with(text(225.0, true, left, top)));
        fp.value_field = Some(field_with(text(225.0, false, left, top)));

        keep_footprint_text_upright(&mut fp).unwrap();

        let attrs = |field: &Option<board::Field>| {
            let field = field.as_ref().unwrap().text.as_ref().unwrap();
            field.text.as_ref().unwrap().attributes.clone().unwrap()
        };
        let summary = |attrs: common::TextAttributes| {
            let angle = attrs.angle.unwrap().value_degrees;
            (angle, attrs.horizontal_alignment, attrs.vertical_alignment)
        };
        // 225 is past 180: half round to 45, left to right, top to bottom.
        assert_eq!(summary(attrs(&fp.reference_field)), (45.0, right, bottom));
        // Without keep-upright nothing changes.
        assert_eq!(summary(attrs(&fp.value_field)), (225.0, left, top));
        let items = &fp.definition.as_ref().unwrap().items;
        let board_text = board::BoardText::decode(items[0].value.as_slice()).unwrap();
        // -90 is 270 in [0, 360): half round to 90, centred stays centred.
        assert_eq!(
            summary(board_text.text.unwrap().attributes.unwrap()),
            (90.0, h_centre, v_centre)
        );
        // 90 is upright already.
        let field = board::Field::decode(items[1].value.as_slice()).unwrap();
        assert_eq!(
            summary(field.text.unwrap().text.unwrap().attributes.unwrap()),
            (90.0, left, bottom)
        );
        assert_eq!(items[2], textbox);
    }

    #[test]
    fn a_board_rounded_rectangle_turns_by_right_angles_only() {
        let turn = |delta_deg| Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg,
        };
        let rounded = || {
            board::BoardGraphicShape::decode(kicad_rectangle("F.Fab", 250_000).value.as_slice())
                .unwrap()
        };
        let mut shape = rounded();
        let error = transform_board_graphic(&mut shape, &turn(45.0)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "its rounded rectangle on F.Fab has no exact form at a turn that is not a \
             multiple of 90°"
        );
        assert_eq!(shape, rounded(), "nothing moved before the refusal");

        let mut shape = rounded();
        transform_board_graphic(&mut shape, &turn(90.0)).unwrap();
        let Some(common::graphic_shape::Geometry::Rectangle(rect)) = shape.shape.unwrap().geometry
        else {
            panic!("still a rectangle")
        };
        // (99, 99) and (101, 101) about (100, 100): (99, 101) and (101, 99).
        assert_eq!(rect.top_left.unwrap(), builders::vec2(99.0, 101.0));
        assert_eq!(rect.bottom_right.unwrap(), builders::vec2(101.0, 99.0));
    }

    /// pcbnew's own turn of these points by 45°, with `EDA_ANGLE`'s
    /// `M_SQRT1_2`, as `repeat_layout`'s `a_45_degree_turn_rounds_as_kicad_rounds_it`
    /// holds them: the sine of 45° in radians puts `y` a nanometre away.
    #[test]
    fn a_turn_by_45_rounds_as_kicad_rounds_it() {
        let turn = Xform::Rotate {
            cx_nm: 0,
            cy_nm: 0,
            delta_deg: 45.0,
        };
        assert_eq!(
            turn.point(150_000_000, 111_386_035),
            (184_827_838, -27_304_197)
        );
        assert_eq!(
            turn.point(150_000_001, 134_005_573),
            (200_822_267, -11_309_769)
        );
    }

    #[test]
    fn a_board_text_box_turns_by_right_angles_only() {
        let turn = |delta_deg| Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0),
            cy_nm: builders::mm_to_nm(100.0),
            delta_deg,
        };
        let held = board::BoardTextBox {
            textbox: Some(common::TextBox {
                top_left: Some(builders::vec2(99.0, 99.0)),
                bottom_right: Some(builders::vec2(103.0, 101.0)),
                attributes: Some(common::TextAttributes {
                    angle: Some(common::Angle { value_degrees: 0.0 }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut tb = held.clone();
        assert!(transform_board_text_box(&mut tb, &turn(45.0)).is_err());
        assert_eq!(tb, held, "nothing moved before the refusal");

        transform_board_text_box(&mut tb, &turn(90.0)).unwrap();
        let textbox = tb.textbox.unwrap();
        assert_eq!(textbox.top_left.unwrap(), builders::vec2(99.0, 101.0));
        assert_eq!(textbox.bottom_right.unwrap(), builders::vec2(101.0, 97.0));
        assert_eq!(
            textbox.attributes.unwrap().angle.unwrap().value_degrees,
            90.0
        );
    }

    #[test]
    fn model_and_group_items_pass_through_untouched() {
        let model = builders::pack_any(
            &board::Footprint3DModel::default(),
            "kiapi.board.types.Footprint3DModel",
        );
        let original = model.value.clone();
        let mut fp = instance_with(vec![model]);

        let xf = Xform::Translate {
            dx_nm: builders::mm_to_nm(10.0),
            dy_nm: 0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();
        assert_eq!(fp.definition.unwrap().items[0].value, original);
    }
}
