//! A custom pad's shape: its `(options …)` and `(primitives …)`, read and
//! written as KiCad 10 does (`PCB_IO_KICAD_SEXPR::format( const PAD* )`,
//! `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp:1951-2066`, 10.0.8).
//!
//! One model for the library tools (#675) and the board paths (#679), as the
//! maintainer asked on both. The primitives use `set_footprint_graphics`' item
//! vocabulary and validators, in the pad's own coordinates: relative to its
//! anchor and unrotated, as KiCad stores them. The pad's `size` is the
//! anchor's size and is not part of this model.

// The custom pad tools of #675 and #679 build on this model in the commits
// that follow; drop this once they use it.
#![cfg_attr(not(test), allow(dead_code))]

use super::footprint_graphics::{Fill, FootprintGraphic, Point};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::SexpNode;
use serde::{Deserialize, Serialize};

/// The anchor shape KiCad merges with the primitives (`(anchor …)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Anchor {
    Circle,
    Rect,
}

/// The outline KiCad uses for a custom pad's clearance in zones
/// (`(clearance …)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Clearance {
    #[default]
    Outline,
    Convexhull,
}

/// A custom pad's anchor, clearance option and primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomPadShape {
    pub(super) anchor: Anchor,
    #[serde(default)]
    pub(super) clearance: Clearance,
    pub(super) primitives: Vec<FootprintGraphic>,
}

impl CustomPadShape {
    /// Read a custom pad's shape from its `(pad …)` node. Every clause is
    /// either represented or refused, so a shape read here and written back
    /// is the shape KiCad has. Without `(options …)`, KiCad's defaults apply:
    /// a circle anchor (`PAD::PAD`, `pcbnew/pad.cpp:95`) and the outline
    /// clearance (`PADSTACK::PADSTACK`, `pcbnew/padstack.cpp:49`).
    pub(super) fn from_pad(pad: &SexpNode) -> Result<Self, String> {
        let mut anchor = Anchor::Circle;
        let mut clearance = Clearance::Outline;
        if let Some(options) = only_child(pad, "options")? {
            for option in items(options) {
                match option.head() {
                    Some("anchor") => {
                        anchor = match single_word(option, "anchor")? {
                            "circle" => Anchor::Circle,
                            "rect" => Anchor::Rect,
                            other => return Err(format!("unknown custom pad anchor '{other}'")),
                        }
                    }
                    Some("clearance") => {
                        clearance = match single_word(option, "clearance")? {
                            "outline" => Clearance::Outline,
                            "convexhull" => Clearance::Convexhull,
                            other => return Err(format!("unknown custom pad clearance '{other}'")),
                        }
                    }
                    other => {
                        return Err(format!(
                            "custom pad option '{}' is not supported",
                            other.unwrap_or("?")
                        ))
                    }
                }
            }
        }
        let primitives =
            only_child(pad, "primitives")?.ok_or("the custom pad has no primitives")?;
        let primitives = items(primitives)
            .enumerate()
            .map(|(index, item)| {
                read_primitive(item).map_err(|reason| format!("primitive {index}: {reason}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if primitives.is_empty() {
            return Err("the custom pad has no primitives".to_string());
        }
        Ok(Self {
            anchor,
            clearance,
            primitives,
        })
    }

    /// Read and validate a shape given as JSON, in `set_footprint_graphics`'
    /// item vocabulary. The error names the field.
    ///
    /// Every coordinate, radius and width is snapped to KiCad's 1 nm grid, as
    /// KiCad's own footprint editor stores it, so a computed value such as
    /// `0.1 + 0.2` is written as `0.3` and reads back as given (#694 review).
    /// A primitive that rounding makes invalid, such as a line whose ends
    /// meet, is refused with the field named.
    pub(super) fn from_json(value: &serde_json::Value) -> Result<Self, (String, String)> {
        let mut shape: Self = serde_json::from_value(value.clone())
            .map_err(|error| (String::new(), error.to_string()))?;
        if shape.primitives.is_empty() {
            return Err((
                "primitives".to_string(),
                "a custom pad needs at least one primitive".to_string(),
            ));
        }
        for (index, primitive) in shape.primitives.iter_mut().enumerate() {
            primitive
                .validate_and_normalize()
                .map_err(|reason| (format!("primitives[{index}]"), reason))?;
            snap_to_nanometres(primitive);
            primitive.validate_and_normalize().map_err(|reason| {
                (
                    format!("primitives[{index}]"),
                    format!("{reason} once rounded to KiCad's 1 nm grid"),
                )
            })?;
        }
        Ok(shape)
    }

    /// The shape as JSON, the form [`Self::from_json`] reads.
    pub(super) fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a custom pad shape serializes")
    }

    /// `(options …)` as KiCad writes it for a pad child whose line is
    /// indented by `depth` tabs. The first line's indentation is the caller's.
    pub(super) fn options_sexp(&self, depth: usize) -> String {
        let clearance = match self.clearance {
            Clearance::Outline => "outline",
            Clearance::Convexhull => "convexhull",
        };
        let anchor = match self.anchor {
            Anchor::Circle => "circle",
            Anchor::Rect => "rect",
        };
        let inner = tabs(depth + 1);
        format!(
            "(options\n{inner}(clearance {clearance})\n{inner}(anchor {anchor})\n{})",
            tabs(depth)
        )
    }

    /// `(primitives …)` as KiCad writes it for a pad child whose line is
    /// indented by `depth` tabs. The first line's indentation is the caller's.
    pub(super) fn primitives_sexp(&self, depth: usize) -> String {
        let mut out = String::from("(primitives\n");
        for primitive in &self.primitives {
            out.push_str(&tabs(depth + 1));
            out.push_str(&primitive_sexp(primitive, depth + 1));
            out.push('\n');
        }
        out.push_str(&tabs(depth));
        out.push(')');
        out
    }
}

/// A length in millimetres on KiCad's 1 nm grid, without a negative zero.
fn nanometres(value: f64) -> f64 {
    let snapped = (value * 1e6).round() / 1e6;
    if snapped == 0.0 {
        0.0
    } else {
        snapped
    }
}

/// Snap every coordinate, radius and width of `primitive` to 1 nm.
fn snap_to_nanometres(primitive: &mut FootprintGraphic) {
    let snap = |point: &mut Point| {
        point.x = nanometres(point.x);
        point.y = nanometres(point.y);
    };
    match primitive {
        FootprintGraphic::Line {
            start,
            end,
            stroke_width_mm,
        } => {
            snap(start);
            snap(end);
            *stroke_width_mm = nanometres(*stroke_width_mm);
        }
        FootprintGraphic::Arc {
            start,
            mid,
            end,
            stroke_width_mm,
        } => {
            snap(start);
            snap(mid);
            snap(end);
            *stroke_width_mm = nanometres(*stroke_width_mm);
        }
        FootprintGraphic::Rect {
            start,
            end,
            stroke_width_mm,
            ..
        } => {
            snap(start);
            snap(end);
            *stroke_width_mm = nanometres(*stroke_width_mm);
        }
        FootprintGraphic::Circle {
            center,
            radius_mm,
            stroke_width_mm,
            ..
        } => {
            snap(center);
            *radius_mm = nanometres(*radius_mm);
            *stroke_width_mm = nanometres(*stroke_width_mm);
        }
        FootprintGraphic::Poly {
            points,
            stroke_width_mm,
            ..
        } => {
            points.iter_mut().for_each(snap);
            *stroke_width_mm = nanometres(*stroke_width_mm);
        }
    }
}

/// The node's items after its head.
fn items(node: &SexpNode) -> impl Iterator<Item = &SexpNode> {
    node.children().unwrap_or_default().iter().skip(1)
}

/// The node's one direct child `tag`, if any; a second one is refused,
/// since KiCad's reader would keep one of them.
fn only_child<'a>(node: &'a SexpNode, tag: &str) -> Result<Option<&'a SexpNode>, String> {
    match node.find_all(tag).as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => Err(format!("({tag} …) appears more than once")),
    }
}

/// The one word of `(tag word)`.
fn single_word<'a>(node: &'a SexpNode, tag: &str) -> Result<&'a str, String> {
    match node.children().unwrap_or_default() {
        [_, word] => word
            .as_str()
            .ok_or_else(|| format!("({tag} …) is not a word")),
        _ => Err(format!("({tag} …) must hold one word")),
    }
}

/// One primitive, refusing any clause KiCad's writer does not produce for it.
fn read_primitive(item: &SexpNode) -> Result<FootprintGraphic, String> {
    let head = item.head().ok_or("a primitive has no type")?;
    let allowed: &[&str] = match head {
        "gr_line" => &["start", "end", "width"],
        "gr_arc" => &["start", "mid", "end", "width"],
        "gr_rect" => &["start", "end", "width", "fill"],
        "gr_circle" => &["center", "end", "width", "fill"],
        "gr_poly" => &["pts", "width", "fill"],
        "gr_curve" => return Err("Bezier curve primitives (gr_curve) are not supported".into()),
        "gr_bbox" | "gr_vector" => {
            return Err(format!("{head} is a KiCad proxy item, not a pad primitive"))
        }
        other => return Err(format!("unknown primitive type '{other}'")),
    };
    let mut seen = Vec::new();
    for clause in items(item) {
        let tag = clause
            .head()
            .ok_or_else(|| format!("{head} holds a bare value"))?;
        if !allowed.contains(&tag) {
            return Err(format!("{head} clause '{tag}' is not supported"));
        }
        if seen.contains(&tag) {
            return Err(format!("{head} clause '{tag}' appears more than once"));
        }
        seen.push(tag);
    }
    let width = item
        .find("width")
        .and_then(|width| width.get_f64(1))
        .ok_or_else(|| format!("{head} has no width"))?;
    let point = |tag: &str| -> Result<Point, String> {
        let node = item
            .find(tag)
            .ok_or_else(|| format!("{head} has no {tag}"))?;
        xy(node).ok_or_else(|| format!("{head} {tag} is not a point"))
    };
    // KiCad's reader (`parsePCB_SHAPE`, `pcb_io_kicad_sexpr_parser.cpp`,
    // 10.0.8): `yes` or `solid` fills, `no` or `none` does not. Without the
    // keyword, as in files from older writers and external generators, a
    // polygon is filled, and a rectangle or circle is filled when its width
    // is 0. KiCad's own writer always writes the keyword.
    let fill = || -> Result<Fill, String> {
        match item.find("fill").map(|fill| single_word(fill, "fill")) {
            Some(Ok("yes" | "solid")) => Ok(Fill::Solid),
            Some(Ok("no" | "none")) => Ok(Fill::None),
            Some(Ok(other)) => Err(format!("{head} fill '{other}' is not supported")),
            Some(Err(reason)) => Err(reason),
            None if head == "gr_poly" || width == 0.0 => Ok(Fill::Solid),
            None => Ok(Fill::None),
        }
    };
    let mut primitive = match head {
        "gr_line" => FootprintGraphic::Line {
            start: point("start")?,
            end: point("end")?,
            stroke_width_mm: width,
        },
        "gr_arc" => FootprintGraphic::Arc {
            start: point("start")?,
            mid: point("mid")?,
            end: point("end")?,
            stroke_width_mm: width,
        },
        "gr_rect" => FootprintGraphic::Rect {
            start: point("start")?,
            end: point("end")?,
            stroke_width_mm: width,
            fill: fill()?,
        },
        "gr_circle" => {
            let center = point("center")?;
            let end = point("end")?;
            FootprintGraphic::Circle {
                center,
                radius_mm: (end.x - center.x).hypot(end.y - center.y),
                stroke_width_mm: width,
                fill: fill()?,
            }
        }
        "gr_poly" => {
            let pts = item.find("pts").ok_or("gr_poly has no pts")?;
            let points = items(pts)
                .map(|node| match node.head() {
                    Some("xy") => xy(node).ok_or_else(|| "gr_poly point is not a point".into()),
                    // KiCad also writes arc segments in an outline. The
                    // shared vocabulary's polygon has points only; 3 of the
                    // 2,027 custom pads in KiCad 10.0.8's library use them.
                    Some("arc") => {
                        Err("gr_poly outlines with arc segments are not supported".to_string())
                    }
                    _ => Err("gr_poly pts holds something other than xy".to_string()),
                })
                .collect::<Result<Vec<_>, String>>()?;
            FootprintGraphic::Poly {
                points,
                stroke_width_mm: width,
                fill: fill()?,
            }
        }
        _ => unreachable!("every allowed head is handled above"),
    };
    primitive.validate_and_normalize()?;
    Ok(primitive)
}

/// The point of `(tag x y)`.
fn xy(node: &SexpNode) -> Option<Point> {
    if node.children()?.len() != 3 {
        return None;
    }
    let point = Point {
        x: node.get_f64(1)?,
        y: node.get_f64(2)?,
    };
    (point.x.is_finite() && point.y.is_finite()).then_some(point)
}

fn tabs(depth: usize) -> String {
    "\t".repeat(depth)
}

fn point_clause(tag: &str, point: Point) -> String {
    format!("({tag} {} {})", fmt_f64(point.x), fmt_f64(point.y))
}

fn fill_word(fill: &Fill) -> &'static str {
    match fill {
        Fill::Solid => "yes",
        Fill::None => "no",
    }
}

/// One primitive as KiCad writes it, its own line indented by `depth` tabs.
fn primitive_sexp(primitive: &FootprintGraphic, depth: usize) -> String {
    let inner = tabs(depth + 1);
    let mut clauses = Vec::new();
    let head = match primitive {
        FootprintGraphic::Line {
            start,
            end,
            stroke_width_mm,
        } => {
            clauses.push(point_clause("start", *start));
            clauses.push(point_clause("end", *end));
            clauses.push(format!("(width {})", fmt_f64(*stroke_width_mm)));
            "gr_line"
        }
        FootprintGraphic::Arc {
            start,
            mid,
            end,
            stroke_width_mm,
        } => {
            clauses.push(point_clause("start", *start));
            clauses.push(point_clause("mid", *mid));
            clauses.push(point_clause("end", *end));
            clauses.push(format!("(width {})", fmt_f64(*stroke_width_mm)));
            "gr_arc"
        }
        FootprintGraphic::Rect {
            start,
            end,
            stroke_width_mm,
            fill,
        } => {
            clauses.push(point_clause("start", *start));
            clauses.push(point_clause("end", *end));
            clauses.push(format!("(width {})", fmt_f64(*stroke_width_mm)));
            clauses.push(format!("(fill {})", fill_word(fill)));
            "gr_rect"
        }
        FootprintGraphic::Circle {
            center,
            radius_mm,
            stroke_width_mm,
            fill,
        } => {
            let end = Point {
                x: center.x + radius_mm,
                y: center.y,
            };
            clauses.push(point_clause("center", *center));
            clauses.push(point_clause("end", end));
            clauses.push(format!("(width {})", fmt_f64(*stroke_width_mm)));
            clauses.push(format!("(fill {})", fill_word(fill)));
            "gr_circle"
        }
        FootprintGraphic::Poly {
            points,
            stroke_width_mm,
            fill,
        } => {
            clauses.push(format!(
                "(pts\n{}{}\n{inner})",
                tabs(depth + 2),
                xy_lines(points, depth + 2)
            ));
            clauses.push(format!("(width {})", fmt_f64(*stroke_width_mm)));
            clauses.push(format!("(fill {})", fill_word(fill)));
            "gr_poly"
        }
    };
    let mut out = format!("({head}");
    for clause in clauses {
        out.push('\n');
        out.push_str(&inner);
        out.push_str(&clause);
    }
    out.push('\n');
    out.push_str(&tabs(depth));
    out.push(')');
    out
}

/// A polygon's `(xy …)` list as KiCad's `Prettify` lays it out
/// (`common/io/kicad/kicad_io_utils.cpp`, 10.0.8): points share a line while
/// the column, counting each tab as one, is below 99
/// (`xySpecialCaseColumnLimit`); the next one starts a new line indented by
/// `depth` tabs.
fn xy_lines(points: &[Point], depth: usize) -> String {
    const XY_COLUMN_LIMIT: usize = 99;
    let mut out = String::new();
    let mut column = depth;
    for (index, point) in points.iter().enumerate() {
        let clause = point_clause("xy", *point);
        if index == 0 {
            column += clause.len();
            out.push_str(&clause);
        } else if column < XY_COLUMN_LIMIT {
            column += 1 + clause.len();
            out.push(' ');
            out.push_str(&clause);
        } else {
            out.push('\n');
            out.push_str(&tabs(depth));
            column = depth + clause.len();
            out.push_str(&clause);
        }
    }
    out
}

#[cfg(test)]
#[path = "pad_primitives_tests.rs"]
mod tests;
