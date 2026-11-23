//! Footprint creation and pad editing tools (`create_footprint`, `edit_footprint_pad`).

use super::footprint_drill_sexp;
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::footprint_kicad10;
use crate::tools::solder_margins;
use crate::tools::{get_path, require_array, require_str, ToolContext};
use copperpour_sexp::parser::{parse_sexp, SexpNode};
use copperpour_sexp::writer::{
    apply_edits, find_direct_child_blocks, read_consistent, write_atomic_if_unchanged, SexpEdit,
};
use serde_json::json;

/// Minimal pad geometry needed to derive outlines, courtyards, and pin 1.
#[derive(Debug, Clone)]
pub(super) struct PadGeom {
    pub(super) number: String,
    pub(super) pad_type: String,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
    /// Rotation in degrees, as the pad's `(at x y rotation)`.
    pub(super) rotation: f64,
}

impl PadGeom {
    /// The pad's axis-aligned extent `(min_x, min_y, max_x, max_y)`, its
    /// rotation included.
    pub(super) fn extent(&self) -> (f64, f64, f64, f64) {
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let half_x = (self.w / 2.0 * cos).abs() + (self.h / 2.0 * sin).abs();
        let half_y = (self.w / 2.0 * sin).abs() + (self.h / 2.0 * cos).abs();
        (
            self.x - half_x,
            self.y - half_y,
            self.x + half_x,
            self.y + half_y,
        )
    }
}

/// Axis-aligned bounding box `(min_x, min_y, max_x, max_y)` over pad extents.
pub(super) fn pads_bbox(pads: &[PadGeom]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in pads {
        let (x0, y0, x1, y1) = p.extent();
        min_x = min_x.min(x0);
        min_y = min_y.min(y0);
        max_x = max_x.max(x1);
        max_y = max_y.max(y1);
    }
    (min_x, min_y, max_x, max_y)
}

/// Courtyard clearance per the contributor's rule: an explicit value wins, else
/// `package_type`, else auto-detect (through-hole 0.5 mm, sub-0603 body 0.15 mm,
/// otherwise SMT 0.25 mm). BGA (1.0 mm) is opt-in via `package_type` because an
/// area array can't be reliably auto-detected from pads alone.
pub(super) fn courtyard_clearance(
    explicit: Option<f64>,
    package_type: Option<&str>,
    pads: &[PadGeom],
    body: Option<(f64, f64)>,
) -> f64 {
    if let Some(c) = explicit {
        return c;
    }
    match package_type {
        Some("bga") => return 1.0,
        Some("small") => return 0.15,
        Some("through_hole") | Some("th") => return 0.5,
        Some("smd") => return 0.25,
        _ => {}
    }
    if pads.iter().any(|p| p.pad_type.contains("thru")) {
        return 0.5;
    }
    if let Some((bw, bh)) = body {
        // 0603 imperial body is 1.6 x 0.8 mm; anything shorter is "smaller".
        if bw.max(bh) < 1.6 {
            return 0.15;
        }
    }
    0.25
}

/// Index of pin 1: the pad numbered "1", else the first pad. `None` if no pads.
pub(super) fn pin1_index(pads: &[PadGeom]) -> Option<usize> {
    if pads.is_empty() {
        return None;
    }
    Some(pads.iter().position(|p| p.number == "1").unwrap_or(0))
}

/// The rectangle corner (of the four) nearest point `(px, py)`.
fn nearest_corner(min_x: f64, min_y: f64, max_x: f64, max_y: f64, px: f64, py: f64) -> (f64, f64) {
    let cx = if (px - min_x).abs() <= (max_x - px).abs() {
        min_x
    } else {
        max_x
    };
    let cy = if (py - min_y).abs() <= (max_y - py).abs() {
        min_y
    } else {
        max_y
    };
    (cx, cy)
}

fn point_toward(from: (f64, f64), toward: (f64, f64), d: f64) -> (f64, f64) {
    let dx = toward.0 - from.0;
    let dy = toward.1 - from.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        return from;
    }
    (from.0 + dx / len * d, from.1 + dy / len * d)
}

/// Ordered vertices of a rectangle outline whose corner nearest `(px, py)` is
/// chamfered by `chamfer` mm (clamped to 40% of the shorter side) — the F.Fab
/// pin-1 marker. Clockwise, KiCAD footprint Y-down.
pub(super) fn chamfered_rect_points(
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    px: f64,
    py: f64,
    chamfer: f64,
) -> Vec<(f64, f64)> {
    let ch = chamfer
        .min(0.4 * (max_x - min_x).min(max_y - min_y))
        .max(0.0);
    let corners = [
        (min_x, min_y),
        (max_x, min_y),
        (max_x, max_y),
        (min_x, max_y),
    ];
    let (tcx, tcy) = nearest_corner(min_x, min_y, max_x, max_y, px, py);
    let mut pts = Vec::new();
    for (i, &(cx, cy)) in corners.iter().enumerate() {
        if (cx - tcx).abs() < 1e-9 && (cy - tcy).abs() < 1e-9 && ch > 0.0 {
            let prev = corners[(i + 3) % 4];
            let next = corners[(i + 1) % 4];
            pts.push(point_toward((cx, cy), prev, ch));
            pts.push(point_toward((cx, cy), next, ch));
        } else {
            pts.push((cx, cy));
        }
    }
    pts
}

/// The `(model ...)` block, as KiCad's footprint writer prints it, when a
/// `model` object with a non-empty `path` is present. The path is passed
/// through verbatim (absolute or a KiCad env-var path).
pub(super) fn build_model_sexp(args: &serde_json::Value) -> String {
    let model = match args.get("model") {
        Some(m) if m.is_object() => m,
        _ => return String::new(),
    };
    let path = match model["path"].as_str() {
        Some(p) if !p.is_empty() => p,
        _ => return String::new(),
    };
    let xyz = |key: &str, default: f64| -> String {
        let o = &model[key];
        ["x", "y", "z"]
            .map(|axis| footprint_kicad10::g10(o[axis].as_f64().unwrap_or(default)))
            .join(" ")
    };
    format!(
        "(model {} (offset (xyz {})) (scale (xyz {})) (rotate (xyz {})))",
        crate::tools::footprint_metadata::quote_string(path),
        xyz("offset", 0.0),
        xyz("scale", 1.0),
        xyz("rotate", 0.0)
    )
}

/// The layer names a pad's `layers` argument may hold, in `create_footprint`
/// and `edit_footprint_pad` alike (#710).
pub(super) const PAD_LAYER_NAMES: &[&str] = &[
    "F.Cu", "B.Cu", "F.Paste", "B.Paste", "F.Mask", "B.Mask", "*.Cu", "*.Mask",
];

/// The names of a pad's `layers` argument, checked against `PAD_LAYER_NAMES`:
/// strings only, at least one. Shared by `create_footprint` and
/// `edit_footprint_pad` (#710).
fn pad_layer_names(layers: &[serde_json::Value]) -> Result<Vec<&str>, String> {
    let mut names = Vec::with_capacity(layers.len());
    for layer in layers {
        let layer = layer
            .as_str()
            .ok_or_else(|| "pad layers must contain strings".to_string())?;
        if !PAD_LAYER_NAMES.contains(&layer) {
            return Err(format!("invalid pad layer: {layer}"));
        }
        names.push(layer);
    }
    if names.is_empty() {
        return Err("pad layers must not be empty".to_string());
    }
    Ok(names)
}

/// A pad's `(layers …)` as KiCad writes it in a footprint library
/// (`PCB_IO_KICAD_SEXPR::formatLayers`, `pcbnew/pcb_io/kicad_sexpr/
/// pcb_io_kicad_sexpr.cpp`, 10.0.8): `*.Cu` for front and back copper (a pad,
/// not a zone), `*.Paste` and `*.Mask` for both sides, then every remaining
/// layer in layer-ID order (`include/layer_ids.h`: F.Cu 0, F.Mask 1, B.Cu 2,
/// B.Mask 3, F.Paste 13, B.Paste 15). A name given twice is written once.
pub(super) fn kicad_pad_layers(names: &[&str]) -> String {
    let has = |name: &str| names.contains(&name);
    let all_copper = has("*.Cu") || (has("F.Cu") && has("B.Cu"));
    let all_paste = has("F.Paste") && has("B.Paste");
    let all_mask = has("*.Mask") || (has("F.Mask") && has("B.Mask"));
    let mut written = Vec::new();
    for (wildcard, set) in [
        ("*.Cu", all_copper),
        ("*.Paste", all_paste),
        ("*.Mask", all_mask),
    ] {
        if set {
            written.push(wildcard);
        }
    }
    for (layer, covered) in [
        ("F.Cu", all_copper),
        ("F.Mask", all_mask),
        ("B.Cu", all_copper),
        ("B.Mask", all_mask),
        ("F.Paste", all_paste),
        ("B.Paste", all_paste),
    ] {
        if has(layer) && !covered {
            written.push(layer);
        }
    }
    let quoted: Vec<String> = written.iter().map(|layer| format!("\"{layer}\"")).collect();
    format!("(layers {})", quoted.join(" "))
}

/// A pad's layers as written in its `(layers …)`.
fn written_pad_layers(pad: &SexpNode) -> Vec<String> {
    pad.find("layers")
        .and_then(SexpNode::children)
        .map(|children| {
            children[1..]
                .iter()
                .filter_map(SexpNode::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The silk line width KiCad's library conventions use, in mm.
pub(super) const SILK_WIDTH: f64 = 0.12;
/// How far the silk outline runs outside a given body, centre line to body
/// edge, in mm (KiCad library conventions, as asked on #704 and #711).
const SILK_BODY_OFFSET: f64 = 0.11;
/// The smallest gap between a silk line's edge and a pad's copper, in mm.
pub(super) const SILK_PAD_CLEARANCE: f64 = 0.2;

/// What `create_footprint` draws around its pads: the graphics, and where the
/// Reference and Value go.
pub(super) struct FootprintOutlines {
    pub(super) graphics: Vec<footprint_kicad10::Graphic>,
    pub(super) reference_at: (f64, f64),
    pub(super) value_at: (f64, f64),
}

/// The pieces of the line from `from` to `to` along one axis, at `across` on
/// the other, that keep `SILK_PAD_CLEARANCE` from every pad's copper; pieces
/// shorter than a line's width are dropped.
pub(super) fn silk_pieces(
    pads: &[PadGeom],
    horizontal: bool,
    across: f64,
    from: f64,
    to: f64,
) -> Vec<(f64, f64)> {
    let keep_out = SILK_PAD_CLEARANCE + SILK_WIDTH / 2.0;
    let mut pieces = vec![(from, to)];
    for pad in pads {
        let (x0, y0, x1, y1) = pad.extent();
        let (along, side) = if horizontal {
            ((x0, x1), (y0, y1))
        } else {
            ((y0, y1), (x0, x1))
        };
        if across <= side.0 - keep_out || across >= side.1 + keep_out {
            continue;
        }
        let (cut_from, cut_to) = (along.0 - keep_out, along.1 + keep_out);
        pieces = pieces
            .into_iter()
            .flat_map(|(start, end)| {
                [(start, end.min(cut_from)), (start.max(cut_to), end)]
                    .into_iter()
                    .filter(|(a, b)| b > a)
            })
            .collect();
    }
    pieces.retain(|(start, end)| end - start >= SILK_WIDTH);
    pieces
}

/// Build the courtyard, silkscreen, fab outline and pin-1 marker (silk dot +
/// fab chamfer) for a footprint from its pads and, when given, its body (#711):
/// with a body, the courtyard goes around the pads and the body, and the silk
/// runs `SILK_BODY_OFFSET` outside the body, broken where it would come closer
/// than `SILK_PAD_CLEARANCE` to a pad. Without one, both follow the pad envelope.
pub(super) fn build_footprint_graphics(
    args: &serde_json::Value,
    pads: &[PadGeom],
) -> FootprintOutlines {
    use footprint_kicad10::{Graphic, Shape};
    let (pmin_x, pmin_y, pmax_x, pmax_y) = pads_bbox(pads);

    let body = match (args["body_width"].as_f64(), args["body_height"].as_f64()) {
        (Some(bw), Some(bh)) => Some((bw, bh)),
        _ => None,
    };
    let clearance = courtyard_clearance(
        args["courtyard_clearance"].as_f64(),
        args["package_type"].as_str(),
        pads,
        body,
    );
    // The body is centred on the pads.
    let (cx, cy) = ((pmin_x + pmax_x) / 2.0, (pmin_y + pmax_y) / 2.0);
    let body_box =
        body.map(|(bw, bh)| (cx - bw / 2.0, cy - bh / 2.0, cx + bw / 2.0, cy + bh / 2.0));

    // Courtyard: around the pads and the body, plus the clearance.
    let (umin_x, umin_y, umax_x, umax_y) = match body_box {
        Some((bx0, by0, bx1, by1)) => (
            pmin_x.min(bx0),
            pmin_y.min(by0),
            pmax_x.max(bx1),
            pmax_y.max(by1),
        ),
        None => (pmin_x, pmin_y, pmax_x, pmax_y),
    };
    let (cmin_x, cmin_y, cmax_x, cmax_y) = (
        umin_x - clearance,
        umin_y - clearance,
        umax_x + clearance,
        umax_y + clearance,
    );

    let line = |shape, layer, width| Graphic {
        shape,
        layer,
        width,
        filled: false,
    };
    let mut graphics = vec![line(
        Shape::Rect {
            start: (cmin_x, cmin_y),
            end: (cmax_x, cmax_y),
        },
        "F.CrtYd",
        0.05,
    )];

    // Silk: outside the body, broken near pads; without a body, just outside
    // the pad envelope so it clears them.
    let (smin_x, smin_y, smax_x, smax_y) = match body_box {
        Some((bx0, by0, bx1, by1)) => {
            let outline = (
                bx0 - SILK_BODY_OFFSET,
                by0 - SILK_BODY_OFFSET,
                bx1 + SILK_BODY_OFFSET,
                by1 + SILK_BODY_OFFSET,
            );
            let (x0, y0, x1, y1) = outline;
            for (horizontal, across, from, to) in [
                (true, y0, x0, x1),
                (true, y1, x0, x1),
                (false, x0, y0, y1),
                (false, x1, y0, y1),
            ] {
                for (start, end) in silk_pieces(pads, horizontal, across, from, to) {
                    let (start, end) = if horizontal {
                        ((start, across), (end, across))
                    } else {
                        ((across, start), (across, end))
                    };
                    graphics.push(line(Shape::Line { start, end }, "F.SilkS", SILK_WIDTH));
                }
            }
            outline
        }
        None => {
            let silk_margin = 0.15;
            let outline = (
                pmin_x - silk_margin,
                pmin_y - silk_margin,
                pmax_x + silk_margin,
                pmax_y + silk_margin,
            );
            graphics.push(line(
                Shape::Rect {
                    start: (outline.0, outline.1),
                    end: (outline.2, outline.3),
                },
                "F.SilkS",
                SILK_WIDTH,
            ));
            outline
        }
    };

    // Fab: the component body when given, else the pad envelope. May overlap
    // pads — fab is a documentation layer, not subject to silk-over-pad rules.
    let (fmin_x, fmin_y, fmax_x, fmax_y) = body_box.unwrap_or((pmin_x, pmin_y, pmax_x, pmax_y));

    if let Some(i1) = pin1_index(pads) {
        let p1 = &pads[i1];

        // Fab outline with the pin-1 corner chamfered.
        let chamfer = (0.25 * (fmax_x - fmin_x).min(fmax_y - fmin_y)).clamp(0.3, 1.0);
        let points = chamfered_rect_points(fmin_x, fmin_y, fmax_x, fmax_y, p1.x, p1.y, chamfer);
        graphics.push(line(Shape::Poly { points }, "F.Fab", 0.1));

        // Silk pin-1 dot just outside the silk outline, aligned with pin 1's
        // pad — NOT at the footprint corner, where a dot is ambiguous between
        // pin 1 and the last pin that shares the same corner. It sits directly
        // beside pin 1 so the mark is unmistakable.
        let (dx, dy) = if (p1.x - cx).abs() >= (p1.y - cy).abs() {
            // Pin 1 is on a left/right edge: dot outside that edge, at pin 1's y.
            let sign = if p1.x < cx { -1.0 } else { 1.0 };
            let edge = if sign < 0.0 { smin_x } else { smax_x };
            (edge + sign * 0.4, p1.y)
        } else {
            // Pin 1 is on a top/bottom edge: dot outside that edge, at pin 1's x.
            let sign = if p1.y < cy { -1.0 } else { 1.0 };
            let edge = if sign < 0.0 { smin_y } else { smax_y };
            (p1.x, edge + sign * 0.4)
        };
        graphics.push(Graphic {
            shape: Shape::Circle {
                center: (dx, dy),
                end: (dx + 0.15, dy),
            },
            layer: "F.SilkS",
            width: 0.1,
            filled: true,
        });
    } else {
        // No pads to mark pin 1 against — plain fab rectangle.
        graphics.push(line(
            Shape::Rect {
                start: (fmin_x, fmin_y),
                end: (fmax_x, fmax_y),
            },
            "F.Fab",
            0.1,
        ));
    }

    // Reference above the courtyard (F.SilkS), value below it (F.Fab).
    FootprintOutlines {
        graphics,
        reference_at: (cx, cmin_y - 1.0),
        value_at: (cx, cmax_y + 1.0),
    }
}

pub(crate) async fn handle_create_footprint(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let output = get_path(args, "output")?;
    // Both are schema-required. Defaulting them meant a call with neither
    // wrote `(footprint "Footprint" …)` with no pads, no courtyard, no
    // silkscreen and no fab outline — through `write_atomic`, which replaces
    // unconditionally — over whatever `.kicad_mod` was already at `output`,
    // and returned `{"success": true, "pad_count": 0}` (#218).
    let name = match require_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let description = args["description"].as_str().unwrap_or("");

    let replace_existing = match args.get("replace_existing") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => match value.as_bool() {
            Some(value) => value,
            None => {
                return Ok(invalid_library_argument(
                    "replace_existing",
                    "must be a boolean when supplied",
                ))
            }
        },
    };

    let pads_val = match require_array(args, "pads") {
        Ok(v) => v.clone(),
        Err(e) => return Ok(e),
    };
    if let Err(error) = validate_footprint_pad_items(&pads_val) {
        return Ok(error);
    }

    if !replace_existing && output.exists() {
        let shown = copperpour_sexp::paths::display_path(&output);
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::Conflict {
                paths: vec![shown.clone()],
            },
            format!(
                "Footprint '{shown}' already exists; pass replace_existing=true to replace it, \
                 or choose a different output path."
            ),
        ));
    }
    let read = if replace_existing && output.exists() {
        Some(read_consistent(&output)?)
    } else {
        None
    };
    // Each pad as KiCad's writer prints it, before its UUID, with what
    // `FOOTPRINT::cmp_pads` sorts by.
    struct WrittenPad {
        geometry: PadGeom,
        shape_order: u8,
        sexp: String,
    }
    let mut pads: Vec<WrittenPad> = Vec::new();
    let mut warnings = Vec::new();
    for pad in &pads_val {
        // Safe after validate_footprint_pad_items: every schema-required field
        // has the declared type, so no plausible-looking pad is invented.
        let number = pad["number"].as_str().expect("validated").to_string();
        let pad_type = pad["type"].as_str().expect("validated").to_string();
        let shape = pad["shape"].as_str().expect("validated");
        let x = pad["x"].as_f64().expect("validated");
        let y = pad["y"].as_f64().expect("validated");
        let w = pad["width"].as_f64().expect("validated");
        let h = pad["height"].as_f64().expect("validated");

        let layer_names = if let Some(layers) = pad["layers"].as_array() {
            pad_layer_names(layers).map_err(anyhow::Error::msg)?
        } else if pad_type == "smd" {
            vec!["F.Cu", "F.Paste", "F.Mask"]
        } else {
            vec!["*.Cu", "*.Mask"]
        };
        let layers = kicad_pad_layers(&layer_names);

        // KiCad keeps a pad's orientation in [0, 360) (`EDA_ANGLE::Normalize`).
        let rotation = pad["rotation"].as_f64().unwrap_or(0.0).rem_euclid(360.0);
        let at = if rotation == 0.0 {
            format!(
                "(at {} {})",
                footprint_kicad10::mm(x),
                footprint_kicad10::mm(y)
            )
        } else {
            format!(
                "(at {} {} {})",
                footprint_kicad10::mm(x),
                footprint_kicad10::mm(y),
                footprint_kicad10::g10(rotation)
            )
        };

        let roundrect_ratio = if let Some(ratio) = pad["roundrect_rratio"].as_f64() {
            if !(0.0..=0.5).contains(&ratio) {
                anyhow::bail!("roundrect_rratio must be between 0 and 0.5");
            }
            format!("(roundrect_rratio {})", footprint_kicad10::g10(ratio))
        } else {
            String::new()
        };

        let drill_sexp = if let Some(drill) = pad.get("drill") {
            footprint_drill_sexp(drill, &pad_type, w, h).expect("validated drill")
        } else {
            String::new()
        };
        let custom_shape = match pad.get("custom_shape").filter(|value| !value.is_null()) {
            Some(value) => {
                let custom = crate::tools::pad_primitives::CustomPadShape::from_json(value)
                    .expect("validated custom shape");
                format!(
                    "{} {}",
                    crate::tools::pad_primitives::inline(&custom.options_sexp(0)),
                    crate::tools::pad_primitives::inline(&custom.primitives_sexp(0))
                )
            }
            None => String::new(),
        };

        let fabrication_property = pad["fabrication_property"]
            .as_str()
            .map(|name| PadFabricationProperty::parse(name).expect("validated"));
        // A round drill is a bare diameter; an oval one is an object.
        let round_hole = pad["drill"].as_f64().is_some_and(|drill| drill > 0.0);
        if let Some(warning) = fabrication_property
            .and_then(|property| property.padstack_warning(&number, &pad_type, round_hole))
        {
            warnings.push(warning);
        }
        let property = fabrication_property
            .and_then(PadFabricationProperty::token)
            .map_or_else(String::new, |token| format!("(property {token})"));
        // KiCad writes it for a plated hole (`PCB_IO_KICAD_SEXPR::format( const PAD* )`).
        let unused_layers = if pad_type == "thru_hole" {
            "(remove_unused_layers no)"
        } else {
            ""
        };

        // The children in the order KiCad's pad writer prints them.
        let sexp = format!(
            "(pad {} {} {} {} (size {} {}) {} {} {} {} {} {}",
            crate::tools::footprint_metadata::quote_string(&number),
            pad_type,
            shape,
            at,
            footprint_kicad10::mm(w),
            footprint_kicad10::mm(h),
            drill_sexp,
            property,
            layers,
            unused_layers,
            roundrect_ratio,
            custom_shape
        );
        // KiCad's `PAD_SHAPE` order (`pcbnew/padstack.h`).
        let shape_order = match shape {
            "circle" => 0,
            "rect" => 1,
            "oval" => 2,
            "roundrect" => 4,
            _ => 6,
        };
        pads.push(WrittenPad {
            geometry: PadGeom {
                number,
                pad_type,
                x,
                y,
                w,
                h,
                rotation,
            },
            shape_order,
            sexp,
        });
    }
    let pad_geoms: Vec<PadGeom> = pads.iter().map(|pad| pad.geometry.clone()).collect();

    // Courtyard, silk, fab and pin-1 marker, derived from the pads and body.
    let outlines = (!pad_geoms.is_empty()).then(|| build_footprint_graphics(args, &pad_geoms));
    let model_sexp = build_model_sexp(args);

    let attr = if pad_geoms.iter().any(|p| p.pad_type == "smd") {
        "smd"
    } else {
        "through_hole"
    };

    // The footprint in the order KiCad's writer prints it
    // (`PCB_IO_KICAD_SEXPR::format( const FOOTPRINT* )`), graphics and pads
    // sorted as `FOOTPRINT::cmp_drawings` and `cmp_pads` sort them, then laid
    // out by its `Prettify` pass (#711).
    let quote = crate::tools::footprint_metadata::quote_string;
    let mut text = format!(
        "(footprint {} (version {}) (generator \"copperpour\") (generator_version \"10.0\") \
         (layer \"F.Cu\")",
        quote(name),
        footprint_kicad10::FOOTPRINT_VERSION
    );
    if !description.is_empty() {
        text.push_str(&format!(" (descr {})", quote(description)));
    }
    let (reference_at, value_at) = outlines
        .as_ref()
        .map_or(((0.0, 0.0), (0.0, 0.0)), |o| (o.reference_at, o.value_at));
    let text_font = "(size 1 1) (thickness 0.15)";
    let hidden_font = "(size 1.27 1.27)";
    for field in [
        footprint_kicad10::field_sexp(
            "Reference",
            "REF**",
            reference_at,
            "F.SilkS",
            false,
            text_font,
        ),
        footprint_kicad10::field_sexp("Value", name, value_at, "F.Fab", false, text_font),
        footprint_kicad10::field_sexp("Datasheet", "", (0.0, 0.0), "F.Fab", true, hidden_font),
        footprint_kicad10::field_sexp("Description", "", (0.0, 0.0), "F.Fab", true, hidden_font),
    ] {
        text.push(' ');
        text.push_str(&field);
    }
    text.push_str(&format!(
        " (attr {attr}) (duplicate_pad_numbers_are_jumpers no)"
    ));
    let mut graphics = outlines.map(|o| o.graphics).unwrap_or_default();
    graphics.sort_by(|a, b| a.kicad_order(b));
    for graphic in &graphics {
        text.push(' ');
        text.push_str(&graphic.sexp(&footprint_kicad10::new_uuid()));
    }
    let nm = footprint_kicad10::nanometres;
    pads.sort_by(|a, b| {
        let (pa, pb) = (&a.geometry, &b.geometry);
        footprint_kicad10::str_num_cmp(&pa.number, &pb.number)
            .then((nm(pa.x), nm(pa.y)).cmp(&(nm(pb.x), nm(pb.y))))
            .then((nm(pa.w), nm(pa.h)).cmp(&(nm(pb.w), nm(pb.h))))
            .then(a.shape_order.cmp(&b.shape_order))
    });
    for pad in &pads {
        text.push_str(&format!(
            " {} (uuid \"{}\"))",
            pad.sexp,
            footprint_kicad10::new_uuid()
        ));
    }
    text.push_str(" (embedded_fonts no) ");
    text.push_str(&model_sexp);
    text.push(')');
    let content = copperpour_sexp::prettify(&text);

    // Ensure parent directory exists
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(refusal) = crate::tools::write_if_unchanged(
        &output,
        "footprint",
        "create_footprint",
        read.as_deref(),
        content,
    )? {
        return Ok(refusal);
    }

    let mut response = json!({
        "success": true,
        "footprint": name,
        "output": output.to_str().unwrap_or(""),
        "pad_count": pad_geoms.len(),
        "courtyard": !pad_geoms.is_empty(),
        "pin1_marked": !pad_geoms.is_empty(),
        "model": args.get("model").and_then(|m| m["path"].as_str()).unwrap_or("")
    });
    if !warnings.is_empty() {
        response["warnings"] = json!(warnings);
    }
    Ok(CallToolResult::text(
        serde_json::to_string(&response).unwrap(),
    ))
}

/// Enforce the nested `pads.items.required` schema before any output file is
/// created or replaced. Top-level MCP validation is deliberately shallow, so
/// handlers that own nested objects must preserve this invariant themselves.
fn validate_footprint_pad_items(pads: &[serde_json::Value]) -> Result<(), CallToolResult> {
    for (index, pad) in pads.iter().enumerate() {
        if !pad.is_object() {
            return Err(invalid_library_argument(
                &format!("pads[{index}]"),
                "must be an object",
            ));
        }
        for field in ["number", "type", "shape"] {
            if pad[field].as_str().is_none() {
                return Err(invalid_library_argument(
                    &format!("pads[{index}].{field}"),
                    "missing or not a string",
                ));
            }
        }
        for field in ["x", "y", "width", "height"] {
            if pad[field].as_f64().is_none() {
                return Err(invalid_library_argument(
                    &format!("pads[{index}].{field}"),
                    "missing or not a number",
                ));
            }
        }
        if let Some(drill) = pad.get("drill") {
            footprint_drill_sexp(
                drill,
                pad["type"].as_str().expect("validated"),
                pad["width"].as_f64().expect("validated"),
                pad["height"].as_f64().expect("validated"),
            )
            .map_err(|reason| invalid_library_argument(&format!("pads[{index}].drill"), reason))?;
        }
        let shape = pad["shape"].as_str().expect("validated");
        match (
            shape,
            pad.get("custom_shape").filter(|value| !value.is_null()),
        ) {
            ("custom", None) => {
                return Err(invalid_library_argument(
                    &format!("pads[{index}].custom_shape"),
                    "shape 'custom' needs custom_shape, the pad's anchor and primitives",
                ))
            }
            ("custom", Some(value)) => {
                if let Err((field, reason)) =
                    crate::tools::pad_primitives::CustomPadShape::from_json(value)
                {
                    let field = if field.is_empty() {
                        format!("pads[{index}].custom_shape")
                    } else {
                        format!("pads[{index}].custom_shape.{field}")
                    };
                    return Err(invalid_library_argument(&field, reason));
                }
            }
            (other, Some(_)) => {
                return Err(invalid_library_argument(
                    &format!("pads[{index}].custom_shape"),
                    format!("applies to a custom pad, but shape is '{other}'"),
                ))
            }
            _ => {}
        }
        if let Some(value) = pad
            .get("fabrication_property")
            .filter(|value| !value.is_null())
        {
            if let Err(reason) = PadFabricationProperty::from_argument(value) {
                return Err(invalid_library_argument(
                    &format!("pads[{index}].fabrication_property"),
                    reason,
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn handle_edit_footprint_pad(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "footprint_path")?;
    let pad_number = match require_str(args, "pad_number") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let new_number = match args.get("new_number") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match value.as_str() {
            Some(number) => Some(number),
            None => {
                return Ok(invalid_library_argument(
                    "new_number",
                    "must be a string when supplied",
                ))
            }
        },
    };
    let match_all = match args.get("match_all") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => match value.as_bool() {
            Some(match_all) => match_all,
            None => {
                return Ok(invalid_library_argument(
                    "match_all",
                    "must be a boolean when supplied",
                ))
            }
        },
    };
    let shape = match args.get("shape") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match value.as_str() {
            Some(shape)
                if matches!(shape, "circle" | "rect" | "oval" | "roundrect" | "custom") =>
            {
                Some(shape)
            }
            Some(shape) => {
                return Ok(invalid_library_argument(
                    "shape",
                    format!(
                        "unsupported shape '{shape}'; expected circle, rect, oval, roundrect, or custom"
                    ),
                ))
            }
            None => {
                return Ok(invalid_library_argument(
                    "shape",
                    "must be a string when supplied",
                ))
            }
        },
    };
    // Omission leaves the pad as it is; null is not a value the schema accepts, so it is refused here too.
    let zone_connect = match args.get("zone_connect") {
        None => None,
        Some(value) => match value.as_str() {
            Some(name) => match PadZoneConnect::parse(name) {
                Some(zone_connect) => Some(zone_connect),
                None => {
                    return Ok(invalid_library_argument(
                        "zone_connect",
                        format!(
                            "unsupported zone connection '{name}'; expected inherited, solid, thermal, or none"
                        ),
                    ))
                }
            },
            None => {
                return Ok(invalid_library_argument(
                    "zone_connect",
                    "must be a string when supplied",
                ))
            }
        },
    };

    let fabrication_property = match args
        .get("fabrication_property")
        .map(PadFabricationProperty::from_argument)
        .transpose()
    {
        Ok(property) => property,
        Err(reason) => return Ok(invalid_library_argument("fabrication_property", reason)),
    };
    let margins = match solder_margins::requested(args) {
        Ok(margins) => margins,
        Err(refusal) => return Ok(invalid_library_argument(refusal.field, refusal.reason)),
    };
    let roundrect_rratio = match args.get("roundrect_rratio").map(corner_ratio).transpose() {
        Ok(ratio) => ratio,
        Err(reason) => return Ok(invalid_library_argument("roundrect_rratio", reason)),
    };
    let layers = match args.get("layers") {
        None => None,
        Some(value) => match value.as_array().map(|layers| pad_layer_names(layers)) {
            Some(Ok(names)) => Some(kicad_pad_layers(&names)),
            Some(Err(reason)) => return Ok(invalid_library_argument("layers", reason)),
            None => {
                return Ok(invalid_library_argument(
                    "layers",
                    "must be an array of layer names when supplied",
                ))
            }
        },
    };
    let custom_shape = match args
        .get("custom_shape")
        .map(crate::tools::pad_primitives::CustomPadShape::from_json)
        .transpose()
    {
        Ok(custom_shape) => custom_shape,
        Err((field, reason)) => {
            let field = if field.is_empty() {
                "custom_shape".to_string()
            } else {
                format!("custom_shape.{field}")
            };
            return Ok(invalid_library_argument(&field, reason));
        }
    };
    match (shape, &custom_shape) {
        (Some("custom"), None) => {
            return Ok(invalid_library_argument(
                "custom_shape",
                "shape 'custom' needs custom_shape, the pad's anchor and primitives",
            ))
        }
        (Some(shape), Some(_)) if shape != "custom" => {
            return Ok(invalid_library_argument(
                "custom_shape",
                format!("applies to a custom pad, but shape is '{shape}'"),
            ))
        }
        _ => {}
    }
    if let (Some(shape), Some(_)) = (shape, roundrect_rratio) {
        if shape != "roundrect" {
            return Ok(invalid_library_argument(
                "roundrect_rratio",
                format!("applies to a roundrect pad, but shape is '{shape}'"),
            ));
        }
    }

    let content = read_consistent(&path)?;
    let file_version = match parse_sexp(&content) {
        Ok(root) if root.head() == Some("footprint") => solder_margins::file_version(&root),
        Ok(root) => {
            return Ok(invalid_library_argument(
                "footprint_path",
                crate::tools::footprint_root_reason(root.head()),
            ))
        }
        Err(error) => {
            return Ok(invalid_library_argument(
                "footprint_path",
                format!("footprint is malformed: {error}"),
            ))
        }
    };
    if let Err(refusal) = solder_margins::refuse_zero_in_older_file(file_version, &margins) {
        return Ok(invalid_library_argument(refusal.field, refusal.reason));
    }

    let mut edits = Vec::new();
    let mut matched_count = 0usize;
    let mut observed_margins = vec![Vec::new(); margins.len()];
    let mut observed_zone_connects = Vec::new();
    let mut observed_fabrication_properties = Vec::new();
    let mut observed_ratios = Vec::new();
    let mut observed_custom_shapes = Vec::new();
    let mut observed_layers = Vec::new();
    let mut warnings = Vec::new();
    for (start, end) in find_direct_child_blocks(&content, "footprint") {
        let block = &content[start..end];
        let Ok(node) = parse_sexp(block) else {
            return Ok(invalid_library_argument(
                "footprint_path",
                "footprint contains a malformed direct child",
            ));
        };
        if node.head() != Some("pad") || node.get(1).and_then(SexpNode::as_str) != Some(pad_number)
        {
            continue;
        }

        matched_count += 1;
        // Without a new shape, the ratio and the custom shape need a pad that
        // already has that shape: KiCad writes the ratio only for roundrect
        // (and chamfered) pads, and primitives only for custom ones.
        let current_shape = node.get(3).and_then(SexpNode::as_str).unwrap_or("?");
        if shape.is_none() && roundrect_rratio.is_some() && current_shape != "roundrect" {
            return Ok(invalid_library_argument(
                "roundrect_rratio",
                format!(
                    "pad '{pad_number}' is {current_shape}, not roundrect; pass shape 'roundrect' to change it"
                ),
            ));
        }
        if shape.is_none() && custom_shape.is_some() && current_shape != "custom" {
            return Ok(invalid_library_argument(
                "custom_shape",
                format!(
                    "pad '{pad_number}' is {current_shape}, not custom; pass shape 'custom' to change it"
                ),
            ));
        }
        let mut edited = match edit_footprint_pad_block(block, args, new_number, shape) {
            Ok(edited) => edited,
            Err(reason) => return Ok(invalid_library_argument("shape", reason)),
        };
        if let Some(custom) = &custom_shape {
            edited = set_pad_custom_shape(edited, custom);
            let written = parse_sexp(&edited)?;
            let read_back = crate::tools::pad_primitives::CustomPadShape::from_pad(&written)
                .map_err(|reason| {
                    anyhow::anyhow!("the custom shape does not read back: {reason}")
                })?;
            anyhow::ensure!(
                &read_back == custom,
                "the custom shape reads back differently from what was asked"
            );
            push_distinct(&mut observed_custom_shapes, read_back.to_json());
        }
        if let Some(ratio) = roundrect_rratio {
            edited = set_pad_child(
                edited,
                "roundrect_rratio",
                Some(format!("(roundrect_rratio {})", format_ratio(ratio))),
            );
            let written = parse_sexp(&edited)?;
            let ratio_of = |pad: &SexpNode| {
                pad.find("roundrect_rratio")
                    .and_then(|ratio| ratio.get_f64(1))
            };
            push_distinct(
                &mut observed_ratios,
                json!({ "previous": ratio_of(&node), "current": ratio_of(&written) }),
            );
        }
        if let Some(layers) = &layers {
            // Replaced in place, so the pad keeps its UUID and every other child.
            edited = set_pad_child(edited, "layers", Some(layers.clone()));
            let written = parse_sexp(&edited)?;
            push_distinct(
                &mut observed_layers,
                json!({
                    "previous": written_pad_layers(&node),
                    "current": written_pad_layers(&written)
                }),
            );
        }
        if let Some(zone_connect) = zone_connect {
            edited = set_pad_zone_connect(edited, zone_connect);
            observed_zone_connects.push(
                PadZoneConnect::of_pad(&edited).map_or("unrecognized", PadZoneConnect::as_str),
            );
        }
        if let Some(property) = fabrication_property {
            edited = set_pad_fabrication_property(edited, property);
            // Judge the pad as written: this call may also change its number or drill.
            let written = parse_sexp(&edited).unwrap_or_else(|_| node.clone());
            let observed = PadFabricationProperty::of_pad(&written);
            let header = |index| {
                written
                    .get(index)
                    .and_then(SexpNode::as_str)
                    .unwrap_or_default()
            };
            if let Some(warning) =
                observed.padstack_warning(header(1), header(2), pad_has_round_hole(&written))
            {
                push_distinct(&mut warnings, warning);
            }
            observed_fabrication_properties.push(observed.as_str());
        }
        for ((margin, setting), observed) in margins.iter().zip(&mut observed_margins) {
            edited = set_pad_child(edited, margin.name(), setting.child(*margin));
            // Both sides read as KiCad reads them, `current` off the pad as written.
            let written = parse_sexp(&edited).unwrap_or_else(|_| node.clone());
            let read =
                |pad: &SexpNode| solder_margins::read(pad, *margin, &[margin.name()], file_version);
            observed.push(json!({ "previous": read(&node), "current": read(&written) }));
        }
        edits.push(SexpEdit::replace(start, end, edited));
        if !match_all {
            break;
        }
    }

    if matched_count == 0 {
        return Ok(invalid_library_argument(
            "pad_number",
            format!("pad '{pad_number}' was not found"),
        ));
    }

    let new_content = apply_edits(content.clone(), edits);
    write_atomic_if_unchanged(&path, &content, &new_content)?;

    let mut response = json!({
        "success": true,
        "pad": pad_number,
        "shape": shape,
        "matched_count": matched_count,
        "updated_count": matched_count
    });
    // Read off the pads as written, not echoed from the request.
    report_observed(&mut response, "zone_connect", observed_zone_connects);
    report_observed(
        &mut response,
        "fabrication_property",
        observed_fabrication_properties,
    );
    for ((margin, _), observed) in margins.iter().zip(observed_margins) {
        solder_margins::report(&mut response, *margin, observed);
    }
    for (key, observed) in [
        ("roundrect_rratio", observed_ratios),
        ("custom_shape", observed_custom_shapes),
        ("layers", observed_layers),
    ] {
        match observed.as_slice() {
            [] => {}
            [one] => response[key] = one.clone(),
            several => response[key] = json!(several),
        }
    }
    if !warnings.is_empty() {
        response["warnings"] = json!(warnings);
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&response).unwrap(),
    ))
}

/// Report one value when every edited pad agrees, or each distinct value.
pub(super) fn report_observed(response: &mut serde_json::Value, key: &str, observed: Vec<&str>) {
    let mut distinct = Vec::new();
    for value in observed {
        push_distinct(&mut distinct, value);
    }
    match distinct.as_slice() {
        [] => {}
        [observed] => response[key] = json!(observed),
        disagreeing => response[key] = json!(disagreeing),
    }
}

fn push_distinct<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// A corner ratio argument: a fraction of the pad's smaller side, 0..0.5, as
/// KiCad's `roundrect_rratio` and `chamfer_ratio` are (#107 can reuse it).
fn corner_ratio(value: &serde_json::Value) -> Result<f64, String> {
    let ratio = value
        .as_f64()
        .ok_or_else(|| "must be a number when supplied".to_string())?;
    if !(0.0..=0.5).contains(&ratio) {
        return Err(format!("{ratio} is outside 0..0.5"));
    }
    Ok(ratio)
}

/// A ratio as KiCad's writer formats it (`FormatDouble2Str`,
/// `common/string_utils.cpp`, 10.0.8): ten significant digits, or sixteen
/// decimals at or below 0.0001, trailing zeros removed.
pub(super) fn format_ratio(ratio: f64) -> String {
    if ratio == 0.0 {
        return "0".to_string();
    }
    let text = if ratio.abs() <= 0.0001 {
        format!("{ratio:.16}")
    } else {
        let integer_digits = ratio.abs().log10().floor() as i32 + 1;
        let decimals = (10 - integer_digits).max(0) as usize;
        format!("{ratio:.decimals$}")
    };
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// Give a pad block `custom`'s `(options …)` and `(primitives …)`, in
/// KiCad's order and layout: on their own lines at the pad's indentation and
/// line ending, or inline for a pad written on one line.
fn set_pad_custom_shape(
    pad_block: String,
    custom: &crate::tools::pad_primitives::CustomPadShape,
) -> String {
    let separator = find_direct_child_blocks(&pad_block, "pad")
        .first()
        .map(|&(start, _)| {
            pad_block[leading_whitespace_start(&pad_block, start)..start].to_string()
        })
        .unwrap_or_default();
    let (options, primitives) = match separator.rfind('\n') {
        Some(newline) => {
            let indent = &separator[newline + 1..];
            let depth = if indent.chars().all(|c| c == '\t') {
                indent.len()
            } else {
                2
            };
            let line_ending = if pad_block.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            (
                custom.options_sexp(depth).replace('\n', line_ending),
                custom.primitives_sexp(depth).replace('\n', line_ending),
            )
        }
        None => (
            crate::tools::pad_primitives::inline(&custom.options_sexp(0)),
            crate::tools::pad_primitives::inline(&custom.primitives_sexp(0)),
        ),
    };
    let pad_block = set_pad_child(pad_block, "options", Some(options));
    set_pad_child(pad_block, "primitives", Some(primitives))
}

/// A pad's `(zone_connect N)` override. The numbers are KiCad's
/// `ZONE_CONNECTION` values; `Inherited` is written by omitting the token.
/// KiCad's pad dialog offers these four and not `THT_THERMAL` (3), which is a
/// zone setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PadZoneConnect {
    Inherited,
    None,
    Thermal,
    Solid,
}

impl PadZoneConnect {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "inherited" => Some(Self::Inherited),
            "none" => Some(Self::None),
            "thermal" => Some(Self::Thermal),
            "solid" => Some(Self::Solid),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Inherited => "inherited",
            Self::None => "none",
            Self::Thermal => "thermal",
            Self::Solid => "solid",
        }
    }

    fn token(self) -> Option<u8> {
        match self {
            Self::Inherited => None,
            Self::None => Some(0),
            Self::Thermal => Some(1),
            Self::Solid => Some(2),
        }
    }

    /// The connection a pad block carries, or `None` for a value this enum
    /// cannot name (such as KiCad's `3`).
    fn of_pad(pad_block: &str) -> Option<Self> {
        let children = pad_children(pad_block, "zone_connect");
        let Some(&(start, end)) = children.first() else {
            return Some(Self::Inherited);
        };
        let node = parse_sexp(&pad_block[start..end]).ok()?;
        match node.get(1).and_then(SexpNode::as_str)? {
            "0" => Some(Self::None),
            "1" => Some(Self::Thermal),
            "2" => Some(Self::Solid),
            _ => None,
        }
    }
}

/// Direct children of a pad block whose head is `head`.
fn pad_children(pad_block: &str, head: &str) -> Vec<(usize, usize)> {
    find_direct_child_blocks(pad_block, "pad")
        .into_iter()
        .filter(|&(start, end)| {
            parse_sexp(&pad_block[start..end]).is_ok_and(|child| child.head() == Some(head))
        })
        .collect()
}

/// Whitespace immediately before `index`, so a child can be added or removed
/// with the indentation and line endings its siblings already use.
fn leading_whitespace_start(source: &str, index: usize) -> usize {
    source[..index]
        .trim_end_matches(|c: char| c.is_ascii_whitespace())
        .len()
}

/// The pad children `set_pad_child` places, in the order KiCad writes them
/// (`PCB_IO_KICAD_SEXPR::format( const PAD* )`). A new child goes before the
/// first existing child listed after it, which is where KiCad would write it.
const PAD_CHILD_ORDER: &[&str] = &[
    "property",
    "layers",
    "remove_unused_layers",
    "keep_end_layers",
    "zone_layer_connections",
    "roundrect_rratio",
    "chamfer_ratio",
    "chamfer",
    "net",
    "pinfunction",
    "pintype",
    "die_length",
    "solder_mask_margin",
    "solder_paste_margin",
    "solder_paste_margin_ratio",
    "clearance",
    "zone_connect",
    "thermal_bridge_width",
    "thermal_bridge_angle",
    "thermal_gap",
    "options",
    "primitives",
    "padstack",
    "teardrops",
    "uuid",
];

pub(super) fn set_pad_zone_connect(pad_block: String, zone_connect: PadZoneConnect) -> String {
    set_pad_child(
        pad_block,
        "zone_connect",
        zone_connect
            .token()
            .map(|token| format!("(zone_connect {token})")),
    )
}

pub(super) fn set_pad_fabrication_property(
    pad_block: String,
    property: PadFabricationProperty,
) -> String {
    set_pad_child(
        pad_block,
        "property",
        property.token().map(|token| format!("(property {token})")),
    )
}

/// Make `child` the pad's only `head` child, or remove every `head` child
/// when it is `None`. `head` must be in `PAD_CHILD_ORDER`.
fn set_pad_child(pad_block: String, head: &str, child: Option<String>) -> String {
    let position = PAD_CHILD_ORDER
        .iter()
        .position(|&known| known == head)
        .expect("pad child has a known order");
    let before = &PAD_CHILD_ORDER[position + 1..];
    let existing = pad_children(&pad_block, head);
    let mut edits = Vec::new();
    match (child, existing.split_first()) {
        (Some(child), Some((&(start, end), duplicates))) => {
            edits.push(SexpEdit::replace(start, end, child));
            for &(start, end) in duplicates {
                edits.push(SexpEdit::delete(
                    leading_whitespace_start(&pad_block, start),
                    end,
                ));
            }
        }
        (Some(child), None) => {
            let children = find_direct_child_blocks(&pad_block, "pad");
            let first_after = children.iter().find(|&&(start, end)| {
                parse_sexp(&pad_block[start..end])
                    .is_ok_and(|node| node.head().is_some_and(|head| before.contains(&head)))
            });
            if let Some(&(start, _)) = first_after {
                let separator = &pad_block[leading_whitespace_start(&pad_block, start)..start];
                edits.push(SexpEdit::insert(start, format!("{child}{separator}")));
            } else if let Some(&(start, end)) = children.last() {
                let separator = &pad_block[leading_whitespace_start(&pad_block, start)..start];
                edits.push(SexpEdit::insert(end, format!("{separator}{child}")));
            } else if let Some(close) = pad_block.rfind(')') {
                edits.push(SexpEdit::insert(close, format!(" {child}")));
            }
        }
        (None, _) => {
            for &(start, end) in &existing {
                edits.push(SexpEdit::delete(
                    leading_whitespace_start(&pad_block, start),
                    end,
                ));
            }
        }
    }
    apply_edits(pad_block, edits)
}

/// A pad's fabrication property, KiCad's `PAD_PROP`. KiCad writes it as
/// `(property pad_prop_<name>)` and writes nothing for `None`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum PadFabricationProperty {
    None,
    Bga,
    FiducialGlobal,
    FiducialLocal,
    Testpoint,
    Heatsink,
    Castellated,
    Mechanical,
    Pressfit,
}

impl PadFabricationProperty {
    /// In KiCad's `PAD_PROP` order.
    const ALL: [Self; 9] = [
        Self::None,
        Self::Bga,
        Self::FiducialGlobal,
        Self::FiducialLocal,
        Self::Testpoint,
        Self::Heatsink,
        Self::Castellated,
        Self::Mechanical,
        Self::Pressfit,
    ];

    pub(super) fn names() -> [&'static str; 9] {
        Self::ALL.map(Self::as_str)
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|property| property.as_str() == name)
    }

    fn from_argument(value: &serde_json::Value) -> Result<Self, String> {
        let name = value
            .as_str()
            .ok_or_else(|| "must be a string when supplied".to_string())?;
        Self::parse(name).ok_or_else(|| {
            format!(
                "unsupported fabrication property '{name}'; expected one of {}",
                Self::names().join(", ")
            )
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bga => "bga",
            Self::FiducialGlobal => "fiducial_glob",
            Self::FiducialLocal => "fiducial_loc",
            Self::Testpoint => "testpoint",
            Self::Heatsink => "heatsink",
            Self::Castellated => "castellated",
            Self::Mechanical => "mechanical",
            Self::Pressfit => "pressfit",
        }
    }

    fn token(self) -> Option<String> {
        (self != Self::None).then(|| format!("pad_prop_{}", self.as_str()))
    }

    /// The warning KiCad's DRC and pad dialog give this property on a pad of
    /// `pad_type` (`PAD::CheckPad`, reported as "Padstack is questionable").
    /// KiCad still saves such a pad, so this is a warning, not a refusal.
    fn padstack_warning(self, number: &str, pad_type: &str, round_hole: bool) -> Option<String> {
        let plated_hole = pad_type == "thru_hole";
        let npth = pad_type == "np_thru_hole";
        let reason = match self {
            Self::FiducialGlobal | Self::FiducialLocal if npth => {
                Some("('fiducial' pads are normally plated)")
            }
            Self::Testpoint if npth => Some("('testpoint' pads are normally plated)"),
            Self::Heatsink if npth => Some("('heatsink' pads are normally plated)"),
            Self::Castellated if !plated_hole => Some("('castellated' pads are normally PTH)"),
            Self::Bga if pad_type != "smd" => Some("('BGA' property is for SMD pads)"),
            Self::Mechanical if !plated_hole => Some("('mechanical' pads are normally PTH)"),
            Self::Pressfit if !plated_hole || !round_hole => {
                Some("('press-fit' pads are normally PTH with round holes)")
            }
            _ => None,
        }?;
        Some(format!("pad '{number}': Padstack is questionable {reason}"))
    }

    /// The property KiCad would load from a pad: the last token it
    /// recognizes across every `(property …)` child, unknown tokens skipped.
    pub(crate) fn of_pad(pad: &SexpNode) -> Self {
        let mut property = Self::None;
        for child in pad.find_all("property") {
            for token in child.children().unwrap_or_default().iter().skip(1) {
                match token.as_str() {
                    Some("none") => property = Self::None,
                    Some(name) => {
                        if let Some(known) = name.strip_prefix("pad_prop_").and_then(Self::parse) {
                            property = known;
                        }
                    }
                    None => {}
                }
            }
        }
        property
    }
}

/// Whether a pad has a drilled round hole, KiCad's `PAD::HasDrilledHole`: a
/// drill whose width equals its height.
fn pad_has_round_hole(pad: &SexpNode) -> bool {
    crate::tools::pcb_components::file_pad_drill(pad)
        .and_then(|drill| drill.size)
        .is_some_and(|size| size.x > 0.0 && size.x == size.y)
}

pub(super) fn invalid_library_argument(field: &str, reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        ToolErrorKind::InvalidArgument {
            field: field.to_string(),
            reason: reason.clone(),
        },
        format!("Argument '{field}' is invalid: {reason}"),
    )
}

pub(crate) fn skip_sexp_token(source: &str, mut index: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    let start = index;
    if bytes.get(index) == Some(&b'"') {
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 2;
            } else if bytes[index] == b'"' {
                return Some((start, index + 1));
            } else {
                index += 1;
            }
        }
        return None;
    }
    while bytes
        .get(index)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b')')
    {
        index += 1;
    }
    (index > start).then_some((start, index))
}

fn pad_shape_range(pad_block: &str) -> Option<(usize, usize)> {
    let open = pad_block.find('(')? + 1;
    let (_, pad_end) = skip_sexp_token(pad_block, open)?;
    if &pad_block[open..pad_end] != "pad" {
        return None;
    }
    let (_, number_end) = skip_sexp_token(pad_block, pad_end)?;
    let (_, type_end) = skip_sexp_token(pad_block, number_end)?;
    skip_sexp_token(pad_block, type_end)
}

fn remove_pad_shape_children(pad_block: String, keep_roundrect_ratio: bool) -> String {
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(&pad_block, "pad") {
        let Ok(child) = parse_sexp(&pad_block[start..end]) else {
            continue;
        };
        let remove = match child.head() {
            Some("roundrect_rratio") => !keep_roundrect_ratio,
            Some("chamfer_ratio" | "chamfer" | "rect_delta" | "options" | "primitives") => true,
            _ => false,
        };
        if remove {
            edits.push(SexpEdit::delete(start, end));
        }
    }
    apply_edits(pad_block, edits)
}

fn has_direct_pad_child(pad_block: &str, head: &str) -> bool {
    find_direct_child_blocks(pad_block, "pad")
        .into_iter()
        .filter_map(|(start, end)| parse_sexp(&pad_block[start..end]).ok())
        .any(|child| child.head() == Some(head))
}

pub(super) fn edit_footprint_pad_block(
    pad_block: &str,
    args: &serde_json::Value,
    new_number: Option<&str>,
    shape: Option<&str>,
) -> Result<String, String> {
    let mut new_pad = pad_block.to_string();

    if let Some(number) = new_number {
        if let Some(number_start) = new_pad.find('"') {
            if let Some(number_end) = new_pad[number_start + 1..].find('"') {
                let number_end = number_start + 1 + number_end;
                new_pad.replace_range(number_start + 1..number_end, &escape_library_string(number));
            }
        }
    }

    if let Some(shape) = shape {
        let (shape_start, shape_end) = pad_shape_range(&new_pad)
            .ok_or_else(|| "could not locate the pad shape token".to_string())?;
        new_pad.replace_range(shape_start..shape_end, shape);
        let is_roundrect = shape == "roundrect";
        new_pad = remove_pad_shape_children(new_pad, is_roundrect);
        if is_roundrect && !has_direct_pad_child(&new_pad, "roundrect_rratio") {
            let insert_at = new_pad
                .rfind(')')
                .ok_or_else(|| "pad block has no closing parenthesis".to_string())?;
            let ratio = if new_pad.contains('\n') {
                "\n    (roundrect_rratio 0.25)"
            } else {
                " (roundrect_rratio 0.25)"
            };
            new_pad.insert_str(insert_at, ratio);
        }
    }

    if let Some(x) = args["x"].as_f64() {
        if let Some(at_pos) = new_pad.find("(at ") {
            let at_end = new_pad[at_pos..]
                .find(')')
                .map(|i| at_pos + i + 1)
                .unwrap_or(new_pad.len());
            let at_block = &new_pad[at_pos..at_end];
            let parts: Vec<&str> = at_block
                .trim_start_matches("(at ")
                .trim_end_matches(')')
                .split_whitespace()
                .collect();
            let old_y = parts
                .get(1)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let rot = parts.get(2).map(|s| format!(" {}", s)).unwrap_or_default();
            new_pad.replace_range(at_pos..at_end, &format!("(at {} {}{})", x, old_y, rot));
        }
    }
    if let Some(y) = args["y"].as_f64() {
        if let Some(at_pos) = new_pad.find("(at ") {
            let at_end = new_pad[at_pos..]
                .find(')')
                .map(|i| at_pos + i + 1)
                .unwrap_or(new_pad.len());
            let at_block = &new_pad[at_pos..at_end];
            let parts: Vec<&str> = at_block
                .trim_start_matches("(at ")
                .trim_end_matches(')')
                .split_whitespace()
                .collect();
            let old_x = parts
                .first()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let rot = parts.get(2).map(|s| format!(" {}", s)).unwrap_or_default();
            new_pad.replace_range(at_pos..at_end, &format!("(at {} {}{})", old_x, y, rot));
        }
    }
    if args["width"].as_f64().is_some() || args["height"].as_f64().is_some() {
        if let Some(size_pos) = new_pad.find("(size ") {
            let size_end = new_pad[size_pos..]
                .find(')')
                .map(|i| size_pos + i + 1)
                .unwrap_or(new_pad.len());
            let size_block = &new_pad[size_pos..size_end];
            let parts: Vec<&str> = size_block
                .trim_start_matches("(size ")
                .trim_end_matches(')')
                .split_whitespace()
                .collect();
            let old_width = parts
                .first()
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0);
            let old_height = parts
                .get(1)
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0);
            let width = args["width"].as_f64().unwrap_or(old_width);
            let height = args["height"].as_f64().unwrap_or(old_height);
            new_pad.replace_range(size_pos..size_end, &format!("(size {width} {height})"));
        }
    }
    if let Some(drill) = args.get("drill") {
        let node = parse_sexp(&new_pad).map_err(|e| e.to_string())?;
        let pad_type = node.get(2).and_then(SexpNode::as_str).unwrap_or("");
        let size = node.find("size").ok_or("pad size is missing")?;
        let drill_sexp = footprint_drill_sexp(
            drill,
            pad_type,
            size.get_f64(1).ok_or("pad width is missing")?,
            size.get_f64(2).ok_or("pad height is missing")?,
        )?;
        let existing =
            find_direct_child_blocks(&new_pad, "pad")
                .into_iter()
                .find(|(start, end)| {
                    parse_sexp(&new_pad[*start..*end]).is_ok_and(|n| n.head() == Some("drill"))
                });
        if let Some((drill_pos, drill_end)) = existing {
            let old_drill =
                parse_sexp(&new_pad[drill_pos..drill_end]).map_err(|e| e.to_string())?;
            if old_drill.find("offset").is_some() {
                return Err("editing an offset drill is not supported".into());
            }
            new_pad.replace_range(drill_pos..drill_end, &drill_sexp);
        } else {
            let insert_at = new_pad.rfind(')').unwrap_or(new_pad.len());
            new_pad.insert_str(insert_at, &format!(" {drill_sexp}"));
        }
    }

    Ok(new_pad)
}

pub(super) fn escape_library_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

// ─── Library table helpers ────────────────────────────────────────────────────
