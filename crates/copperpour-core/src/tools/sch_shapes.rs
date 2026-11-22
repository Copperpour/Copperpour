//! `add_schematic_shapes` and `list_schematic_shapes`: the graphic lines,
//! polylines, rectangles and circles a sheet draws, which carry no electrical
//! meaning.
//!
//! The arguments use the primitive vocabulary `set_footprint_graphics` and
//! `create_symbol` share (#501): `line`, `poly`, `rect` and `circle`, points as
//! `{x, y}`, `stroke_width_mm`. KiCad writes a graphic line as a two-point
//! `(polyline …)`, so `line` and `poly` are both polylines in the file, and the
//! list reports a two-point polyline as a `line`. Every shape of one call is
//! written by one `SchematicCommand`, committed through the guard
//! `edit_schematic_text` uses, so one bad shape writes none.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_graphics::{
    common_shape_properties_with_fill, point_schema, primitive_schema,
};
use crate::tools::sch_batch::schematic_text_color;
use crate::tools::sch_texts::{commit_checked, read_parsed};
use crate::tools::sch_wiring::find_first_symbol_instance;
use crate::tools::{get_path, invalid_arg, ToolContext, ToolDef};
use copperpour_sexp::writer::{find_direct_child_blocks, new_uuid};
use copperpour_sexp::{
    parse_sexp, DocumentRevision, ItemAnchor, ItemChange, ItemId, SchematicCommand, SexpNode,
};
use serde_json::{json, Map, Value};
use std::path::Path;

const ADD: &str = "add_schematic_shapes";
const LIST: &str = "list_schematic_shapes";

/// KiCad's schematic unit is 100 nm, so it saves a coordinate to four decimals
/// of a millimetre: 1.123456 reads back as 1.1235.
const DECIMALS: f64 = 10_000.0;

/// The largest coordinate KiCad holds: `i32::MAX` units of 100 nm.
const MAX_MM: f64 = 214_748.364_7;

const LINE_STYLES: &[&str] = &[
    "default",
    "solid",
    "dash",
    "dot",
    "dash_dot",
    "dash_dot_dot",
];

/// The fills this tool writes: nothing, the sheet's background colour, or a
/// colour of its own. KiCad 10 saves a sheet shape's `(fill (type outline))`
/// as a colour fill in the stroke's colour, so `outline` would not read back as
/// written; its hatch fills are listed but not written.
const FILLS: &[&str] = &["none", "background", "color"];

fn color_schema(what: &str) -> Value {
    json!({
        "type": "array",
        "description": format!(
            "{what} as KiCad writes it: [r, g, b] or [r, g, b, a], channels 0-255 and alpha \
             0-1 (default 1)."
        ),
        "items": { "type": "number" },
        "minItems": 3,
        "maxItems": 4
    })
}

/// The shared shape properties of `set_footprint_graphics` and
/// `create_symbol`, `stroke_width_mm` and `fill`, with a sheet's fills, plus
/// the stroke style and colours a sheet shape carries.
fn sheet_shape_properties() -> Map<String, Value> {
    let mut properties = common_shape_properties_with_fill(FILLS);
    properties["stroke_width_mm"]["description"] =
        json!("Stroke width in millimetres; 0 (the default) draws the sheet's default width");
    properties["fill"]["description"] = json!(
        "'none' (the default), 'background' (the sheet's background colour) or 'color', which \
         takes fill_color"
    );
    properties.insert(
        "stroke_type".to_owned(),
        json!({
            "type": "string",
            "enum": LINE_STYLES,
            "description": "Line style (default 'default', the sheet's own)"
        }),
    );
    properties.insert(
        "stroke_color".to_owned(),
        color_schema("Stroke colour; omitted, the sheet's default colour"),
    );
    properties.insert(
        "fill_color".to_owned(),
        color_schema("The fill colour, with fill 'color' only"),
    );
    properties
}

/// The properties a line takes: the stroke, no fill.
fn stroke_properties() -> Map<String, Value> {
    let mut properties = sheet_shape_properties();
    properties.remove("fill");
    properties.remove("fill_color");
    properties
}

/// The keys each shape type accepts, and of them the ones it requires.
fn shape_keys(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    const STROKE: [&str; 3] = ["stroke_width_mm", "stroke_type", "stroke_color"];
    Some(match kind {
        "line" => (
            &["type", "start", "end", STROKE[0], STROKE[1], STROKE[2]],
            &["type", "start", "end"],
        ),
        "poly" => (
            &[
                "type",
                "points",
                STROKE[0],
                STROKE[1],
                STROKE[2],
                "fill",
                "fill_color",
            ],
            &["type", "points"],
        ),
        "rect" => (
            &[
                "type",
                "start",
                "end",
                STROKE[0],
                STROKE[1],
                STROKE[2],
                "fill",
                "fill_color",
            ],
            &["type", "start", "end"],
        ),
        "circle" => (
            &[
                "type",
                "center",
                "radius_mm",
                STROKE[0],
                STROKE[1],
                STROKE[2],
                "fill",
                "fill_color",
            ],
            &["type", "center", "radius_mm"],
        ),
        _ => return None,
    })
}

fn shapes_schema() -> Value {
    let mut line = stroke_properties();
    line.insert("start".to_owned(), point_schema());
    line.insert("end".to_owned(), point_schema());

    let mut poly = sheet_shape_properties();
    poly.insert(
        "points".to_owned(),
        json!({ "type": "array", "minItems": 3, "items": point_schema() }),
    );

    let mut rect = sheet_shape_properties();
    rect.insert("start".to_owned(), point_schema());
    rect.insert("end".to_owned(), point_schema());

    let mut circle = sheet_shape_properties();
    circle.insert("center".to_owned(), point_schema());
    circle.insert(
        "radius_mm".to_owned(),
        json!({ "type": "number", "exclusiveMinimum": 0, "description": "Circle radius in millimetres" }),
    );

    let branch =
        |kind: &str, properties| primitive_schema(kind, properties, shape_keys(kind).unwrap().1);
    json!({
        "type": "array",
        "minItems": 1,
        "description": "The shapes to add, in sheet millimetres: line {start, end}, poly {points: \
                        three or more}, rect {start, end} or circle {center, radius_mm}. A poly \
                        is open; repeat its first point last to close it.",
        "items": {
            "oneOf": [
                branch("line", line),
                branch("poly", poly),
                branch("rect", rect),
                branch("circle", circle)
            ]
        }
    })
}

pub(super) fn add_tool() -> ToolDef {
    tool!(
        "add_schematic_shapes",
        "Add graphic lines, polylines, rectangles and circles to a sheet, all in one write; they \
         carry no electrical meaning, so they draw boxes, dividers and callouts. Same shape types \
         and geometry keys as set_footprint_graphics (line, poly, rect, circle; points as {x, y}; \
         stroke_width_mm), plus stroke_type, stroke_color, and a sheet's fills (none, background \
         or color with fill_color) for poly, rect and circle. One \
         bad shape refuses the call and writes nothing. Pass revision from \
         list_schematic_shapes to refuse the write if the sheet changed since. Refused while \
         KiCad holds the sheet or its project's root schematic open. Reports each shape as \
         read back from the file, with its uuid, and the sheet's new revision. Text goes \
         through add_schematic_text; batch_delete removes a shape by uuid.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                "shapes": shapes_schema(),
                "revision": {
                    "type": "string",
                    "description": "Optional: the revision list_schematic_shapes or list_schematic_texts returned; the write is refused if the sheet no longer has it"
                }
            },
            "required": ["schematic", "shapes"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_add_schematic_shapes(args, ctx).await }
    )
}

pub(super) fn list_tool() -> ToolDef {
    tool!(
        "list_schematic_shapes",
        "List a sheet's graphic shapes: lines, polylines, rectangles, circles, arcs and Bezier \
         curves, each with its uuid, geometry, stroke and fill, in add_schematic_shapes's \
         vocabulary (a two-point polyline is a line). Wires, buses, text and the shapes inside \
         library symbols are not listed. Returns the sheet's revision, which add_schematic_shapes \
         takes.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" }
            },
            "required": ["schematic"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_list_schematic_shapes(args, ctx).await }
    )
}

/// A shape to write, checked and in KiCad's units.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    /// A line (two points, never filled) or a poly (three or more): both
    /// are polylines.
    Polyline {
        points: Vec<(f64, f64)>,
        stroke: Stroke,
        fill: Fill,
    },
    Rect {
        start: (f64, f64),
        end: (f64, f64),
        stroke: Stroke,
        fill: Fill,
    },
    Circle {
        center: (f64, f64),
        radius: f64,
        stroke: Stroke,
        fill: Fill,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Stroke {
    width: f64,
    style: String,
    /// `(color R G B A)`, or `None` for the sheet's default.
    color: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Fill {
    None,
    Background,
    /// `(color R G B A)`.
    Color(String),
}

/// `value` as KiCad will hold it, rounded to its 100 nm unit, or why not.
pub(super) fn millimetres(value: &Value, field: &str) -> Result<f64, CallToolResult> {
    let raw = value
        .as_f64()
        .ok_or_else(|| invalid_arg(field, "missing or not a number"))?;
    let rounded = (raw * DECIMALS).round() / DECIMALS + 0.0;
    if !rounded.is_finite() || rounded.abs() > MAX_MM {
        return Err(invalid_arg(
            field,
            &format!("must be a finite number of millimetres within ±{MAX_MM}"),
        ));
    }
    Ok(rounded)
}

fn point(shape: &Value, key: &str, at: &str) -> Result<(f64, f64), CallToolResult> {
    point_at(&shape[key], &format!("{at}.{key}"))
}

/// `value` as a point `{x, y}`, its field named `field` in a refusal.
fn point_at(value: &Value, field: &str) -> Result<(f64, f64), CallToolResult> {
    let Some(object) = value.as_object() else {
        return Err(invalid_arg(field, "missing or not an object {x, y}"));
    };
    if let Some(unknown) = object.keys().find(|key| !matches!(key.as_str(), "x" | "y")) {
        return Err(invalid_arg(&format!("{field}.{unknown}"), "unknown key"));
    }
    Ok((
        millimetres(&value["x"], &format!("{field}.x"))?,
        millimetres(&value["y"], &format!("{field}.y"))?,
    ))
}

fn color(shape: &Value, key: &str, at: &str) -> Result<Option<String>, CallToolResult> {
    match shape.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => schematic_text_color(value)
            .map(Some)
            .map_err(|reason| invalid_arg(&format!("{at}.{key}"), &reason)),
    }
}

fn stroke(shape: &Value, at: &str) -> Result<Stroke, CallToolResult> {
    let width = match shape.get("stroke_width_mm") {
        None | Some(Value::Null) => 0.0,
        Some(value) => millimetres(value, &format!("{at}.stroke_width_mm"))?,
    };
    if width < 0.0 {
        return Err(invalid_arg(
            &format!("{at}.stroke_width_mm"),
            "must not be negative",
        ));
    }
    let style = match shape.get("stroke_type") {
        None | Some(Value::Null) => "default",
        Some(value) => value
            .as_str()
            .filter(|style| LINE_STYLES.contains(style))
            .ok_or_else(|| {
                invalid_arg(
                    &format!("{at}.stroke_type"),
                    &format!("must be one of {}", LINE_STYLES.join(", ")),
                )
            })?,
    };
    // KiCad saves a stroke colour of 0 0 0 0, its default, as no colour.
    let color = color(shape, "stroke_color", at)?.filter(|color| color != "(color 0 0 0 0)");
    Ok(Stroke {
        width,
        style: style.to_owned(),
        color,
    })
}

fn fill(shape: &Value, at: &str) -> Result<Fill, CallToolResult> {
    let fill = match shape.get("fill") {
        None | Some(Value::Null) => "none",
        Some(value) => value
            .as_str()
            .filter(|fill| FILLS.contains(fill))
            .ok_or_else(|| {
                invalid_arg(
                    &format!("{at}.fill"),
                    &format!("must be one of {}", FILLS.join(", ")),
                )
            })?,
    };
    let fill_color = color(shape, "fill_color", at)?;
    match (fill, fill_color) {
        ("color", Some(color)) => Ok(Fill::Color(color)),
        ("color", None) => Err(invalid_arg(
            &format!("{at}.fill_color"),
            "is required with fill 'color'",
        )),
        (_, Some(_)) => Err(invalid_arg(
            &format!("{at}.fill_color"),
            "is taken only with fill 'color'",
        )),
        ("background", None) => Ok(Fill::Background),
        _ => Ok(Fill::None),
    }
}

/// Shape `index` of the call, checked: the dispatch does not hold a `oneOf`
/// inside an array item to its branch, so every key is checked here.
fn parse_shape(shape: &Value, index: usize) -> Result<Shape, CallToolResult> {
    let at = format!("shapes[{index}]");
    let Some(object) = shape.as_object() else {
        return Err(invalid_arg(&at, "must be an object"));
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some((allowed, required)) = shape_keys(kind) else {
        return Err(invalid_arg(
            &format!("{at}.type"),
            "must be one of line, poly, rect, circle",
        ));
    };
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_arg(
            &format!("{at}.{unknown}"),
            &format!("is not a key of a {kind}"),
        ));
    }
    if let Some(missing) = required.iter().find(|key| !object.contains_key(**key)) {
        return Err(invalid_arg(
            &format!("{at}.{missing}"),
            &format!("is required for a {kind}"),
        ));
    }
    let stroke = stroke(shape, &at)?;
    match kind {
        "line" => {
            let (start, end) = (point(shape, "start", &at)?, point(shape, "end", &at)?);
            if start == end {
                return Err(invalid_arg(
                    &format!("{at}.end"),
                    "is the start point once rounded to KiCad's 0.0001 mm; a line needs length",
                ));
            }
            Ok(Shape::Polyline {
                points: vec![start, end],
                stroke,
                fill: Fill::None,
            })
        }
        "poly" => {
            let points = shape["points"]
                .as_array()
                .ok_or_else(|| invalid_arg(&format!("{at}.points"), "must be an array"))?;
            if points.len() < 3 {
                return Err(invalid_arg(
                    &format!("{at}.points"),
                    "needs three or more points; two points are a line",
                ));
            }
            let points = points
                .iter()
                .enumerate()
                .map(|(i, p)| point_at(p, &format!("{at}.points[{i}]")))
                .collect::<Result<Vec<_>, _>>()?;
            if points.windows(2).all(|pair| pair[0] == pair[1]) {
                return Err(invalid_arg(
                    &format!("{at}.points"),
                    "are all one point once rounded to KiCad's 0.0001 mm",
                ));
            }
            Ok(Shape::Polyline {
                points,
                stroke,
                fill: fill(shape, &at)?,
            })
        }
        "rect" => {
            let (start, end) = (point(shape, "start", &at)?, point(shape, "end", &at)?);
            if start.0 == end.0 || start.1 == end.1 {
                return Err(invalid_arg(
                    &format!("{at}.end"),
                    "shares an x or a y with start once rounded to KiCad's 0.0001 mm; a \
                     rectangle needs width and height",
                ));
            }
            Ok(Shape::Rect {
                start,
                end,
                stroke,
                fill: fill(shape, &at)?,
            })
        }
        "circle" => {
            let center = point(shape, "center", &at)?;
            let radius = millimetres(&shape["radius_mm"], &format!("{at}.radius_mm"))?;
            if radius <= 0.0 {
                return Err(invalid_arg(
                    &format!("{at}.radius_mm"),
                    "must be positive once rounded to KiCad's 0.0001 mm",
                ));
            }
            Ok(Shape::Circle {
                center,
                radius,
                stroke,
                fill: fill(shape, &at)?,
            })
        }
        _ => unreachable!("shape_keys knows only these types"),
    }
}

/// A coordinate as KiCad writes it: the shortest decimal that reads back as
/// the rounded value, `0` for `-0`.
pub(super) fn num(value: f64) -> String {
    format!("{}", value + 0.0)
}

fn stroke_block(stroke: &Stroke) -> String {
    let color = stroke
        .color
        .as_ref()
        .map(|color| format!("\t\t\t{color}\n"))
        .unwrap_or_default();
    format!(
        "\t\t(stroke\n\t\t\t(width {})\n\t\t\t(type {})\n{color}\t\t)\n",
        num(stroke.width),
        stroke.style
    )
}

fn fill_block(fill: &Fill) -> String {
    match fill {
        Fill::None => "\t\t(fill\n\t\t\t(type none)\n\t\t)\n".to_owned(),
        Fill::Background => "\t\t(fill\n\t\t\t(type background)\n\t\t)\n".to_owned(),
        Fill::Color(color) => format!("\t\t(fill\n\t\t\t(type color)\n\t\t\t{color}\n\t\t)\n"),
    }
}

/// The shape as KiCad 10 saves it at the top level of a sheet, so a save in
/// KiCad leaves its block's bytes as they are, though KiCad may move the block
/// among the sheet's items. A two-point polyline is KiCad's graphic
/// line, which it writes without a fill.
fn shape_block(shape: &Shape, uuid: &str) -> String {
    let uuid = format!("\t\t(uuid \"{uuid}\")\n\t)");
    match shape {
        Shape::Polyline {
            points,
            stroke,
            fill,
        } => {
            let xy: Vec<String> = points
                .iter()
                .map(|(x, y)| format!("(xy {} {})", num(*x), num(*y)))
                .collect();
            let fill = if points.len() > 2 {
                fill_block(fill)
            } else {
                String::new()
            };
            format!(
                "(polyline\n\t\t(pts\n\t\t\t{}\n\t\t)\n{}{fill}{uuid}",
                xy.join(" "),
                stroke_block(stroke)
            )
        }
        Shape::Rect {
            start,
            end,
            stroke,
            fill,
        } => format!(
            "(rectangle\n\t\t(start {} {})\n\t\t(end {} {})\n{}{}{uuid}",
            num(start.0),
            num(start.1),
            num(end.0),
            num(end.1),
            stroke_block(stroke),
            fill_block(fill)
        ),
        Shape::Circle {
            center,
            radius,
            stroke,
            fill,
        } => format!(
            "(circle\n\t\t(center {} {})\n\t\t(radius {})\n{}{}{uuid}",
            num(center.0),
            num(center.1),
            num(*radius),
            stroke_block(stroke),
            fill_block(fill)
        ),
    }
}

/// The kinds of top-level item the list reports.
fn is_shape(kind: &str) -> bool {
    matches!(kind, "polyline" | "rectangle" | "circle" | "arc" | "bezier")
}

/// A sheet as saved: its top-level items and, of them, its shapes as the list
/// reports them.
struct Sheet {
    content: String,
    revision: String,
    /// Each top-level item's head, uuid and bytes, in file order.
    items: Vec<(String, Option<String>, (usize, usize))>,
    shapes: Vec<Value>,
}

impl Sheet {
    fn parse(content: String) -> Result<Self, String> {
        let tree = parse_sexp(&content).map_err(|error| format!("it cannot be parsed: {error}"))?;
        if tree.head() != Some("kicad_sch") {
            return Err("its root is not (kicad_sch …)".to_owned());
        }
        let mut items = Vec::new();
        let mut shapes = Vec::new();
        for (start, end) in find_direct_child_blocks(&content, "kicad_sch") {
            let node = parse_sexp(&content[start..end])
                .map_err(|error| format!("the item at byte {start} cannot be parsed: {error}"))?;
            let kind = node.head().unwrap_or_default().to_owned();
            let uuid = node.find_str("uuid").map(str::to_owned);
            if is_shape(&kind) {
                shapes.push(
                    shape_json(&node, uuid.as_deref())
                        .map_err(|what| format!("the {kind} at byte {start} has no {what}"))?,
                );
            }
            items.push((kind, uuid, (start, end)));
        }
        Ok(Sheet {
            revision: DocumentRevision::of(&content).to_string(),
            content,
            items,
            shapes,
        })
    }

    fn bytes(&self, index: usize) -> &str {
        let (start, end) = self.items[index].2;
        &self.content[start..end]
    }

    /// Where a new shape goes: before the first placed symbol, where
    /// `add_schematic_text` puts a text, or before KiCad's trailing instance
    /// tables on a sheet without one.
    fn anchor(&self) -> anyhow::Result<ItemAnchor> {
        let first = find_first_symbol_instance(&self.content).and_then(|start| {
            self.items
                .iter()
                .find(|(_, _, (at, _))| *at == start)
                .and_then(|(_, uuid, _)| uuid.clone())
        });
        Ok(match first {
            Some(uuid) => ItemAnchor::Before(ItemId::new(uuid)?),
            None => ItemAnchor::BeforeFooter,
        })
    }
}

fn xy(node: Option<&SexpNode>) -> Option<(f64, f64)> {
    let node = node?;
    Some((node.get_f64(1)?, node.get_f64(2)?))
}

/// `(color R G B A)` as `[r, g, b, a]`, or `None` when absent.
fn color_json(node: Option<&SexpNode>) -> Option<Value> {
    let node = node?;
    let [r, g, b, a] = [1, 2, 3, 4].map(|i| node.get_f64(i));
    // KiCad writes whole channels, reported as integers; any other value is
    // reported as it stands rather than truncated.
    let channel = |value: Option<f64>| {
        value.map(|value| {
            if value.fract() == 0.0 && (0.0..=255.0).contains(&value) {
                json!(value as u64)
            } else {
                json!(value)
            }
        })
    };
    Some(json!([channel(r)?, channel(g)?, channel(b)?, a?]))
}

type Point = (f64, f64);

enum SavedGeometry {
    Polyline(Vec<Point>),
    Bezier([Point; 4]),
    Rectangle(Point, Point),
    Circle(Point, f64),
    Arc(Point, Point, Point),
}

impl SavedGeometry {
    fn read(node: &SexpNode) -> Result<Self, &'static str> {
        let point = |tag| xy(node.find(tag)).ok_or(tag);
        Ok(match node.head() {
            Some(head @ ("polyline" | "bezier")) => {
                let points: Vec<Point> = node
                    .find("pts")
                    .ok_or("pts")?
                    .find_all("xy")
                    .into_iter()
                    .map(|node| xy(Some(node)))
                    .collect::<Option<_>>()
                    .ok_or("readable points")?;
                if head == "bezier" {
                    Self::Bezier(points.try_into().map_err(|_| "four points")?)
                } else if points.len() >= 2 {
                    Self::Polyline(points)
                } else {
                    return Err("two points");
                }
            }
            Some("rectangle") => Self::Rectangle(point("start")?, point("end")?),
            Some("circle") => {
                Self::Circle(point("center")?, node.find_f64("radius").ok_or("radius")?)
            }
            Some("arc") => Self::Arc(point("start")?, point("mid")?, point("end")?),
            _ => return Err("shape type"),
        })
    }

    fn json(self) -> Value {
        let point = |(x, y): Point| json!({ "x": x, "y": y });
        match self {
            Self::Polyline(points) => match points.as_slice() {
                [start, end] => json!({"type": "line", "start": point(*start), "end": point(*end)}),
                _ => {
                    json!({"type": "poly", "points": points.into_iter().map(point).collect::<Vec<_>>()})
                }
            },
            Self::Bezier(points) => json!({"type": "bezier", "points": points.map(point)}),
            Self::Rectangle(start, end) => {
                json!({"type": "rect", "start": point(start), "end": point(end)})
            }
            Self::Circle(center, radius) => {
                json!({"type": "circle", "center": point(center), "radius_mm": radius})
            }
            Self::Arc(start, mid, end) => {
                json!({"type": "arc", "start": point(start), "mid": point(mid), "end": point(end)})
            }
        }
    }
}

pub(super) fn saved_shape_bounds(
    node: &SexpNode,
) -> Option<copperpour_sexp::schematic::SymbolBounds> {
    use copperpour_sexp::schematic::{arc_bounds, SymbolBounds};
    let finite = |(x, y): Point| x.is_finite() && y.is_finite();
    let points = |vertices: &[Point]| {
        let (&first, rest) = vertices.split_first()?;
        vertices.iter().copied().all(finite).then(|| {
            let mut bounds = SymbolBounds::spanning(first, first);
            for &(x, y) in rest {
                bounds.include(x, y);
            }
            bounds
        })
    };
    match SavedGeometry::read(node).ok()? {
        SavedGeometry::Polyline(vertices) => points(&vertices),
        // The curve stays within the control points' envelope.
        SavedGeometry::Bezier(vertices) => points(&vertices),
        SavedGeometry::Rectangle(a, b) => points(&[a, b]),
        SavedGeometry::Circle((x, y), radius) if radius >= 0.0 => {
            points(&[(x - radius, y - radius), (x + radius, y + radius)])
        }
        SavedGeometry::Arc(a, m, b) if [a, m, b].into_iter().all(finite) => arc_bounds(a, m, b),
        _ => None,
    }
}

/// A listed shape in `add_schematic_shapes`'s vocabulary, or the part it lacks.
fn shape_json(node: &SexpNode, uuid: Option<&str>) -> Result<Value, &'static str> {
    let mut shape = SavedGeometry::read(node)?.json();
    let stroke = node.find("stroke");
    shape["uuid"] = json!(uuid);
    shape["stroke_width_mm"] = json!(stroke.and_then(|s| s.find_f64("width")).unwrap_or(0.0));
    shape["stroke_type"] = json!(stroke
        .and_then(|s| s.find("type"))
        .and_then(|t| t.get(1))
        .and_then(SexpNode::as_str)
        .unwrap_or("default"));
    shape["stroke_color"] = json!(color_json(stroke.and_then(|s| s.find("color"))));
    if shape["type"] != "line" {
        let fill = node.find("fill");
        let kind = fill
            .and_then(|f| f.find("type"))
            .and_then(|t| t.get(1))
            .and_then(SexpNode::as_str)
            .unwrap_or("none");
        shape["fill"] = json!(kind);
        shape["fill_color"] = json!(color_json(fill.and_then(|f| f.find("color"))));
    }
    Ok(shape)
}

/// The sheet at `path` as saved, or the refusal saying why it cannot be read.
fn read_sheet(path: &Path) -> anyhow::Result<Result<Sheet, CallToolResult>> {
    read_parsed(path, Sheet::parse)
}

/// Whether `after` is `before` with `added`, in that order and one after
/// another, put in among its top-level items, and nothing else changed: every
/// other item byte for byte in its place.
fn added_only(before: &Sheet, after: &Sheet, added: &[(String, String)]) -> Result<(), String> {
    let is_new = |uuid: &Option<String>| {
        uuid.as_deref()
            .is_some_and(|uuid| added.iter().any(|(id, _)| id == uuid))
    };
    let kept = |sheet: &Sheet| -> Vec<String> {
        (0..sheet.items.len())
            .filter(|&i| !is_new(&sheet.items[i].1))
            .map(|i| sheet.bytes(i).to_owned())
            .collect()
    };
    if kept(before) != kept(after) {
        return Err("a top-level item other than the new shapes changed".to_owned());
    }
    let new: Vec<usize> = (0..after.items.len())
        .filter(|&i| is_new(&after.items[i].1))
        .collect();
    if new.len() != added.len() || new.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return Err(format!(
            "the {} new shapes are not {} items in a row",
            new.len(),
            added.len()
        ));
    }
    for (&index, (uuid, block)) in new.iter().zip(added) {
        if after.items[index].1.as_deref() != Some(uuid) || after.bytes(index) != block {
            return Err(format!(
                "shape {uuid} is not written as prepared, in its place"
            ));
        }
    }
    Ok(())
}

async fn handle_add_schematic_shapes(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let schematic = get_path(args, "schematic")?;
    let Some(requested) = args["shapes"].as_array() else {
        return Ok(invalid_arg("shapes", "missing or not an array"));
    };
    if requested.is_empty() {
        return Ok(invalid_arg("shapes", "must hold at least one shape"));
    }
    let revision = match args.get("revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(revision)) => Some(revision.as_str()),
        Some(_) => return Ok(invalid_arg("revision", "must be a string")),
    };
    // Every shape is checked before the sheet is read: one bad shape writes
    // none.
    let mut shapes = Vec::with_capacity(requested.len());
    for (index, shape) in requested.iter().enumerate() {
        match parse_shape(shape, index) {
            Ok(shape) => shapes.push(shape),
            Err(refusal) => return Ok(refusal),
        }
    }
    let before = match read_sheet(&schematic)? {
        Ok(sheet) => sheet,
        Err(refusal) => return Ok(refusal),
    };
    if revision.is_some_and(|revision| revision != before.revision) {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::StaleTarget {
                target: schematic.display().to_string(),
                reason: "the sheet changed since this revision was listed".to_owned(),
            },
            format!(
                "{ADD} refused: the sheet changed since this revision was listed. No shape was \
                 written; run {LIST} again and pass its revision."
            ),
        ));
    }
    let added: Vec<(String, String)> = shapes
        .iter()
        .map(|shape| {
            let uuid = new_uuid();
            let block = shape_block(shape, &uuid);
            (uuid, block)
        })
        .collect();
    let anchor = before.anchor()?;
    // Inserts at one place land in reverse order of the changes, so they are
    // listed last first to come out in the order asked for.
    let changes = added
        .iter()
        .rev()
        .map(|(uuid, block)| {
            Ok(ItemChange {
                id: ItemId::new(uuid.clone())?,
                before: None,
                after: Some(block.clone()),
                anchor: anchor.clone(),
            })
        })
        .collect::<Result<Vec<_>, copperpour_sexp::SexpError>>()?;
    let command = SchematicCommand::from_changes(&before.content, ADD, changes)?;
    let after = match commit_checked(&schematic, &before.content, ADD, &command, |prepared| {
        let after = Sheet::parse(prepared.to_owned())?;
        added_only(&before, &after, &added).map(|()| after)
    })? {
        Ok(after) => after,
        Err(refusal) => return Ok(refusal),
    };
    // `added_only` proved each new uuid's block is in the file the read-back
    // matched, and `Sheet::parse` lists every polyline, rectangle and circle,
    // so each is found.
    let landed: Vec<&Value> = added
        .iter()
        .filter_map(|(uuid, _)| after.shapes.iter().find(|shape| shape["uuid"] == **uuid))
        .collect();
    Ok(CallToolResult::json(&json!({
        "added_count": landed.len(),
        "shapes": landed,
        "revision": after.revision,
        "source": "file"
    })))
}

async fn handle_list_schematic_shapes(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let schematic = get_path(args, "schematic")?;
    let sheet = match read_sheet(&schematic)? {
        Ok(sheet) => sheet,
        Err(refusal) => return Ok(refusal),
    };
    Ok(CallToolResult::json(&json!({
        "count": sheet.shapes.len(),
        "shapes": sheet.shapes,
        "revision": sheet.revision,
        "source": "file"
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::handler::McpHandler;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::sch_texts::fault;
    use crate::tools::ServerConfig;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// KiCad 10.0.8's sheet of ten shapes: see its README.
    const SHAPES: &str = "schematic_shapes_kicad10.kicad_sch";
    /// KiCad 10.0.6's sheet of six texts, a label and a placed symbol, and no
    /// shape: the one `SHAPES` was built from.
    const TEXTS: &str = "schematic_texts_kicad10.kicad_sch";
    const LINE: &str = "5a1e0c00-2280-4c1e-9a00-000000000001";
    const POLY: &str = "5a1e0c00-2280-4c1e-9a00-000000000002";
    const FRAME: &str = "5a1e0c00-2280-4c1e-9a00-000000000003";
    const SHADED: &str = "5a1e0c00-2280-4c1e-9a00-000000000004";
    const TINTED: &str = "5a1e0c00-2280-4c1e-9a00-000000000005";
    const RING: &str = "5a1e0c00-2280-4c1e-9a00-000000000006";
    const DOT: &str = "5a1e0c00-2280-4c1e-9a00-000000000007";
    const ARC: &str = "5a1e0c00-2280-4c1e-9a00-000000000008";
    const PENNANT: &str = "5a1e0c00-2280-4c1e-9a00-000000000009";
    const CURVE: &str = "5a1e0c00-2280-4c1e-9a00-00000000000a";
    const WIRE: &str = "5a1e0c00-2280-4c1e-9a00-0000000000a0";

    /// The arguments that draw each shape of `SHAPES` but the arc and the
    /// curve, which `add_schematic_shapes` does not write.
    fn requests() -> Vec<(&'static str, Value)> {
        vec![
            (
                LINE,
                json!({ "type": "line", "start": { "x": 12.7, "y": 101.6 }, "end": { "x": 177.8, "y": 101.6 } }),
            ),
            (
                POLY,
                json!({
                    "type": "poly",
                    "points": [{ "x": 12.7, "y": 109.22 }, { "x": 38.1, "y": 116.84 }, { "x": 63.5, "y": 109.22 }],
                    "stroke_width_mm": 0.254, "stroke_type": "dash", "stroke_color": [194, 0, 0]
                }),
            ),
            (
                FRAME,
                json!({ "type": "rect", "start": { "x": 20.32, "y": 17.78 }, "end": { "x": 101.6, "y": 83.82 }, "stroke_type": "dash_dot" }),
            ),
            (
                SHADED,
                json!({
                    "type": "rect", "start": { "x": 114.3, "y": 38.1 }, "end": { "x": 139.7, "y": 63.5 },
                    "stroke_width_mm": 0.1524, "stroke_type": "solid", "fill": "background"
                }),
            ),
            (
                TINTED,
                json!({
                    "type": "rect", "start": { "x": 152.4, "y": 38.1 }, "end": { "x": 177.8, "y": 63.5 },
                    "fill": "color", "fill_color": [255, 230, 153, 0.5]
                }),
            ),
            (
                RING,
                json!({ "type": "circle", "center": { "x": 88.9, "y": 127 }, "radius_mm": 6.35 }),
            ),
            (
                DOT,
                json!({
                    "type": "circle", "center": { "x": 114.3, "y": 127 }, "radius_mm": 3.81,
                    "stroke_width_mm": 0.3, "stroke_type": "dot", "stroke_color": [0, 0, 255, 1],
                    "fill": "color", "fill_color": [0, 0, 255, 0.25]
                }),
            ),
            (
                PENNANT,
                json!({
                    "type": "poly",
                    "points": [{ "x": 127, "y": 76.2 }, { "x": 139.7, "y": 88.9 }, { "x": 114.3, "y": 88.9 }, { "x": 127, "y": 76.2 }],
                    "fill": "color", "fill_color": [0, 194, 0, 0.3]
                }),
            ),
        ]
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .expect("the fixture is committed")
    }

    /// The top-level block KiCad wrote for `uuid` in `source`.
    fn kicad_block(source: &str, uuid: &str) -> String {
        let blocks: Vec<_> = find_direct_child_blocks(source, "kicad_sch")
            .into_iter()
            .map(|(start, end)| &source[start..end])
            .filter(|block| block.contains(&format!("\t\t(uuid \"{uuid}\")\n\t)")))
            .collect();
        assert_eq!(blocks.len(), 1, "{uuid}");
        blocks[0].to_owned()
    }

    fn sheet_with(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SHAPES);
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    async fn call(tool: &str, sheet: &Path, mut args: Value) -> CallToolResult {
        args["schematic"] = json!(sheet.to_str().unwrap());
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 228, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    async fn add(sheet: &Path, shapes: Value) -> CallToolResult {
        call(ADD, sheet, json!({ "shapes": shapes })).await
    }

    fn text(result: &CallToolResult) -> &str {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        text
    }

    fn body(result: &CallToolResult) -> Value {
        assert!(!result.is_error, "{}", text(result));
        serde_json::from_str(text(result)).unwrap()
    }

    fn refused(result: &CallToolResult, kind: &str) -> Value {
        assert_eq!(
            extract_error_kind(result).as_deref(),
            Some(kind),
            "{}",
            text(result)
        );
        serde_json::from_str(text(result)).unwrap()
    }

    async fn listed(sheet: &Path) -> Value {
        body(&call(LIST, sheet, json!({})).await)
    }

    fn uuids(reply: &Value) -> Vec<String> {
        reply["shapes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|shape| shape["uuid"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `block` as KiCad wrote it for `old`, carrying `new` instead.
    fn renamed(block: &str, old: &str, new: &str) -> String {
        assert_eq!(block.matches(old).count(), 1);
        block.replace(old, new)
    }

    /// `source` with `blocks` put in, one per line and in order, before the
    /// line that starts with `before`, as KiCad indents a top-level item.
    fn inserted(source: &str, before: &str, blocks: &[String]) -> String {
        assert_eq!(source.matches(before).count(), 1, "{before}");
        let lines: String = blocks.iter().map(|block| format!("\t{block}\n")).collect();
        source.replacen(before, &format!("{lines}{before}"), 1)
    }

    /// Each shape this tool writes is the bytes KiCad 10.0.8 saved for it, so a
    /// save in KiCad leaves them as they are.
    #[test]
    fn each_shape_is_written_as_kicad_saves_it() {
        let source = fixture(SHAPES);
        for (index, (uuid, request)) in requests().into_iter().enumerate() {
            let shape = parse_shape(&request, index).unwrap();
            assert_eq!(
                shape_block(&shape, uuid),
                kicad_block(&source, uuid),
                "{uuid}"
            );
        }
    }

    #[tokio::test]
    async fn only_the_sheets_own_shapes_are_listed() {
        let (_dir, path) = sheet_with(&fixture(SHAPES));
        let reply = listed(&path).await;
        assert_eq!(reply["source"], "file");
        assert_eq!(
            reply["revision"],
            DocumentRevision::of(&fixture(SHAPES)).to_string()
        );
        // KiCad's own order; the wire, the texts and the polylines, arcs and
        // circle `Device:Ammeter_AC` draws in `lib_symbols` are not shapes.
        assert_eq!(
            uuids(&reply),
            [POLY, FRAME, SHADED, TINTED, RING, DOT, ARC, PENNANT, CURVE, LINE]
        );
        assert!(fixture(SHAPES).contains(&format!("(uuid \"{WIRE}\")")));
        assert!(fixture(SHAPES).contains("\t\t\t\t(arc\n"));
        let shapes: Vec<&Value> = reply["shapes"].as_array().unwrap().iter().collect();
        assert_eq!(
            shapes[8],
            &json!({
                "type": "bezier", "uuid": CURVE,
                "points": [
                    { "x": 152.4, "y": 101.6 }, { "x": 158.75, "y": 95.25 },
                    { "x": 165.1, "y": 107.95 }, { "x": 171.45, "y": 101.6 }
                ],
                "stroke_width_mm": 0.0, "stroke_type": "default", "stroke_color": null,
                "fill": "none", "fill_color": null
            })
        );
        assert_eq!(
            shapes[9],
            &json!({
                "type": "line", "uuid": LINE,
                "start": { "x": 12.7, "y": 101.6 }, "end": { "x": 177.8, "y": 101.6 },
                "stroke_width_mm": 0.0, "stroke_type": "default", "stroke_color": null
            })
        );
        assert_eq!(
            shapes[0],
            &json!({
                "type": "poly", "uuid": POLY,
                "points": [{ "x": 12.7, "y": 109.22 }, { "x": 38.1, "y": 116.84 }, { "x": 63.5, "y": 109.22 }],
                "stroke_width_mm": 0.254, "stroke_type": "dash", "stroke_color": [194, 0, 0, 1.0],
                "fill": "none", "fill_color": null
            })
        );
        assert_eq!(
            shapes[3],
            &json!({
                "type": "rect", "uuid": TINTED,
                "start": { "x": 152.4, "y": 38.1 }, "end": { "x": 177.8, "y": 63.5 },
                "stroke_width_mm": 0.0, "stroke_type": "default", "stroke_color": null,
                "fill": "color", "fill_color": [255, 230, 153, 0.5]
            })
        );
        assert_eq!(
            shapes[5],
            &json!({
                "type": "circle", "uuid": DOT,
                "center": { "x": 114.3, "y": 127.0 }, "radius_mm": 3.81,
                "stroke_width_mm": 0.3, "stroke_type": "dot", "stroke_color": [0, 0, 255, 1.0],
                "fill": "color", "fill_color": [0, 0, 255, 0.25]
            })
        );
        assert_eq!(
            shapes[6],
            &json!({
                "type": "arc", "uuid": ARC,
                "start": { "x": 139.7, "y": 127.0 }, "mid": { "x": 146.05, "y": 120.65 },
                "end": { "x": 152.4, "y": 127.0 },
                "stroke_width_mm": 0.0, "stroke_type": "default", "stroke_color": null,
                "fill": "none", "fill_color": null
            })
        );
    }

    /// A listed line, poly, rect or circle with a fill this tool writes passes back to `add_schematic_shapes`
    /// once its uuid, and a default it reports, are dropped.
    #[tokio::test]
    async fn a_listed_shape_draws_the_same_shape() {
        let (_dir, path) = sheet_with(&fixture(SHAPES));
        let reply = listed(&path).await;
        let shapes = reply["shapes"].as_array().unwrap();
        let again: Vec<Value> = shapes
            .iter()
            .filter(|shape| !matches!(shape["type"].as_str(), Some("arc" | "bezier")))
            .map(|shape| {
                let mut shape = shape.as_object().unwrap().clone();
                shape.remove("uuid");
                shape.retain(|_, value| !value.is_null());
                Value::Object(shape)
            })
            .collect();
        let added = body(&add(&path, json!(again)).await);
        assert_eq!(added["added_count"], 8);
        for (old, new) in shapes
            .iter()
            .filter(|s| !matches!(s["type"].as_str(), Some("arc" | "bezier")))
            .zip(added["shapes"].as_array().unwrap())
        {
            let mut new = new.clone();
            new["uuid"] = old["uuid"].clone();
            assert_eq!(&new, old);
        }
    }

    #[tokio::test]
    async fn shapes_land_before_the_first_symbol_in_the_order_asked() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let kicad = fixture(SHAPES);
        let requests = requests();
        let order = [RING, LINE, TINTED, POLY];
        let shapes: Vec<Value> = order
            .iter()
            .map(|uuid| {
                requests
                    .iter()
                    .find(|(id, _)| id == uuid)
                    .unwrap()
                    .1
                    .clone()
            })
            .collect();
        let reply = body(&add(&path, json!(shapes)).await);
        assert_eq!(reply["added_count"], 4);
        assert_eq!(reply["source"], "file");
        let written = uuids(&reply);
        let blocks: Vec<String> = order
            .iter()
            .zip(&written)
            .map(|(old, new)| renamed(&kicad_block(&kicad, old), old, new))
            .collect();
        assert_eq!(
            read(&path),
            inserted(&fixture(TEXTS), "\t(symbol\n\t\t(lib_id", &blocks)
        );
        let after = listed(&path).await;
        assert_eq!(uuids(&after), written);
        assert_eq!(reply["revision"], after["revision"]);
        assert_eq!(reply["shapes"], after["shapes"]);
    }

    /// A sheet with KiCad's footer and no placed symbol, as a new root sheet
    /// is, takes the shape before the footer.
    #[tokio::test]
    async fn a_sheet_with_a_footer_takes_the_shape_before_it() {
        let source = fixture("erc_severity_kicad10.kicad_sch");
        assert!(!source.contains("(lib_id"));
        let (_dir, path) = sheet_with(&source);
        let ring = requests()[5].1.clone();
        let reply = body(&add(&path, json!([ring])).await);
        let uuid = &uuids(&reply)[0];
        let block = renamed(&kicad_block(&fixture(SHAPES), RING), RING, uuid);
        assert_eq!(
            read(&path),
            inserted(&source, "\t(sheet_instances", &[block])
        );
    }

    /// A sheet with no placed symbol and no footer takes the shape last.
    #[tokio::test]
    async fn a_sheet_without_a_symbol_takes_the_shape_last() {
        let source = fixture("pin_net_names_sub_kicad10.kicad_sch");
        let (_dir, path) = sheet_with(&source);
        let ring = requests()[5].1.clone();
        let reply = body(&add(&path, json!([ring])).await);
        let uuid = &uuids(&reply)[0];
        let block = renamed(&kicad_block(&fixture(SHAPES), RING), RING, uuid);
        let close = source.trim_end().len() - 1;
        assert_eq!(
            read(&path),
            format!("{}\t{block}\n{}", &source[..close], &source[close..])
        );
    }

    #[tokio::test]
    async fn coordinates_are_rounded_as_kicad_holds_them() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let reply = body(
            &add(
                &path,
                json!([{
                    "type": "rect",
                    "start": { "x": 1.123456, "y": -0.00001 },
                    "end": { "x": 25.4, "y": 12.700049 },
                    "stroke_color": [0, 0, 0, 0]
                }]),
            )
            .await,
        );
        let shape = &reply["shapes"][0];
        assert_eq!(shape["start"], json!({ "x": 1.1235, "y": 0.0 }));
        assert_eq!(shape["end"], json!({ "x": 25.4, "y": 12.7 }));
        // KiCad saves its default stroke colour as none.
        assert_eq!(shape["stroke_color"], Value::Null);
        let written = read(&path);
        assert!(written.contains("\t\t(start 1.1235 0)\n\t\t(end 25.4 12.7)\n"));
        assert!(!written.contains("(color 0 0 0 0)"));
    }

    fn bad_shapes() -> Vec<(Value, &'static str)> {
        let p = |x: f64, y: f64| json!({ "x": x, "y": y });
        vec![
            (
                json!({ "type": "arc", "start": p(0.0, 0.0), "end": p(1.0, 1.0) }),
                "shapes[1].type",
            ),
            (
                json!({ "start": p(0.0, 0.0), "end": p(1.0, 1.0) }),
                "shapes[1].type",
            ),
            (json!("rect"), "shapes[1]"),
            (
                json!({ "type": "line", "start": p(0.0, 0.0) }),
                "shapes[1].end",
            ),
            (
                json!({ "type": "line", "start": p(0.0, 0.0), "end": p(0.0, 0.00004) }),
                "shapes[1].end",
            ),
            (
                json!({ "type": "line", "start": p(0.0, 0.0), "end": p(1.0, 0.0), "fill": "none" }),
                "shapes[1].fill",
            ),
            (
                json!({ "type": "line", "start": { "x": 0 }, "end": p(1.0, 0.0) }),
                "shapes[1].start.y",
            ),
            (
                json!({ "type": "line", "start": { "x": 0, "y": 0, "z": 0 }, "end": p(1.0, 0.0) }),
                "shapes[1].start.z",
            ),
            (
                json!({ "type": "line", "start": p(0.0, 0.0), "end": p(1e300, 0.0) }),
                "shapes[1].end.x",
            ),
            (
                json!({ "type": "poly", "points": [p(0.0, 0.0), p(1.0, 0.0)] }),
                "shapes[1].points",
            ),
            (
                json!({ "type": "poly", "points": [p(0.0, 0.0), p(1.0, 0.0), "x"] }),
                "shapes[1].points[2]",
            ),
            (
                json!({ "type": "poly", "points": [p(1.0, 1.0), p(1.0, 1.0), p(1.0, 1.0)] }),
                "shapes[1].points",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(10.0, 0.0) }),
                "shapes[1].end",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "fill": "outline" }),
                "shapes[1].fill",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "fill": "color" }),
                "shapes[1].fill_color",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "fill_color": [1, 2, 3] }),
                "shapes[1].fill_color",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "stroke_type": "dashed" }),
                "shapes[1].stroke_type",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "stroke_width_mm": -1 }),
                "shapes[1].stroke_width_mm",
            ),
            (
                json!({ "type": "rect", "start": p(0.0, 0.0), "end": p(1.0, 1.0), "stroke_color": [256, 0, 0] }),
                "shapes[1].stroke_color",
            ),
            (
                json!({ "type": "circle", "center": p(0.0, 0.0), "radius_mm": 0 }),
                "shapes[1].radius_mm",
            ),
            (
                json!({ "type": "circle", "center": p(0.0, 0.0), "radius_mm": 0.00001 }),
                "shapes[1].radius_mm",
            ),
            (
                json!({ "type": "circle", "center": p(0.0, 0.0), "radius": 1 }),
                "shapes[1].radius",
            ),
        ]
    }

    /// One bad shape after a good one writes neither, through the served
    /// dispatch and through the handler alone, which checks every key itself.
    #[tokio::test]
    async fn one_bad_shape_writes_none() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let good = requests()[0].1.clone();
        let context = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        for (bad, field) in bad_shapes() {
            let shapes = json!([good, bad]);
            refused(&add(&path, shapes.clone()).await, "invalid_argument");
            let args = json!({ "schematic": path.to_str().unwrap(), "shapes": shapes });
            let direct = handle_add_schematic_shapes(&args, &context).await.unwrap();
            let error = refused(&direct, "invalid_argument");
            assert_eq!(error["error"]["field"], field, "{bad}");
        }
        assert_eq!(read(&path), fixture(TEXTS));
    }

    #[tokio::test]
    async fn the_schema_refuses_what_it_does_not_declare() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let good = requests()[0].1.clone();
        for (tool, args) in [
            (LIST, json!({ "revision": "0/0" })),
            (ADD, json!({ "shapes": [] })),
            (ADD, json!({})),
            (ADD, json!({ "shapes": [good], "text": "x" })),
            (ADD, json!({ "shapes": [good], "revision": 7 })),
        ] {
            refused(&call(tool, &path, args).await, "invalid_argument");
        }
        assert_eq!(read(&path), fixture(TEXTS));
    }

    #[tokio::test]
    async fn a_missing_file_or_one_that_is_not_a_sheet_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(SHAPES);
        let good = json!([requests()[0].1]);
        refused(&call(LIST, &missing, json!({})).await, "file_not_found");
        refused(&add(&missing, good.clone()).await, "file_not_found");
        let project = dir.path().join("complex_hierarchy.kicad_sch");
        let source = fixture("title_block_hierarchy/complex_hierarchy.kicad_pro");
        std::fs::write(&project, &source).unwrap();
        refused(&call(LIST, &project, json!({})).await, "invalid_argument");
        refused(&add(&project, good).await, "invalid_argument");
        assert_eq!(read(&project), source);
    }

    #[tokio::test]
    async fn a_stale_revision_writes_nothing() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let first = listed(&path).await["revision"].as_str().unwrap().to_owned();
        let good = requests()[0].1.clone();
        let reply = body(&call(ADD, &path, json!({ "shapes": [good], "revision": first })).await);
        let moved_on = read(&path);
        refused(
            &call(ADD, &path, json!({ "shapes": [good], "revision": first })).await,
            "stale_target",
        );
        assert_eq!(read(&path), moved_on);
        // The revision the write returned holds for the next one.
        body(
            &call(
                ADD,
                &path,
                json!({ "shapes": [good], "revision": reply["revision"] }),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn a_sheet_kicad_holds_open_is_not_written() {
        let (dir, path) = sheet_with(&fixture(TEXTS));
        let lock = r#"{"username":"copperpour-test","hostname":"test-host"}"#;
        std::fs::write(dir.path().join(format!("~{SHAPES}.lck")), lock).unwrap();
        refused(&add(&path, json!([requests()[0].1])).await, "conflict");
        assert_eq!(read(&path), fixture(TEXTS));
    }

    /// Eeschema locks only a project's root schematic, so the root's lock
    /// guards the sheets below it.
    #[tokio::test]
    async fn a_sheet_below_a_root_kicad_holds_open_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "complex_hierarchy.kicad_pro",
            "complex_hierarchy.kicad_sch",
            "ampli_ht.kicad_sch",
        ] {
            let source = fixture(&format!("title_block_hierarchy/{name}"));
            std::fs::write(dir.path().join(name), source).unwrap();
        }
        let child = dir.path().join("ampli_ht.kicad_sch");
        let root_lock = dir.path().join("~complex_hierarchy.kicad_sch.lck");
        std::fs::write(
            &root_lock,
            r#"{"username":"copperpour-test","hostname":"test-host"}"#,
        )
        .unwrap();
        refused(&add(&child, json!([requests()[0].1])).await, "conflict");
        let ampli = fixture("title_block_hierarchy/ampli_ht.kicad_sch");
        assert_eq!(read(&child), ampli);
        std::fs::remove_file(&root_lock).unwrap();
        let reply = body(&add(&child, json!([requests()[0].1])).await);
        assert_eq!(listed(&child).await["shapes"], reply["shapes"]);
    }

    #[tokio::test]
    async fn a_sheet_saved_after_the_read_is_left_as_saved() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let saved = fixture(TEXTS).replacen("(text \"Plain note\"", "(text \"Saved\"", 1);
        crate::tools::write_hook::set(saved.clone());
        let result = add(&path, json!([requests()[0].1])).await;
        assert!(!crate::tools::write_hook::armed());
        refused(&result, "conflict");
        assert_eq!(read(&path), saved);
    }

    /// A prepared sheet that changes another item, or leaves a shape out, is
    /// refused before any byte reaches the file.
    #[tokio::test]
    async fn a_prospective_sheet_that_fails_its_check_is_not_written() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let relabelled = fixture(TEXTS).replacen("(label \"NET_A\"", "(label \"NET_B\"", 1);
        for prospective in [relabelled, fixture(TEXTS)] {
            fault::set_prospective(prospective);
            refused(&add(&path, json!([requests()[0].1])).await, "stale_target");
        }
        assert_eq!(read(&path), fixture(TEXTS));
    }

    #[tokio::test]
    async fn a_file_changed_after_the_commit_is_uncertain() {
        let (_dir, path) = sheet_with(&fixture(TEXTS));
        let saved = fixture(TEXTS).replacen("(text \"Plain note\"", "(text \"Saved after\"", 1);
        fault::set_committed(saved.clone());
        refused(
            &add(&path, json!([requests()[0].1])).await,
            "mutation_outcome_uncertain",
        );
        assert_eq!(read(&path), saved);
    }

    #[test]
    fn the_check_finds_any_change_but_the_new_shapes() {
        let source = fixture(TEXTS);
        let before = Sheet::parse(source.clone()).unwrap();
        let block = |uuid: &str| renamed(&kicad_block(&fixture(SHAPES), RING), RING, uuid);
        let (a, b) = (
            "00000000-0000-4000-8000-00000000000a",
            "00000000-0000-4000-8000-00000000000b",
        );
        let added = [(a.to_owned(), block(a)), (b.to_owned(), block(b))];
        let at = "\t(symbol\n\t\t(lib_id";
        let both = inserted(&source, at, &[block(a), block(b)]);
        let parse = |text: String| Sheet::parse(text).unwrap();
        assert_eq!(added_only(&before, &parse(both.clone()), &added), Ok(()));
        // Out of order, apart, one missing, or another item changed.
        assert!(added_only(
            &before,
            &parse(inserted(&source, at, &[block(b), block(a)])),
            &added
        )
        .is_err());
        let apart = inserted(
            &inserted(&source, at, &[block(b)]),
            "\t(label \"NET_A\"",
            &[block(a)],
        );
        assert!(added_only(&before, &parse(apart), &added).is_err());
        assert!(added_only(&before, &parse(inserted(&source, at, &[block(a)])), &added).is_err());
        let relabelled = both.replacen("(label \"NET_A\"", "(label \"NET_B\"", 1);
        assert!(added_only(&before, &parse(relabelled), &added).is_err());
        let widened = both.replacen("(radius 6.35)", "(radius 7)", 1);
        assert!(added_only(&before, &parse(widened), &added).is_err());
    }
}
