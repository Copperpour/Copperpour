//! `replace_board_outline`: replace the board's own Edge.Cuts shapes with
//! closed contours of lines and arcs (#47). Contours that do not close
//! exactly, hold an item too small or too far out for KiCad, or cross are
//! refused before anything is written.

use crate::hex::encode_hex;
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_graphics::{point_schema, primitive_schema};
use crate::tools::pcb_board::{
    attempt_ipc_write_in, block_origin, block_tag, format_gr_arc, format_gr_line, graphic_json,
    graphic_kind, BoardWrite, NoLiveBoard,
};
use crate::tools::{get_path, ToolContext, ToolDef};
use copperpour_ipc::builders;
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_sexp::parser::{parse_sexp, SexpNode};
use copperpour_sexp::writer::{
    apply_edits, find_block_with_leading_whitespace, find_direct_child_blocks, read_consistent,
    SexpEdit,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::path::Path;

/// A point in KiCad's board unit, the nanometre.
type Point = (i64, i64);

const EDGE_CUTS: &str = "Edge.Cuts";

/// The stroke `add_board_outline` and `set_board_size` draw Edge.Cuts with.
const STROKE_WIDTH_MM: f64 = 0.05;

/// KiCad's DRC reports an Edge.Cuts line or arc this size or smaller as a
/// malformed outline (`minSizeForValideGraphics` in
/// `DRC_TEST_PROVIDER_MISC::testOutline`, measured as
/// `TestBoardOutlinesGraphicItems` measures it).
const MIN_SIZE_NM: i64 = 1_000;

/// KiCad's board reader clamps a coordinate beyond this (`INT_LIMIT` in
/// `pcb_io_kicad_sexpr_parser.cpp`).
const COORDINATE_LIMIT_NM: i64 = i32::MAX as i64 - 10;

/// The crossing check follows an arc by chords that stray no further than
/// this from it: KiCad's smallest outline item.
const CHORD_ERROR_NM: f64 = 1_000.0;

/// How far off the drawn circle the readback lets an arc's mid point lie.
/// KiCad may store an arc reversed, its mid point then the arc's own midpoint
/// worked out from a centre kept to the nanometre (`EDA_SHAPE::SetArcGeometry`).
const ARC_MID_TOLERANCE_NM: i64 = 10;

/// The graphic kinds KiCad builds a board outline from.
const SHAPE_KINDS: [&str; 6] = ["line", "rect", "arc", "circle", "poly", "curve"];

/// The footprint-owned shapes KiCad also reads on Edge.Cuts.
const FOOTPRINT_SHAPE_TAGS: [&str; 6] = [
    "fp_line",
    "fp_rect",
    "fp_arc",
    "fp_circle",
    "fp_poly",
    "fp_curve",
];

pub(crate) fn tool() -> ToolDef {
    let mut line = serde_json::Map::new();
    line.insert("start".to_string(), point_schema());
    line.insert("end".to_string(), point_schema());
    let mut arc = line.clone();
    arc.insert("mid".to_string(), point_schema());

    tool!(
        "replace_board_outline",
        "Replace the board outline: delete every board-owned shape on Edge.Cuts and draw \
         the closed contours given, made of lines and arcs. A dry run, the default, lists \
         what would be deleted and returns a plan_revision; apply with dry_run=false and \
         that plan_revision as expected_plan_revision. Before writing it refuses a contour \
         that does not close exactly, a line or arc of 0.001 mm or less, and primitives \
         that cross or touch other than at their shared joint. A contour inside an outline \
         is a cut-out; 'contours' gives each contour's role. Edge.Cuts shapes owned by \
         footprints are left alone and counted, not checked. One undoable commit on the \
         board KiCad holds open, else the saved file; the outline is read back after the \
         write.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "contours": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Closed contours in mm, each an ordered list of lines and arcs: each primitive's end is the next one's start, and the last one's end the first one's start",
                    "items": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "oneOf": [
                                primitive_schema("line", line, &["type", "start", "end"]),
                                primitive_schema("arc", arc, &["type", "start", "mid", "end"])
                            ]
                        }
                    }
                },
                "dry_run": {
                    "type": "boolean",
                    "default": true,
                    "description": "Check the contours and list what would be replaced without writing"
                },
                "expected_plan_revision": {
                    "type": "string",
                    "description": "Required for apply; exact plan_revision returned by a current dry run"
                }
            },
            "required": ["board", "contours"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_replace_board_outline(args, ctx).await }
    )
    .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback)
}

// ─── The contours ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Primitive {
    Line {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        mid: Point,
        end: Point,
    },
}

impl Primitive {
    fn start(&self) -> Point {
        match *self {
            Self::Line { start, .. } | Self::Arc { start, .. } => start,
        }
    }

    fn end(&self) -> Point {
        match *self {
            Self::Line { end, .. } | Self::Arc { end, .. } => end,
        }
    }
}

/// Why the contours cannot be drawn, under the argument path that names the
/// fault, as `contours[1][3].end`.
#[derive(Debug)]
struct Fault {
    field: String,
    reason: String,
}

impl Fault {
    fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    fn into_result(self) -> CallToolResult {
        CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: self.field.clone(),
                reason: self.reason.clone(),
            },
            format!(
                "Argument '{}' is invalid: {}. Nothing was written.",
                self.field, self.reason
            ),
        )
    }
}

fn primitive_field(at: (usize, usize)) -> String {
    format!("contours[{}][{}]", at.0, at.1)
}

fn parse_contours(value: &Value) -> Result<Vec<Vec<Primitive>>, Fault> {
    let contours = value
        .as_array()
        .filter(|contours| !contours.is_empty())
        .ok_or_else(|| Fault::new("contours", "must be a non-empty array of contours"))?;
    contours
        .iter()
        .enumerate()
        .map(|(i, contour)| {
            contour
                .as_array()
                .filter(|primitives| !primitives.is_empty())
                .ok_or_else(|| {
                    Fault::new(
                        format!("contours[{i}]"),
                        "must be a non-empty array of lines and arcs",
                    )
                })?
                .iter()
                .enumerate()
                .map(|(j, primitive)| parse_primitive(primitive, &primitive_field((i, j))))
                .collect()
        })
        .collect()
}

fn parse_primitive(value: &Value, field: &str) -> Result<Primitive, Fault> {
    let point = |key: &str| parse_point(&value[key], &format!("{field}.{key}"));
    match value["type"].as_str() {
        Some("line") => Ok(Primitive::Line {
            start: point("start")?,
            end: point("end")?,
        }),
        Some("arc") => Ok(Primitive::Arc {
            start: point("start")?,
            mid: point("mid")?,
            end: point("end")?,
        }),
        _ => Err(Fault::new(
            format!("{field}.type"),
            "must be 'line' or 'arc'",
        )),
    }
}

/// A point in millimetres, rounded to the nanometre as KiCad's board reader
/// rounds (`parseBoardUnits`).
fn parse_point(value: &Value, field: &str) -> Result<Point, Fault> {
    let coordinate = |axis: &str| {
        let field = format!("{field}.{axis}");
        let mm = value[axis]
            .as_f64()
            .ok_or_else(|| Fault::new(&field, "must be a number of millimetres"))?;
        let nm = (mm * 1e6).round();
        if !nm.is_finite() || nm.abs() > COORDINATE_LIMIT_NM as f64 {
            return Err(Fault::new(
                &field,
                format!(
                    "{mm} mm is beyond the ±{} mm KiCad keeps on a board",
                    mm_text(COORDINATE_LIMIT_NM)
                ),
            ));
        }
        Ok(nm as i64)
    };
    Ok((coordinate("x")?, coordinate("y")?))
}

/// Millimetres as exact text, as KiCad writes a board unit.
pub(super) fn mm_text(nm: i64) -> String {
    let sign = if nm < 0 { "-" } else { "" };
    let (whole, fraction) = (nm.unsigned_abs() / 1_000_000, nm.unsigned_abs() % 1_000_000);
    let fraction = format!("{fraction:06}");
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

fn point_text((x, y): Point) -> String {
    format!("({}, {})", mm_text(x), mm_text(y))
}

fn mm(nm: i64) -> f64 {
    nm as f64 / 1e6
}

/// A vector's length as KiCad measures it, rounded to the nanometre
/// (`VECTOR2::EuclideanNorm`).
fn kicad_length(from: Point, to: Point) -> i64 {
    ((to.0 - from.0) as f64)
        .hypot((to.1 - from.1) as f64)
        .round() as i64
}

/// Twice the signed area of the triangle `a b c`: positive when `c` lies
/// left of `a → b`, zero when the three are on one line. Exact.
fn orient(a: Point, b: Point, c: Point) -> i128 {
    let (bx, by) = ((b.0 - a.0) as i128, (b.1 - a.1) as i128);
    let (cx, cy) = ((c.0 - a.0) as i128, (c.1 - a.1) as i128);
    bx * cy - by * cx
}

// ─── The checks ───────────────────────────────────────────────────────────────

/// Whether a contour bounds board or cuts it out, read as KiCad reads nesting:
/// inside an even number of other contours is an outline, inside an odd number
/// a cut-out (`addOutlinesToPolygon`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Outline,
    Cutout,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Cutout => "cutout",
        }
    }
}

fn check_contours(contours: &[Vec<Primitive>]) -> Result<Vec<Role>, Fault> {
    check_sizes(contours)?;
    check_joints(contours)?;
    let chords = chords(contours);
    check_crossings(contours, &chords)?;
    Ok(roles(contours, &chords))
}

fn check_sizes(contours: &[Vec<Primitive>]) -> Result<(), Fault> {
    for (i, contour) in contours.iter().enumerate() {
        for (j, primitive) in contour.iter().enumerate() {
            let field = primitive_field((i, j));
            match *primitive {
                Primitive::Line { start, end } => {
                    let length = kicad_length(start, end);
                    if length <= MIN_SIZE_NM {
                        return Err(Fault::new(
                            field,
                            format!(
                                "the line is {} mm long; KiCad reports an Edge.Cuts line of \
                                 0.001 mm or less as a malformed outline",
                                mm_text(length)
                            ),
                        ));
                    }
                }
                Primitive::Arc { start, mid, end } => {
                    if orient(start, mid, end) == 0 {
                        return Err(Fault::new(
                            field,
                            "its start, mid and end lie on one line, so they draw no arc",
                        ));
                    }
                    // KiCad holds an arc's centre as a board point (`EDA_SHAPE`).
                    let circle = ArcCircle::of(start, mid, end);
                    let limit = COORDINATE_LIMIT_NM as f64;
                    if circle.center.0.abs() > limit || circle.center.1.abs() > limit {
                        return Err(Fault::new(
                            field,
                            format!(
                                "the arc is so nearly straight that its centre lies beyond the \
                                 ±{} mm KiCad keeps on a board; draw it as a line",
                                mm_text(COORDINATE_LIMIT_NM)
                            ),
                        ));
                    }
                    // Measured through the mid point given, which KiCad keeps
                    // and measures through (`GetArcMid`) unless it stores the
                    // arc reversed; then it measures through the arc's own
                    // midpoint, which is longer.
                    let size = kicad_length(start, mid) + kicad_length(mid, end);
                    if size <= MIN_SIZE_NM {
                        return Err(Fault::new(
                            field,
                            format!(
                                "the arc measures {} mm start to mid to end; KiCad reports an \
                                 Edge.Cuts arc of 0.001 mm or less as a malformed outline",
                                mm_text(size)
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Each primitive ends exactly where the next one starts. KiCad would bridge
/// a gap of up to 0.01 mm (`DEFAULT_CHAINING_EPSILON_MM`); this tool closes none.
fn check_joints(contours: &[Vec<Primitive>]) -> Result<(), Fault> {
    for (i, contour) in contours.iter().enumerate() {
        for (j, primitive) in contour.iter().enumerate() {
            let next = (j + 1) % contour.len();
            let (end, start) = (primitive.end(), contour[next].start());
            if end != start {
                let reason = if next == 0 {
                    format!(
                        "the contour does not close: it ends at {} and starts at {}, {} mm apart",
                        point_text(end),
                        point_text(start),
                        mm_text(kicad_length(end, start))
                    )
                } else {
                    format!(
                        "it starts at {}, {} mm from where {} ends at {}",
                        point_text(start),
                        mm_text(kicad_length(end, start)),
                        primitive_field((i, j)),
                        point_text(end)
                    )
                };
                return Err(Fault::new(
                    format!("{}.start", primitive_field((i, next))),
                    reason,
                ));
            }
        }
    }
    Ok(())
}

/// One straight piece of a primitive: a line, or a chord of an arc.
#[derive(Debug, Clone, Copy)]
struct Chord {
    a: Point,
    b: Point,
    /// The contour and primitive it belongs to.
    at: (usize, usize),
}

impl Chord {
    fn x_range(&self) -> (i64, i64) {
        (self.a.0.min(self.b.0), self.a.0.max(self.b.0))
    }

    fn y_range(&self) -> (i64, i64) {
        (self.a.1.min(self.b.1), self.a.1.max(self.b.1))
    }
}

/// An arc as its circle: centre, radius, the angle of its start and its
/// signed sweep, positive from +x towards +y.
struct ArcCircle {
    center: (f64, f64),
    radius: f64,
    start_angle: f64,
    sweep: f64,
}

impl ArcCircle {
    fn of(start: Point, mid: Point, end: Point) -> Self {
        // The circumcentre, relative to `start`, from exact sums.
        let (bx, by) = ((mid.0 - start.0) as i128, (mid.1 - start.1) as i128);
        let (cx, cy) = ((end.0 - start.0) as i128, (end.1 - start.1) as i128);
        let turn = orient(start, mid, end);
        let d = (2 * turn) as f64;
        let (b2, c2) = (bx * bx + by * by, cx * cx + cy * cy);
        let ux = (cy * b2 - by * c2) as f64 / d;
        let uy = (bx * c2 - cx * b2) as f64 / d;
        let center = (start.0 as f64 + ux, start.1 as f64 + uy);
        let angle = |p: Point| (p.1 as f64 - center.1).atan2(p.0 as f64 - center.0);
        let (from, to) = (angle(start), angle(end));
        let sweep = if turn > 0 {
            (to - from).rem_euclid(TAU)
        } else {
            -(from - to).rem_euclid(TAU)
        };
        Self {
            center,
            radius: ux.hypot(uy),
            start_angle: from,
            sweep,
        }
    }

    fn point_at(&self, angle: f64) -> (f64, f64) {
        (
            self.center.0 + self.radius * angle.cos(),
            self.center.1 + self.radius * angle.sin(),
        )
    }

    fn spans(&self, angle: f64) -> bool {
        let along = if self.sweep > 0.0 {
            (angle - self.start_angle).rem_euclid(TAU)
        } else {
            (self.start_angle - angle).rem_euclid(TAU)
        };
        along <= self.sweep.abs()
    }
}

/// The arc's chords, at least two, each within [`CHORD_ERROR_NM`] of it; the
/// first starts on its start and the last ends on its end, exactly.
fn arc_points(start: Point, mid: Point, end: Point) -> Vec<Point> {
    let circle = ArcCircle::of(start, mid, end);
    let step = if circle.radius <= CHORD_ERROR_NM {
        PI
    } else {
        2.0 * (1.0 - CHORD_ERROR_NM / circle.radius).acos()
    };
    let count = ((circle.sweep.abs() / step).ceil() as usize).max(2);
    let mut points = vec![start];
    for k in 1..count {
        let (x, y) = circle.point_at(circle.start_angle + circle.sweep * k as f64 / count as f64);
        let point = (x.round() as i64, y.round() as i64);
        if points.last() != Some(&point) {
            points.push(point);
        }
    }
    if points.last() != Some(&end) {
        points.push(end);
    }
    points
}

/// Every contour as chords, in contour order.
fn chords(contours: &[Vec<Primitive>]) -> Vec<Chord> {
    let mut chords = Vec::new();
    for (i, contour) in contours.iter().enumerate() {
        for (j, primitive) in contour.iter().enumerate() {
            let points = match *primitive {
                Primitive::Line { start, end } => vec![start, end],
                Primitive::Arc { start, mid, end } => arc_points(start, mid, end),
            };
            chords.extend(points.windows(2).map(|pair| Chord {
                a: pair[0],
                b: pair[1],
                at: (i, j),
            }));
        }
    }
    chords
}

fn on_segment(a: Point, b: Point, p: Point) -> bool {
    orient(a, b, p) == 0
        && a.0.min(b.0) <= p.0
        && p.0 <= a.0.max(b.0)
        && a.1.min(b.1) <= p.1
        && p.1 <= a.1.max(b.1)
}

/// Whether two segments share any point, touching included. Exact.
fn segments_meet(p: (Point, Point), q: (Point, Point)) -> bool {
    let d1 = orient(q.0, q.1, p.0).signum();
    let d2 = orient(q.0, q.1, p.1).signum();
    let d3 = orient(p.0, p.1, q.0).signum();
    let d4 = orient(p.0, p.1, q.1).signum();
    (d1 * d2 < 0 && d3 * d4 < 0)
        || on_segment(q.0, q.1, p.0)
        || on_segment(q.0, q.1, p.1)
        || on_segment(p.0, p.1, q.0)
        || on_segment(p.0, p.1, q.1)
}

/// A point the two segments share, to name where they meet.
fn meeting_point(p: (Point, Point), q: (Point, Point)) -> Point {
    for (end, other) in [(p.0, q), (p.1, q), (q.0, p), (q.1, p)] {
        if on_segment(other.0, other.1, end) {
            return end;
        }
    }
    let r = ((p.1 .0 - p.0 .0) as f64, (p.1 .1 - p.0 .1) as f64);
    let s = ((q.1 .0 - q.0 .0) as f64, (q.1 .1 - q.0 .1) as f64);
    let t = (((q.0 .0 - p.0 .0) as f64) * s.1 - ((q.0 .1 - p.0 .1) as f64) * s.0)
        / (r.0 * s.1 - r.1 * s.0);
    (
        (p.0 .0 as f64 + t * r.0).round() as i64,
        (p.0 .1 as f64 + t * r.1).round() as i64,
    )
}

/// The joints two primitives share as neighbours in one contour: one, or two
/// when the contour is just the pair of them.
fn shared_joints(contours: &[Vec<Primitive>], x: (usize, usize), y: (usize, usize)) -> Vec<Point> {
    if x.0 != y.0 {
        return Vec::new();
    }
    let contour = &contours[x.0];
    let mut joints = Vec::new();
    if (x.1 + 1) % contour.len() == y.1 {
        joints.push(contour[x.1].end());
    }
    if (y.1 + 1) % contour.len() == x.1 {
        joints.push(contour[y.1].end());
    }
    joints
}

/// Where two chords of different primitives meet other than at a joint the
/// primitives share; `None` when they do not.
fn contact(c: &Chord, d: &Chord, joints: &[Point]) -> Option<Point> {
    if !segments_meet((c.a, c.b), (d.a, d.b)) {
        return None;
    }
    for &joint in joints {
        let c_other = if c.a == joint {
            c.b
        } else if c.b == joint {
            c.a
        } else {
            continue;
        };
        let d_other = if d.a == joint {
            d.b
        } else if d.b == joint {
            d.a
        } else {
            continue;
        };
        if orient(joint, c_other, d_other) != 0 {
            // Two segments from one point on different lines meet only there.
            return None;
        }
        let along = (c_other.0 - joint.0) as i128 * (d_other.0 - joint.0) as i128
            + (c_other.1 - joint.1) as i128 * (d_other.1 - joint.1) as i128;
        // On one line: they run on from the joint in opposite directions, or
        // one doubles back along the other.
        return (along > 0).then_some(joint);
    }
    Some(meeting_point((c.a, c.b), (d.a, d.b)))
}

/// No two primitives cross or touch, in one contour or across two, except
/// neighbours at their shared joint. Arcs are followed by their chords, swept
/// in x order as KiCad sweeps its own (`checkSelfIntersections`).
fn check_crossings(contours: &[Vec<Primitive>], chords: &[Chord]) -> Result<(), Fault> {
    let mut order: Vec<usize> = (0..chords.len()).collect();
    order.sort_by_key(|&k| chords[k].x_range().0);
    for (n, &k) in order.iter().enumerate() {
        let c = &chords[k];
        let (c_x, c_y) = (c.x_range(), c.y_range());
        for &l in &order[n + 1..] {
            let d = &chords[l];
            if d.x_range().0 > c_x.1 {
                break;
            }
            let d_y = d.y_range();
            if c.at == d.at || d_y.0 > c_y.1 || c_y.0 > d_y.1 {
                continue;
            }
            if let Some(point) = contact(c, d, &shared_joints(contours, c.at, d.at)) {
                let (first, second) = if c.at < d.at {
                    (c.at, d.at)
                } else {
                    (d.at, c.at)
                };
                return Err(Fault::new(
                    primitive_field(first),
                    format!(
                        "it crosses or touches {} at {}; contours may meet only where one \
                         primitive ends and the next starts",
                        primitive_field(second),
                        point_text(point)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Whether `p` lies inside the closed chord loop of one contour, by its
/// winding number. Exact; `p` is never on the loop once the crossings are
/// checked.
fn encloses(loop_chords: &[&Chord], p: Point) -> bool {
    let mut winding = 0i32;
    for chord in loop_chords {
        let (a, b) = (chord.a, chord.b);
        if a.1 <= p.1 {
            if b.1 > p.1 && orient(a, b, p) > 0 {
                winding += 1;
            }
        } else if b.1 <= p.1 && orient(a, b, p) < 0 {
            winding -= 1;
        }
    }
    winding != 0
}

fn roles(contours: &[Vec<Primitive>], chords: &[Chord]) -> Vec<Role> {
    let loops: Vec<Vec<&Chord>> = (0..contours.len())
        .map(|i| chords.iter().filter(|chord| chord.at.0 == i).collect())
        .collect();
    (0..contours.len())
        .map(|i| {
            let p = contours[i][0].start();
            let around = (0..contours.len())
                .filter(|&j| j != i && encloses(&loops[j], p))
                .count();
            if around % 2 == 0 {
                Role::Outline
            } else {
                Role::Cutout
            }
        })
        .collect()
}

/// The box around the contours, arcs' bulges included, in nanometres.
fn bounds(contours: &[Vec<Primitive>]) -> (f64, f64, f64, f64) {
    let mut points: Vec<(f64, f64)> = Vec::new();
    for primitive in contours.iter().flatten() {
        let (start, end) = (primitive.start(), primitive.end());
        points.push((start.0 as f64, start.1 as f64));
        points.push((end.0 as f64, end.1 as f64));
        if let Primitive::Arc { start, mid, end } = *primitive {
            let circle = ArcCircle::of(start, mid, end);
            for quarter in 0..4 {
                let angle = quarter as f64 * FRAC_PI_2 - PI;
                if circle.spans(angle) {
                    let (x, y) = circle.point_at(angle);
                    points.push((x.round(), y.round()));
                }
            }
        }
    }
    points.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    )
}

// ─── The board's Edge.Cuts ────────────────────────────────────────────────────

/// A board-owned shape on Edge.Cuts, as the board text holds it.
struct EdgeShape {
    uuid: String,
    kind: &'static str,
    origin: Option<(f64, f64)>,
    /// The block's byte range with its leading whitespace, so deleting it
    /// leaves no blank line.
    span: (usize, usize),
    /// A line or an arc, read in nanometres; `None` for any other kind.
    geometry: Option<Primitive>,
}

struct EdgeCuts {
    shapes: Vec<EdgeShape>,
    /// Shapes footprints own on Edge.Cuts, which KiCad adds to the outline
    /// and this tool leaves alone.
    footprint_shapes: usize,
}

fn node_point(node: &SexpNode, tag: &str) -> Option<Point> {
    let point = node.find(tag)?;
    let nm = |n| point.get_f64(n).map(|mm| (mm * 1e6).round() as i64);
    Some((nm(1)?, nm(2)?))
}

fn shape_geometry(node: &SexpNode) -> Option<Primitive> {
    match node.head()? {
        "gr_line" => Some(Primitive::Line {
            start: node_point(node, "start")?,
            end: node_point(node, "end")?,
        }),
        "gr_arc" => Some(Primitive::Arc {
            start: node_point(node, "start")?,
            mid: node_point(node, "mid")?,
            end: node_point(node, "end")?,
        }),
        _ => None,
    }
}

fn on_edge_cuts(node: &SexpNode) -> bool {
    node.find_str("layer") == Some(EDGE_CUTS)
}

/// The Edge.Cuts shapes of a board's text: the saved file, or the board KiCad
/// holds, as KiCad serialises it.
fn read_edge_cuts(content: &str) -> EdgeCuts {
    let mut shapes = Vec::new();
    let mut footprint_shapes = 0;
    for (start, end) in find_direct_child_blocks(content, "kicad_pcb") {
        let block = &content[start..end];
        if !block.contains(EDGE_CUTS) {
            continue;
        }
        let Some(tag) = block_tag(block) else {
            continue;
        };
        if tag == "footprint" {
            if let Ok(footprint) = parse_sexp(block) {
                footprint_shapes += footprint
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .filter(|child| {
                        child
                            .head()
                            .is_some_and(|head| FOOTPRINT_SHAPE_TAGS.contains(&head))
                            && on_edge_cuts(child)
                    })
                    .count();
            }
            continue;
        }
        let Some(kind) = graphic_kind(tag).filter(|kind| SHAPE_KINDS.contains(kind)) else {
            continue;
        };
        let Ok(node) = parse_sexp(block) else {
            continue;
        };
        if !on_edge_cuts(&node) {
            continue;
        }
        shapes.push(EdgeShape {
            uuid: node.find_str("uuid").unwrap_or_default().to_string(),
            kind,
            origin: block_origin(&node),
            span: find_block_with_leading_whitespace(content, start).unwrap_or((start, end)),
            geometry: shape_geometry(&node),
        });
    }
    EdgeCuts {
        shapes,
        footprint_shapes,
    }
}

/// What a dry run promises an apply: the board's Edge.Cuts shapes as read
/// from `source`, and the contours to draw.
fn plan_revision(
    board_path: &Path,
    source: &str,
    content: &str,
    existing: &EdgeCuts,
    contours: &[Vec<Primitive>],
) -> anyhow::Result<String> {
    let mut digest = Sha256::new();
    digest.update(source.as_bytes());
    digest.update([0]);
    digest.update(board_path.canonicalize()?.to_string_lossy().as_bytes());
    for shape in &existing.shapes {
        digest.update([0]);
        digest.update(content[shape.span.0..shape.span.1].trim().as_bytes());
    }
    for contour in contours {
        digest.update([1]);
        for primitive in contour {
            digest.update(format!("{primitive:?}").as_bytes());
        }
    }
    Ok(format!("sha256:{}", encode_hex(&digest.finalize())))
}

/// The answer both paths give, before what applying adds to it.
fn plan_body(
    contours: &[Vec<Primitive>],
    roles: &[Role],
    existing: &EdgeCuts,
    revision: &str,
    dry_run: bool,
) -> serde_json::Map<String, Value> {
    let count = |contour: &[Primitive], arcs: bool| {
        contour
            .iter()
            .filter(|primitive| matches!(primitive, Primitive::Arc { .. }) == arcs)
            .count()
    };
    let (x_min, y_min, x_max, y_max) = bounds(contours);
    let round = |nm: f64| nm.round() / 1e6;
    let body = json!({
        "dry_run": dry_run,
        "plan_revision": revision,
        "contour_count": contours.len(),
        "contours": contours.iter().zip(roles).map(|(contour, role)| json!({
            "role": role.as_str(),
            "line_count": count(contour, false),
            "arc_count": count(contour, true),
        })).collect::<Vec<_>>(),
        "line_count": contours.iter().map(|contour| count(contour, false)).sum::<usize>(),
        "arc_count": contours.iter().map(|contour| count(contour, true)).sum::<usize>(),
        "x_min": round(x_min), "y_min": round(y_min),
        "x_max": round(x_max), "y_max": round(y_max),
        "width": round(x_max - x_min), "height": round(y_max - y_min),
        "replaced_count": existing.shapes.len(),
        "replaced": existing.shapes.iter()
            .map(|shape| graphic_json(&shape.uuid, shape.kind, EDGE_CUTS, shape.origin))
            .collect::<Vec<_>>(),
        "footprint_edge_cuts_count": existing.footprint_shapes,
    });
    match body {
        Value::Object(map) => map,
        _ => unreachable!("json! of an object"),
    }
}

fn same_shape(drawn: &Primitive, read: &Primitive) -> bool {
    let same_ends =
        |a: Point, b: Point, c: Point, d: Point| (a == c && b == d) || (a == d && b == c);
    match (*drawn, *read) {
        (Primitive::Line { start, end }, Primitive::Line { start: s, end: e }) => {
            same_ends(start, end, s, e)
        }
        // The mid point read back is any point of the drawn arc: KiCad may
        // store an arc the other way round, with its own midpoint.
        (
            Primitive::Arc { start, mid, end },
            Primitive::Arc {
                start: s,
                mid: m,
                end: e,
            },
        ) => same_ends(start, end, s, e) && on_arc(&ArcCircle::of(start, mid, end), m),
        _ => false,
    }
}

fn on_arc(circle: &ArcCircle, p: Point) -> bool {
    let (dx, dy) = (p.0 as f64 - circle.center.0, p.1 as f64 - circle.center.1);
    (dx.hypot(dy) - circle.radius).abs() <= ARC_MID_TOLERANCE_NM as f64
        && circle.spans(dy.atan2(dx))
}

/// Why the board read back after the write does not hold exactly the
/// contours drawn; `None` when it does.
fn readback_mismatch(contours: &[Vec<Primitive>], after: &EdgeCuts) -> Option<String> {
    let drawn: Vec<&Primitive> = contours.iter().flatten().collect();
    if after.shapes.len() != drawn.len() {
        return Some(format!(
            "Edge.Cuts holds {} board shapes after the write, not the {} drawn",
            after.shapes.len(),
            drawn.len()
        ));
    }
    let mut unread: Vec<Option<Primitive>> =
        after.shapes.iter().map(|shape| shape.geometry).collect();
    for (k, primitive) in drawn.iter().enumerate() {
        let found = unread.iter().position(|read| {
            read.as_ref()
                .is_some_and(|read| same_shape(primitive, read))
        });
        match found {
            Some(index) => unread[index] = None,
            None => {
                return Some(format!(
                    "no Edge.Cuts shape read back matches primitive {k} ({primitive:?})"
                ))
            }
        }
    }
    None
}

fn stale_plan(board_path: &Path) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: board_path.display().to_string(),
            reason: "expected_plan_revision does not match the current plan".to_string(),
        },
        "The plan revision is stale: the board's Edge.Cuts shapes, the contours, or where \
         the board is read from changed since the dry run. Nothing was written; run the dry \
         run again and apply its plan_revision.",
    )
}

fn readback_uncertain(board_path: &Path, reason: impl Into<String>) -> CallToolResult {
    crate::tools::mutation_outcome_uncertain(board_path, "replace_board_outline", reason)
}

// ─── The writes ───────────────────────────────────────────────────────────────

/// One primitive as a KiCad board shape, in exact nanometres.
fn ipc_item(primitive: &Primitive) -> prost_types::Any {
    use kiapi::common::types::{
        graphic_shape::Geometry, GraphicArcAttributes, GraphicSegmentAttributes, Vector2,
    };
    let vector = |(x_nm, y_nm): Point| Some(Vector2 { x_nm, y_nm });
    let geometry = match *primitive {
        Primitive::Line { start, end } => Geometry::Segment(GraphicSegmentAttributes {
            start: vector(start),
            end: vector(end),
        }),
        Primitive::Arc { start, mid, end } => Geometry::Arc(GraphicArcAttributes {
            start: vector(start),
            mid: vector(mid),
            end: vector(end),
        }),
    };
    // The builder takes millimetres and truncates them; the geometry is set
    // in nanometres so the joints KiCad receives are the ones checked.
    let mut shape = builders::board_segment(EDGE_CUTS, STROKE_WIDTH_MM, 0.0, 0.0, 0.0, 0.0);
    if let Some(graphic) = shape.shape.as_mut() {
        graphic.geometry = Some(geometry);
    }
    builders::pack_any(&shape, "kiapi.board.types.BoardGraphicShape")
}

fn format_primitives(contours: &[Vec<Primitive>]) -> String {
    contours
        .iter()
        .flatten()
        .map(|primitive| match *primitive {
            Primitive::Line { start, end } => format_gr_line(
                mm(start.0),
                mm(start.1),
                mm(end.0),
                mm(end.1),
                EDGE_CUTS,
                STROKE_WIDTH_MM,
            ),
            Primitive::Arc { start, mid, end } => format_gr_arc(
                (mm(start.0), mm(start.1)),
                (mm(mid.0), mm(mid.1)),
                (mm(end.0), mm(end.1)),
                EDGE_CUTS,
                STROKE_WIDTH_MM,
            ),
        })
        .collect()
}

async fn handle_replace_board_outline(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board_path = get_path(args, "board")?;
    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let expected = args["expected_plan_revision"].as_str().map(str::to_string);
    if !dry_run && expected.is_none() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_string(),
                reason: "required when dry_run is false".to_string(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }
    let contours = match parse_contours(&args["contours"]) {
        Ok(contours) => contours,
        Err(fault) => return Ok(fault.into_result()),
    };
    let roles = match check_contours(&contours) {
        Ok(roles) => roles,
        Err(fault) => return Ok(fault.into_result()),
    };

    let (live_path, live_contours, live_roles, live_expected) = (
        board_path.clone(),
        contours.clone(),
        roles.clone(),
        expected.clone(),
    );
    let attempt = attempt_ipc_write_in(
        ctx,
        &board_path,
        "board outline replacement",
        move |client, document| {
            replace_live(
                client,
                document,
                &live_path,
                &live_contours,
                &live_roles,
                live_expected.as_deref().filter(|_| !dry_run),
            )
        },
    )
    .await?;
    match attempt {
        BoardWrite::Ipc(result) => Ok(result),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) => replace_in_file(
            &board_path,
            &contours,
            &roles,
            expected.as_deref().filter(|_| !dry_run),
            &reason,
        ),
    }
}

/// The plan on the board KiCad holds, or, given the revision a dry run
/// returned, the replacement in one commit, read back.
fn replace_live(
    client: &KiCadIpcClient,
    document: kiapi::common::types::DocumentSpecifier,
    board_path: &Path,
    contours: &[Vec<Primitive>],
    roles: &[Role],
    expected: Option<&str>,
) -> anyhow::Result<CallToolResult> {
    let before = client.save_document_to_string_in(document.clone())?;
    let existing = read_edge_cuts(&before);
    let revision = plan_revision(board_path, "ipc", &before, &existing, contours)?;
    let mut body = plan_body(contours, roles, &existing, &revision, expected.is_none());
    body.insert("source".to_string(), json!("ipc"));
    let Some(expected) = expected else {
        return Ok(CallToolResult::json(&body));
    };
    if expected != revision {
        return Ok(stale_plan(board_path));
    }
    let ids: Vec<String> = existing
        .shapes
        .iter()
        .map(|shape| shape.uuid.clone())
        .collect();
    if ids.iter().any(String::is_empty) {
        return Ok(CallToolResult::error(
            "KiCad serialised an Edge.Cuts shape with no identifier, so it cannot be deleted; \
             nothing was written.",
        ));
    }
    let items: Vec<prost_types::Any> = contours.iter().flatten().map(ipc_item).collect();

    let mut stale = false;
    let committed =
        client.run_commit_recovering_in(document.clone(), "Replace board outline", |client| {
            // The gate that counts: the outline as it stands inside the commit.
            let current = client.save_document_to_string_in(document.clone())?;
            let now = plan_revision(
                board_path,
                "ipc",
                &current,
                &read_edge_cuts(&current),
                contours,
            )?;
            if now != revision {
                stale = true;
                anyhow::bail!("the board's Edge.Cuts changed before the KiCad commit");
            }
            client.delete_items_in(document.clone(), ids)?;
            client.create_items_in_returning(document.clone(), items)
        });
    if stale {
        return match committed {
            Err(error)
                if error
                    .downcast_ref::<copperpour_ipc::client::IpcOutcomeUnknown>()
                    .is_some() =>
            {
                Err(error)
            }
            Err(_) => Ok(stale_plan(board_path)),
            Ok(_) => Err(anyhow::anyhow!(
                "a stale outline plan committed; the board must be inspected"
            )),
        };
    }
    let created = committed?;
    let drawn: Vec<String> = created
        .iter()
        .filter_map(|item| {
            use prost::Message;
            kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice()).ok()
        })
        .filter_map(|shape| shape.id.map(|id| id.value))
        .collect();

    let after = match client.save_document_to_string_in(document) {
        Ok(after) => after,
        Err(error) => {
            return Ok(readback_uncertain(
                board_path,
                format!("the board could not be read back: {error:#}"),
            ))
        }
    };
    if let Some(reason) = readback_mismatch(contours, &read_edge_cuts(&after)) {
        return Ok(readback_uncertain(board_path, reason));
    }
    body.insert("drawn".to_string(), json!(drawn));
    body.insert("readback_verified".to_string(), json!(true));
    Ok(CallToolResult::json(&body))
}

/// As [`replace_live`], on the saved file when no live KiCad holds the board.
fn replace_in_file(
    board_path: &Path,
    contours: &[Vec<Primitive>],
    roles: &[Role],
    expected: Option<&str>,
    reason: &NoLiveBoard,
) -> anyhow::Result<CallToolResult> {
    let content = read_consistent(board_path)?;
    let existing = read_edge_cuts(&content);
    let revision = plan_revision(board_path, "file", &content, &existing, contours)?;
    let mut body = plan_body(contours, roles, &existing, &revision, expected.is_none());
    body.insert("source".to_string(), json!("file"));
    body.insert("fallback_reason".to_string(), reason.evidence());
    let Some(expected) = expected else {
        return Ok(CallToolResult::json(&body));
    };
    if expected != revision {
        return Ok(stale_plan(board_path));
    }

    let mut edits: Vec<SexpEdit> = existing
        .shapes
        .iter()
        .map(|shape| SexpEdit::delete(shape.span.0, shape.span.1))
        .collect();
    let close = content.rfind(')').unwrap_or(content.len());
    edits.push(SexpEdit::insert(close, format_primitives(contours)));
    let next = apply_edits(content.clone(), edits);
    if let Some(conflict) = crate::tools::replace_if_unchanged(
        board_path,
        "board",
        "replace_board_outline",
        &content,
        next,
    )? {
        return Ok(conflict);
    }

    let after = match read_consistent(board_path) {
        Ok(after) => after,
        Err(error) => {
            return Ok(readback_uncertain(
                board_path,
                format!("the board could not be read back: {error}"),
            ))
        }
    };
    let after = read_edge_cuts(&after);
    if let Some(mismatch) = readback_mismatch(contours, &after) {
        return Ok(readback_uncertain(board_path, mismatch));
    }
    body.insert(
        "drawn".to_string(),
        json!(after
            .shapes
            .iter()
            .map(|shape| shape.uuid.clone())
            .collect::<Vec<_>>()),
    );
    body.insert("readback_verified".to_string(), json!(true));
    body.insert("warning".to_string(), json!(reason.warning()));
    Ok(CallToolResult::json(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::pcb_board::board_mock::{
        board_document, ctx_talking_to, spawn_kicad_holding_board,
    };
    use std::sync::{Arc, Mutex};

    fn line(start: (f64, f64), end: (f64, f64)) -> Value {
        json!({"type": "line", "start": {"x": start.0, "y": start.1}, "end": {"x": end.0, "y": end.1}})
    }

    fn arc(start: (f64, f64), mid: (f64, f64), end: (f64, f64)) -> Value {
        json!({
            "type": "arc",
            "start": {"x": start.0, "y": start.1},
            "mid": {"x": mid.0, "y": mid.1},
            "end": {"x": end.0, "y": end.1}
        })
    }

    /// The straight edge along y = `y` from x = `x0` to `x1`, the half circle
    /// bulging towards +y.
    fn half_disc(x0: f64, x1: f64, y: f64) -> Value {
        let (centre, radius) = ((x0 + x1) / 2.0, (x1 - x0) / 2.0);
        json!([
            line((x0, y), (x1, y)),
            arc((x1, y), (centre, y + radius), (x0, y))
        ])
    }

    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
        json!([
            line((x0, y0), (x1, y0)),
            line((x1, y0), (x1, y1)),
            line((x1, y1), (x0, y1)),
            line((x0, y1), (x0, y0))
        ])
    }

    fn circle(cx: f64, cy: f64, r: f64) -> Value {
        json!([
            arc((cx - r, cy), (cx, cy - r), (cx + r, cy)),
            arc((cx + r, cy), (cx, cy + r), (cx - r, cy))
        ])
    }

    /// A rectangle with tangent corner arcs, its mid points rounded to the
    /// nanometre as a caller's would be.
    fn rounded_rectangle(x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> Value {
        let d = r * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        json!([
            line((x0 + r, y0), (x1 - r, y0)),
            arc((x1 - r, y0), (x1 - d, y0 + d), (x1, y0 + r)),
            line((x1, y0 + r), (x1, y1 - r)),
            arc((x1, y1 - r), (x1 - d, y1 - d), (x1 - r, y1)),
            line((x1 - r, y1), (x0 + r, y1)),
            arc((x0 + r, y1), (x0 + d, y1 - d), (x0, y1 - r)),
            line((x0, y1 - r), (x0, y0 + r)),
            arc((x0, y0 + r), (x0 + d, y0 + d), (x0 + r, y0))
        ])
    }

    fn checked(contours: Value) -> Result<Vec<Role>, Fault> {
        check_contours(&parse_contours(&contours)?)
    }

    fn refusal(contours: Value) -> Fault {
        checked(contours).expect_err("the contours should be refused")
    }

    #[test]
    fn a_half_disc_is_one_outline() {
        assert_eq!(
            checked(json!([half_disc(0.0, 100.0, 0.0)])).unwrap(),
            [Role::Outline]
        );
    }

    #[test]
    fn nesting_alternates_outline_and_cutout_as_kicad_reads_it() {
        let roles = checked(json!([
            rectangle(0.0, 0.0, 100.0, 100.0),
            circle(50.0, 50.0, 20.0),
            rectangle(45.0, 45.0, 55.0, 55.0),
            rectangle(200.0, 0.0, 250.0, 50.0)
        ]))
        .unwrap();
        assert_eq!(
            roles,
            [Role::Outline, Role::Cutout, Role::Outline, Role::Outline]
        );
    }

    #[test]
    fn tangent_corners_meet_only_at_their_joints() {
        assert_eq!(
            checked(json!([rounded_rectangle(0.0, 0.0, 80.0, 50.0, 3.0)])).unwrap(),
            [Role::Outline]
        );
        assert_eq!(
            checked(json!([rounded_rectangle(10.0, 10.0, 12.0, 11.0, 0.4)])).unwrap(),
            [Role::Outline]
        );
    }

    #[test]
    fn a_nanometre_gap_at_a_joint_is_refused() {
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 0.0)),
            line((10.000001, 0.0), (10.0, 10.0)),
            line((10.0, 10.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][1].start");
        assert!(
            fault
                .reason
                .contains("0.000001 mm from where contours[0][0] ends"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn an_open_contour_is_refused() {
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 0.0)),
            line((10.0, 0.0), (10.0, 10.0)),
            line((10.0, 10.0), (0.0, 0.5))
        ]]));
        assert_eq!(fault.field, "contours[0][0].start");
        assert!(fault.reason.contains("does not close"), "{}", fault.reason);
    }

    #[test]
    fn a_line_of_kicads_minimum_or_less_is_refused() {
        let sliver = |side: f64| {
            json!([[
                line((0.0, 0.0), (10.0, 0.0)),
                line((10.0, 0.0), (10.0, side)),
                line((10.0, side), (0.0, 0.0))
            ]])
        };
        let fault = refusal(sliver(0.001));
        assert_eq!(fault.field, "contours[0][1]");
        assert!(fault.reason.contains("0.001 mm long"), "{}", fault.reason);
        assert!(checked(sliver(0.0011)).is_ok());
    }

    #[test]
    fn an_arc_of_kicads_minimum_or_less_is_refused() {
        let fault = refusal(json!([[
            arc((0.0, 0.0), (0.0004, 0.0002), (0.0008, 0.0)),
            line((0.0008, 0.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][0]");
        assert!(
            fault.reason.contains("0.000894 mm start to mid to end"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn an_arc_is_measured_through_the_mid_point_given() {
        // Through the arc's own midpoint it would measure 0.00108 mm, but
        // KiCad keeps this mid and measures through it.
        let fault = refusal(json!([[
            arc((0.0, 0.0), (0.000028, 0.000056), (0.0009, 0.0)),
            line((0.0009, 0.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][0]");
        assert!(
            fault.reason.contains("0.000937 mm start to mid to end"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn an_arc_whose_centre_kicad_cannot_hold_is_refused() {
        // A nanometre off a straight line: its centre is about 1e17 mm away,
        // and chords within 0.001 mm of it would never end.
        let fault = refusal(json!([[
            arc((0.0, 0.0), (50.0, 0.000001), (100.000001, 0.000002)),
            line((100.000001, 0.000002), (100.0, 10.0)),
            line((100.0, 10.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][0]");
        assert!(
            fault.reason.contains("centre lies beyond"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn an_arc_through_three_points_on_a_line_is_refused() {
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 0.0)),
            arc((10.0, 0.0), (5.0, 0.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][1]");
        assert!(fault.reason.contains("lie on one line"), "{}", fault.reason);
    }

    #[test]
    fn a_coordinate_kicad_would_clamp_is_refused() {
        let fault = refusal(json!([rectangle(0.0, 0.0, 3000.0, 10.0)]));
        assert_eq!(fault.field, "contours[0][0].end.x");
        assert!(fault.reason.contains("2147.483637"), "{}", fault.reason);
    }

    #[test]
    fn a_contour_crossing_itself_is_refused() {
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 10.0)),
            line((10.0, 10.0), (10.0, 0.0)),
            line((10.0, 0.0), (0.0, 10.0)),
            line((0.0, 10.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][0]");
        assert!(
            fault.reason.contains("contours[0][2] at (5, 5)"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn overlapping_contours_are_refused() {
        let fault = refusal(json!([
            rectangle(0.0, 0.0, 10.0, 10.0),
            rectangle(5.0, 5.0, 15.0, 15.0)
        ]));
        assert!(
            fault.reason.contains("crosses or touches contours[1]"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn a_cutout_touching_the_outline_is_refused() {
        let fault = refusal(json!([
            rectangle(0.0, 0.0, 100.0, 100.0),
            [
                line((50.0, 0.0), (60.0, 10.0)),
                line((60.0, 10.0), (40.0, 10.0)),
                line((40.0, 10.0), (50.0, 0.0))
            ]
        ]));
        assert_eq!(fault.field, "contours[0][0]");
        assert!(fault.reason.contains("at (50, 0)"), "{}", fault.reason);
    }

    #[test]
    fn a_line_doubling_back_is_refused_and_a_straight_run_is_not() {
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 0.0)),
            line((10.0, 0.0), (5.0, 0.0)),
            line((5.0, 0.0), (5.0, 5.0)),
            line((5.0, 5.0), (0.0, 0.0))
        ]]));
        assert_eq!(fault.field, "contours[0][0]");
        let fault = refusal(json!([[
            line((0.0, 0.0), (10.0, 0.0)),
            line((10.0, 0.0), (0.0, 0.0))
        ]]));
        assert!(
            fault.reason.contains("touches contours[0][1]"),
            "{}",
            fault.reason
        );
        assert!(checked(json!([[
            line((0.0, 0.0), (5.0, 0.0)),
            line((5.0, 0.0), (10.0, 0.0)),
            line((10.0, 0.0), (10.0, 10.0)),
            line((10.0, 10.0), (0.0, 0.0))
        ]]))
        .is_ok());
    }

    #[test]
    fn a_contour_poking_through_an_arc_is_refused() {
        let fault = refusal(json!([
            half_disc(-50.0, 50.0, 0.0),
            rectangle(-1.0, 45.0, 1.0, 55.0)
        ]));
        assert!(
            fault.field == "contours[0][1]" && fault.reason.contains("contours[1]"),
            "{fault:?}"
        );
    }

    #[test]
    fn a_crossing_ten_micrometres_deep_into_an_arc_is_found() {
        // The arc tops out at y = 50; the rectangle's lower edge sits 0.01 mm
        // inside it, so its sides cross the arc about 7.5 µm in.
        let fault = refusal(json!([
            half_disc(-50.0, 50.0, 0.0),
            rectangle(-0.5, 49.99, 0.5, 60.0)
        ]));
        assert!(fault.reason.contains("contours[1]"), "{fault:?}");
        assert!(checked(json!([
            half_disc(-50.0, 50.0, 0.0),
            rectangle(-0.5, 50.01, 0.5, 60.0)
        ]))
        .is_ok());
    }

    #[test]
    fn the_box_takes_in_an_arcs_bulge() {
        let contours = parse_contours(&json!([half_disc(10.0, 30.0, 5.0)])).unwrap();
        let (x0, y0, x1, y1) = bounds(&contours);
        assert_eq!((x0, y0, x1, y1), (10e6, 5e6, 30e6, 15e6));
    }

    #[test]
    fn millimetres_are_written_exactly() {
        assert_eq!(mm_text(299_999), "0.299999");
        assert_eq!(mm_text(-1_500_000), "-1.5");
        assert_eq!(mm_text(2_000_000), "2");
        assert_eq!(
            parse_point(&json!({"x": 0.3, "y": -0.0000004}), "p").unwrap(),
            (300_000, 0)
        );
    }

    #[test]
    fn a_read_back_arc_may_be_reversed_with_any_mid_point_on_it() {
        // A half circle about (5, 0) mm through (8, 4) mm.
        let drawn = Primitive::Arc {
            start: (0, 0),
            mid: (8_000_000, 4_000_000),
            end: (10_000_000, 0),
        };
        let reversed = |mid| Primitive::Arc {
            start: (10_000_000, 0),
            mid,
            end: (0, 0),
        };
        assert!(same_shape(&drawn, &reversed((5_000_000, 5_000_000))));
        assert!(same_shape(
            &drawn,
            &reversed((5_000_000, 5_000_000 + ARC_MID_TOLERANCE_NM))
        ));
        assert!(!same_shape(
            &drawn,
            &reversed((5_000_000, 5_000_000 + ARC_MID_TOLERANCE_NM + 1))
        ));
        assert!(
            !same_shape(&drawn, &reversed((5_000_000, -5_000_000))),
            "the other half"
        );
    }

    // ─── On a saved board ────────────────────────────────────────────────────

    /// A board KiCad 10.0.6 made and saved: a rounded outline, a footprint
    /// with a slot on Edge.Cuts, and texts on F.SilkS and Edge.Cuts.
    const BOARD: &str = include_str!("../../tests/fixtures/board_outline_kicad10.kicad_pcb");

    /// The outline's four lines and four arcs in `BOARD`, in file order.
    const OUTLINE: [&str; 8] = [
        "80edbc7e-cfe2-4a25-8a20-50f6482b355a",
        "d65cb623-1a39-4ce1-b085-f6d28c25ed89",
        "5c5a0563-f438-47a3-b716-ae29a96ff9cb",
        "cb0492d0-0236-402c-8517-3004c6fffded",
        "9381393c-54d8-41c0-996d-bbd7bff0976a",
        "fcda717d-f245-4248-9001-6b9aaa541cab",
        "844e3a41-ac63-45da-ae74-5da2309a8ae2",
        "20fade57-bab5-4d53-b70f-de40265164a6",
    ];

    /// What the tool must leave on `BOARD`: the footprint's Edge.Cuts slot,
    /// the silkscreen text and the text on Edge.Cuts.
    const KEPT: [&str; 3] = [
        "43d37a15-8750-41b9-9cc8-721c18b000e4",
        "323f9e8d-3648-434d-8fec-16fa74d87abb",
        "a29de0ab-5535-43c9-a5ee-8839b9ac95ff",
    ];

    /// `BOARD` with one outline line moved 1 mm.
    fn moved_board() -> String {
        let moved = BOARD.replace("(end 97 0)", "(end 97 1)");
        assert_ne!(moved, BOARD);
        moved
    }

    fn board_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, content).unwrap();
        (dir, board)
    }

    fn body(result: &CallToolResult) -> Value {
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }))
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    async fn call(args: Value, ctx: &ToolContext) -> CallToolResult {
        handle_replace_board_outline(&args, ctx).await.unwrap()
    }

    fn file_ctx() -> ToolContext {
        ctx_talking_to(String::new())
    }

    #[tokio::test]
    async fn a_dry_run_lists_the_outline_and_writes_nothing() {
        let (_dir, board) = board_file(BOARD);
        let result = call(
            json!({"board": board, "contours": [half_disc(0.0, 100.0, 0.0)]}),
            &file_ctx(),
        )
        .await;
        assert!(!result.is_error, "{}", body(&result));
        let answer = body(&result);
        assert_eq!(answer["dry_run"], true);
        assert_eq!(answer["source"], "file");
        assert_eq!(answer["replaced_count"], 8);
        assert_eq!(answer["replaced"][0]["uuid"], OUTLINE[0]);
        assert_eq!(answer["footprint_edge_cuts_count"], 1);
        assert_eq!(
            answer["contours"],
            json!([{"role": "outline", "line_count": 1, "arc_count": 1}])
        );
        assert_eq!(
            (answer["x_min"].clone(), answer["y_max"].clone()),
            (json!(0.0), json!(50.0))
        );
        assert!(answer["plan_revision"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert!(answer.get("drawn").is_none());
        assert_eq!(std::fs::read_to_string(&board).unwrap(), BOARD);
    }

    #[tokio::test]
    async fn an_apply_replaces_only_the_boards_edge_cuts_shapes() {
        let (_dir, board) = board_file(BOARD);
        let ctx = file_ctx();
        let contours = json!([half_disc(0.0, 100.0, 0.0), circle(20.0, 20.0, 0.3)]);
        let plan = body(&call(json!({"board": board, "contours": contours}), &ctx).await);
        let result = call(
            json!({
                "board": board,
                "contours": contours,
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"]
            }),
            &ctx,
        )
        .await;
        assert!(!result.is_error, "{}", body(&result));
        let answer = body(&result);
        assert_eq!(answer["readback_verified"], true);
        assert_eq!(answer["drawn"].as_array().unwrap().len(), 4);
        assert_eq!(answer["contours"][1]["role"], "cutout");
        assert!(answer["warning"].is_string());

        let after = std::fs::read_to_string(&board).unwrap();
        for gone in OUTLINE {
            assert!(!after.contains(gone), "{gone} survived: {after}");
        }
        for kept in KEPT {
            assert!(after.contains(kept), "{kept} was removed: {after}");
        }
        assert!(after.contains("(start 20.3 20)"), "{after}");
        let edge = read_edge_cuts(&after);
        assert_eq!(edge.shapes.len(), 4);
        assert_eq!(edge.footprint_shapes, 1);
        parse_sexp(&after).expect("the board still parses");
    }

    #[tokio::test]
    async fn an_apply_needs_the_dry_runs_revision() {
        let (_dir, board) = board_file(BOARD);
        let result = call(
            json!({"board": board, "contours": [half_disc(0.0, 100.0, 0.0)], "dry_run": false}),
            &file_ctx(),
        )
        .await;
        assert!(result.is_error);
        assert!(body(&result).to_string().contains("expected_plan_revision"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), BOARD);
    }

    #[tokio::test]
    async fn a_stale_revision_writes_nothing() {
        let (_dir, board) = board_file(BOARD);
        let ctx = file_ctx();
        let contours = json!([half_disc(0.0, 100.0, 0.0)]);
        let plan = body(&call(json!({"board": board, "contours": contours}), &ctx).await);
        let moved = moved_board();
        std::fs::write(&board, &moved).unwrap();
        let result = call(
            json!({
                "board": board,
                "contours": contours,
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"]
            }),
            &ctx,
        )
        .await;
        assert!(result.is_error);
        assert!(
            body(&result).to_string().contains("stale"),
            "{}",
            body(&result)
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), moved);
    }

    #[tokio::test]
    async fn a_refused_contour_writes_nothing() {
        let (_dir, board) = board_file(BOARD);
        let result = call(
            json!({
                "board": board,
                "contours": [[line((0.0, 0.0), (10.0, 0.0)), line((10.0, 0.0), (0.0, 1.0))]],
                "dry_run": false,
                "expected_plan_revision": "sha256:any"
            }),
            &file_ctx(),
        )
        .await;
        assert!(result.is_error);
        assert!(body(&result).to_string().contains("contours[0][0].start"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), BOARD);
    }

    /// KiCad's DRC reads what the tool writes over `BOARD`'s outline as a
    /// valid outline, and KiCad's own load and save, which stores each arc as
    /// creating one over IPC does, keeps every primitive within the
    /// readback's tolerance.
    ///
    ///     KICAD_CLI=… cargo test -p copperpour-core --lib kicad_reads_the_written -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real kicad-cli on PATH"]
    async fn kicad_reads_the_written_outlines_as_drawn() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        let notched = json!([
            line((0.0, 0.0), (20.0, 0.0)),
            arc((20.0, 0.0), (30.0, 10.0), (40.0, 0.0)),
            line((40.0, 0.0), (60.0, 0.0)),
            line((60.0, 0.0), (60.0, 40.0)),
            line((60.0, 40.0), (0.0, 40.0)),
            line((0.0, 40.0), (0.0, 0.0))
        ]);
        let uneven = json!([
            line((0.0, 0.0), (20.0, 0.0)),
            arc((20.0, 0.0), (31.234567, 7.654321), (40.0, 0.0)),
            line((40.0, 0.0), (60.0, 0.0)),
            line((60.0, 0.0), (60.0, 40.0)),
            arc((60.0, 40.0), (29.876543, 47.123457), (0.0, 40.0)),
            line((0.0, 40.0), (0.0, 0.0))
        ]);
        let cases = [
            ("half disc", json!([half_disc(0.0, 100.0, 0.0)])),
            ("notch", json!([notched])),
            ("uneven arcs", json!([uneven])),
            (
                "rounded with a hole",
                json!([
                    rounded_rectangle(0.0, 0.0, 80.0, 50.0, 3.0),
                    circle(10.0, 10.0, 1.6)
                ]),
            ),
            (
                "island in a cut-out, and a second board",
                json!([
                    rectangle(0.0, 0.0, 100.0, 100.0),
                    circle(50.0, 50.0, 20.0),
                    rectangle(45.0, 45.0, 55.0, 55.0),
                    half_disc(120.0, 160.0, 0.0)
                ]),
            ),
        ];
        for (name, contours) in cases {
            let (_dir, board) = board_file(BOARD);
            let ctx = file_ctx();
            let plan = body(&call(json!({"board": board, "contours": contours}), &ctx).await);
            let applied = call(
                json!({
                    "board": board,
                    "contours": contours,
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"]
                }),
                &ctx,
            )
            .await;
            assert!(!applied.is_error, "{name}: {}", body(&applied));

            let report = crate::tools::cli::run_drc(&cli, &board, false)
                .await
                .expect("kicad-cli pcb drc");
            let malformed: Vec<String> = report
                .all()
                .filter(|violation| violation.rule == "invalid_outline")
                .map(|violation| violation.description.clone())
                .collect();
            assert!(malformed.is_empty(), "{name}: {malformed:?}");

            let saved = std::process::Command::new(&cli)
                .args(["pcb", "upgrade", "--force"])
                .arg(&board)
                .output()
                .expect("kicad-cli pcb upgrade");
            assert!(saved.status.success(), "{name}: {saved:?}");
            let drawn = parse_contours(&contours).unwrap();
            let after = read_edge_cuts(&std::fs::read_to_string(&board).unwrap());
            for primitive in drawn.iter().flatten() {
                let Primitive::Arc { start, mid, end } = *primitive else {
                    continue;
                };
                let circle = ArcCircle::of(start, mid, end);
                let held: Vec<(i64, i64, f64, bool)> = after
                    .shapes
                    .iter()
                    .filter_map(|shape| match shape.geometry {
                        Some(Primitive::Arc {
                            start: s,
                            mid: m,
                            end: e,
                        }) if (s, e) == (start, end) || (s, e) == (end, start) => {
                            let off = (m.0 as f64 - circle.center.0)
                                .hypot(m.1 as f64 - circle.center.1)
                                - circle.radius;
                            Some((m.0 - mid.0, m.1 - mid.1, off, s == end))
                        }
                        _ => None,
                    })
                    .collect();
                eprintln!(
                    "{name}: arc {start:?} → {end:?}, held as (mid moved by, nm off the circle, reversed) {held:?}"
                );
            }
            assert_eq!(readback_mismatch(&drawn, &after), None, "{name}");
        }
    }

    // ─── On a board KiCad holds ──────────────────────────────────────────────

    /// The board text a mock KiCad holds.
    struct Held {
        text: String,
        /// `(n, text)`: after `n` more reads, KiCad holds `text`, as when
        /// the user edits the board between Copperpour's reads.
        change: Option<(usize, String)>,
    }

    /// A KiCad holding `board` whose text is `held`: it serialises that text,
    /// deletes blocks by UUID and appends created shapes the way KiCad 10
    /// may write them, an arc reversed and its mid point moved `mid_shift`
    /// nanometres in y, off the top of a half disc's arc. `log` records each
    /// command.
    fn spawn_kicad(
        board: &Path,
        held: Arc<Mutex<Held>>,
        log: Arc<Mutex<Vec<String>>>,
        mid_shift: i64,
    ) -> crate::test_support::MockIpcServer {
        use kiapi::common::commands;
        use prost::Message;
        let document = board_document(&board.to_string_lossy());
        spawn_kicad_holding_board(board, move |command| {
            let url = command.type_url.rsplit('.').next().unwrap().to_string();
            log.lock().unwrap().push(url.clone());
            let mut held = held.lock().unwrap();
            let held = &mut *held;
            let text = &mut held.text;
            match url.as_str() {
                "SaveDocumentToString" => {
                    match held.change.take() {
                        Some((0, next)) => *text = next,
                        Some((reads, next)) => held.change = Some((reads - 1, next)),
                        None => {}
                    }
                    Some(builders::pack_any(
                        &commands::SavedDocumentResponse {
                            document: Some(document.clone()),
                            contents: text.clone(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ))
                }
                "BeginCommit" => Some(builders::pack_any(
                    &commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "commit-1".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                )),
                "EndCommit" => Some(builders::pack_any(
                    &commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                )),
                "DeleteItems" => {
                    let request = commands::DeleteItems::decode(command.value.as_slice()).unwrap();
                    for id in request.item_ids {
                        let shape = read_edge_cuts(text)
                            .shapes
                            .into_iter()
                            .find(|shape| shape.uuid == id.value)
                            .expect("a held shape");
                        text.replace_range(shape.span.0..shape.span.1, "");
                    }
                    Some(builders::pack_any(
                        &commands::DeleteItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            deleted_items: vec![],
                        },
                        "kiapi.common.commands.DeleteItemsResponse",
                    ))
                }
                "CreateItems" => {
                    use kiapi::common::types::graphic_shape::Geometry;
                    let request = commands::CreateItems::decode(command.value.as_slice()).unwrap();
                    let mut created = Vec::new();
                    for (n, item) in request.items.into_iter().enumerate() {
                        let mut shape =
                            kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice())
                                .unwrap();
                        let uuid = format!("created-{n}");
                        let xy = |v: &Option<kiapi::common::types::Vector2>| {
                            let v = v.as_ref().unwrap();
                            format!("{} {}", mm_text(v.x_nm), mm_text(v.y_nm))
                        };
                        let block = match shape.shape.as_ref().and_then(|s| s.geometry.as_ref()).unwrap() {
                            Geometry::Segment(s) => format!(
                                "\n\t(gr_line\n\t\t(start {})\n\t\t(end {})\n\t\t(layer \"Edge.Cuts\")\n\t\t(uuid \"{uuid}\")\n\t)",
                                xy(&s.start), xy(&s.end)
                            ),
                            Geometry::Arc(a) => {
                                let mid = a.mid.as_ref().unwrap();
                                format!(
                                    "\n\t(gr_arc\n\t\t(start {})\n\t\t(mid {} {})\n\t\t(end {})\n\t\t(layer \"Edge.Cuts\")\n\t\t(uuid \"{uuid}\")\n\t)",
                                    xy(&a.end), mm_text(mid.x_nm), mm_text(mid.y_nm + mid_shift), xy(&a.start)
                                )
                            }
                            other => panic!("unexpected shape {other:?}"),
                        };
                        let close = text.rfind(')').unwrap();
                        text.insert_str(close, &block);
                        shape.id = Some(kiapi::common::types::Kiid { value: uuid });
                        created.push(commands::ItemCreationResult {
                            status: Some(commands::ItemStatus {
                                code: commands::ItemStatusCode::IscOk as i32,
                                error_message: String::new(),
                            }),
                            item: Some(builders::pack_any(
                                &shape,
                                "kiapi.board.types.BoardGraphicShape",
                            )),
                        });
                    }
                    Some(builders::pack_any(
                        &commands::CreateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            created_items: created,
                        },
                        "kiapi.common.commands.CreateItemsResponse",
                    ))
                }
                _ => None,
            }
        })
    }

    struct Live {
        _dir: tempfile::TempDir,
        board: std::path::PathBuf,
        held: Arc<Mutex<Held>>,
        log: Arc<Mutex<Vec<String>>>,
        _server: crate::test_support::MockIpcServer,
        ctx: ToolContext,
    }

    fn live(mid_shift: i64) -> Live {
        let (dir, board) = board_file("(kicad_pcb)\n");
        let held = Arc::new(Mutex::new(Held {
            text: BOARD.to_string(),
            change: None,
        }));
        let log = Arc::new(Mutex::new(Vec::new()));
        let server = spawn_kicad(&board, held.clone(), log.clone(), mid_shift);
        let ctx = ctx_talking_to(server.address().to_string());
        Live {
            _dir: dir,
            board,
            held,
            log,
            _server: server,
            ctx,
        }
    }

    async fn plan_then_apply(live: &Live, contours: &Value) -> CallToolResult {
        let plan = call(
            json!({"board": live.board, "contours": contours}),
            &live.ctx,
        )
        .await;
        assert!(!plan.is_error, "{}", body(&plan));
        call(
            json!({
                "board": live.board,
                "contours": contours,
                "dry_run": false,
                "expected_plan_revision": body(&plan)["plan_revision"]
            }),
            &live.ctx,
        )
        .await
    }

    #[tokio::test]
    async fn a_live_dry_run_reads_the_board_kicad_holds() {
        let live = live(0);
        let result = call(
            json!({"board": live.board, "contours": [half_disc(0.0, 100.0, 0.0)]}),
            &live.ctx,
        )
        .await;
        let answer = body(&result);
        assert_eq!(answer["source"], "ipc", "{answer}");
        assert_eq!(answer["replaced_count"], 8);
        let log = live.log.lock().unwrap();
        assert!(
            !log.iter().any(|command| command == "BeginCommit"),
            "{log:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&live.board).unwrap(),
            "(kicad_pcb)\n"
        );
    }

    #[tokio::test]
    async fn a_live_apply_is_one_commit_read_back() {
        let live = live(3);
        let result = plan_then_apply(&live, &json!([half_disc(0.0, 100.0, 0.0)])).await;
        let answer = body(&result);
        assert!(!result.is_error, "{answer}");
        assert_eq!(answer["source"], "ipc");
        assert_eq!(answer["readback_verified"], true);
        assert_eq!(answer["drawn"], json!(["created-0", "created-1"]));
        let log = live.log.lock().unwrap();
        let commits: Vec<&String> = log
            .iter()
            .filter(|command| {
                matches!(
                    command.as_str(),
                    "BeginCommit" | "DeleteItems" | "CreateItems" | "EndCommit"
                )
            })
            .collect();
        assert_eq!(
            commits,
            ["BeginCommit", "DeleteItems", "CreateItems", "EndCommit"]
        );
        let held = &live.held.lock().unwrap().text;
        for gone in OUTLINE {
            assert!(!held.contains(gone), "{gone} survived: {held}");
        }
        for kept in KEPT {
            assert!(held.contains(kept), "{kept} was removed: {held}");
        }
    }

    #[tokio::test]
    async fn a_live_read_back_that_differs_is_reported_uncertain() {
        let live = live(ARC_MID_TOLERANCE_NM + 5);
        let result = plan_then_apply(&live, &json!([half_disc(0.0, 100.0, 0.0)])).await;
        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain"),
            "{}",
            body(&result)
        );
    }

    #[tokio::test]
    async fn a_live_outline_changed_before_the_commit_is_stale() {
        let live = live(0);
        let contours = json!([half_disc(0.0, 100.0, 0.0)]);
        let plan = body(
            &call(
                json!({"board": live.board, "contours": contours}),
                &live.ctx,
            )
            .await,
        );
        live.held.lock().unwrap().text = moved_board();
        let result = call(
            json!({
                "board": live.board,
                "contours": contours,
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"]
            }),
            &live.ctx,
        )
        .await;
        assert!(result.is_error);
        assert!(
            body(&result).to_string().contains("stale"),
            "{}",
            body(&result)
        );
        let log = live.log.lock().unwrap();
        assert!(
            !log.iter().any(|command| command == "BeginCommit"),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn an_outline_changed_inside_the_commit_is_stale_and_dropped() {
        let live = live(0);
        let contours = json!([half_disc(0.0, 100.0, 0.0)]);
        let plan = body(
            &call(
                json!({"board": live.board, "contours": contours}),
                &live.ctx,
            )
            .await,
        );
        // The apply reads before the commit, again for the recovery snapshot,
        // and then inside the commit, where the outline has changed.
        live.held.lock().unwrap().change = Some((2, moved_board()));
        let result = call(
            json!({
                "board": live.board,
                "contours": contours,
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"]
            }),
            &live.ctx,
        )
        .await;
        assert!(result.is_error);
        assert!(
            body(&result).to_string().contains("stale"),
            "{}",
            body(&result)
        );
        let log = live.log.lock().unwrap();
        assert!(
            log.iter().any(|command| command == "BeginCommit"),
            "{log:?}"
        );
        assert!(
            !log.iter().any(|command| command == "DeleteItems"),
            "{log:?}"
        );
    }
}
