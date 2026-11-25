//! The graphics of a library symbol: the list `get_symbol_info` gives, and
//! `set_symbol_graphics`, which appends items to one unit and body style or
//! replaces or deletes one item, chosen by its index in that list under the
//! library's revision (#867).
//!
//! KiCad keeps a library symbol's drawing in unit blocks,
//! `(symbol "NAME_<unit>_<body style>" …)`, and writes no UUID for the items in
//! them (`SCH_IO_KICAD_SEXPR_LIB_CACHE::saveSymbol`, `saveSymbolDrawItem`,
//! KiCad 10.0.8). So an item is named by its unit, its body style and its
//! index among the graphics of that block, in file order, and only for the
//! revision of the file it was listed in: KiCad's next save may order a unit's
//! items differently (`SCH_ITEM::operator<`: kind, then position), and every
//! save changes the revision, so a stale index is refused rather than applied
//! to another item.
//!
//! An edit writes only the item it adds, replaces or deletes, in KiCad 10's
//! own layout (`KICAD_FORMAT::Prettify`), and every other byte of the file
//! stays as it was.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::library::{is_on_symbol_grid, skip_sexp_token, validate_graphics, SYMBOL_FILLS};
use crate::tools::symbol_fields::{header, symbol_block, unusable, write_checked};
use crate::tools::symbol_pins::{derived_from, extends};
use crate::tools::{invalid_arg, ToolContext, ToolDef};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::writer::{
    apply_edits, find_block_with_leading_whitespace, find_direct_child_blocks,
};
use copperpour_sexp::{
    parse_sexp, read_consistent, DocumentRevision, SexpEdit, SexpError, SexpNode,
};
use serde_json::{json, Map, Value};
use std::path::Path;

const OPERATION: &str = "set_symbol_graphics";

/// The items a unit block draws, as KiCad's parser reads them
/// (`SCH_IO_KICAD_SEXPR_PARSER::ParseSymbolDrawItem`); pins are not graphics.
const GRAPHICS: [&str; 7] = [
    "rectangle",
    "polyline",
    "arc",
    "circle",
    "bezier",
    "text",
    "text_box",
];

/// The graphics KiCad writes first in a unit (`SCH_SHAPE_T`, before text,
/// text boxes and pins in `KICAD_T`), and the only ones `create_symbol`'s
/// vocabulary draws.
const SHAPES: [&str; 5] = ["rectangle", "polyline", "arc", "circle", "bezier"];

/// KiCad holds what `{:g}` writes with six significant digits; a schematic
/// length is a whole number of 0.0001 mm (`schIUScale`).
fn to_schematic_resolution(mm: f64) -> f64 {
    let rounded = (mm * 10_000.0).round() / 10_000.0;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

/// A length as KiCad writes it.
fn mm(value: f64) -> String {
    fmt_f64(to_schematic_resolution(value))
}

/// One direct child block of a unit block: its head and its span.
#[derive(Debug, Clone)]
struct Child {
    head: String,
    block: (usize, usize),
}

/// A unit block `(symbol "NAME_<unit>_<body style>" …)` of a symbol.
#[derive(Debug, Clone)]
struct UnitBlock {
    unit: u32,
    body_style: u32,
    block: (usize, usize),
    /// Where the block's name token ends.
    name_end: usize,
    children: Vec<Child>,
}

impl UnitBlock {
    /// The children that are graphics, in file order: index `i` is the
    /// graphic the list calls `i`.
    fn graphics(&self) -> Vec<&Child> {
        self.children
            .iter()
            .filter(|child| GRAPHICS.contains(&child.head.as_str()))
            .collect()
    }
}

/// The unit blocks of the symbol `name` whose block is `symbol`, in file
/// order, or why one cannot be read. KiCad takes a unit block's name as the
/// symbol's name, `_`, and two numbers (`SCH_IO_KICAD_SEXPR_PARSER::parseLibSymbol`).
fn unit_blocks(source: &str, symbol: (usize, usize), name: &str) -> Result<Vec<UnitBlock>, String> {
    let mut units = Vec::new();
    for (start, end) in find_direct_child_blocks(&source[symbol.0..symbol.1], "symbol") {
        let block = (symbol.0 + start, symbol.0 + end);
        let Some((head, raw_name)) = header(source, block.0) else {
            continue;
        };
        if head != "symbol" {
            continue;
        }
        let unit_name = parse_sexp(raw_name)
            .ok()
            .and_then(|node| node.as_str().map(str::to_owned))
            .ok_or_else(|| "it holds a unit block without a quoted name".to_owned())?;
        let numbers = unit_name
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('_'))
            .and_then(|rest| {
                let (unit, body_style) = rest.split_once('_')?;
                Some((unit.parse::<u32>().ok()?, body_style.parse::<u32>().ok()?))
            });
        let Some((unit, body_style)) = numbers else {
            return Err(format!(
                "its unit block '{unit_name}' is not named '{name}_<unit>_<body style>'"
            ));
        };
        let (_, head_end) = skip_sexp_token(source, block.0 + 1)
            .ok_or_else(|| format!("its unit block '{unit_name}' does not parse"))?;
        let (_, name_end) = skip_sexp_token(source, head_end)
            .ok_or_else(|| format!("its unit block '{unit_name}' does not parse"))?;
        let children = find_direct_child_blocks(&source[block.0..block.1], "symbol")
            .into_iter()
            .filter_map(|(child_start, child_end)| {
                let at = block.0 + child_start;
                let (head_start, head_end) = skip_sexp_token(source, at + 1)?;
                Some(Child {
                    head: source[head_start..head_end].to_owned(),
                    block: (at, block.0 + child_end),
                })
            })
            .collect();
        units.push(UnitBlock {
            unit,
            body_style,
            block,
            name_end,
            children,
        });
    }
    Ok(units)
}

/// A point `(tag x y)` of `node` as `{x, y}`, y up as the file has it.
fn point(node: &SexpNode) -> Option<Value> {
    Some(json!({ "x": node.get_f64(1)?, "y": node.get_f64(2)? }))
}

fn points(node: &SexpNode) -> Option<Vec<Value>> {
    node.find("pts")?
        .find_all("xy")
        .into_iter()
        .map(point)
        .collect()
}

/// A graphic as the list gives it: `item` in `create_symbol`'s vocabulary
/// where the kind has one, and beside it what that vocabulary cannot say
/// (`private`, a stroke type other than `default`, colours, the fill of a line
/// or arc, a fill type such as `color` or `hatch`), each only when the file
/// has it.
fn describe(text: &str) -> Result<(Value, Map<String, Value>), String> {
    let node = parse_sexp(text).map_err(|error| format!("a graphic does not parse: {error}"))?;
    let kind = node.head().unwrap_or_default().to_owned();
    let missing = |what: &str| format!("a {kind} without {what}");
    let mut extras = Map::new();
    if matches!(node.get(1), Some(SexpNode::Atom(atom)) if atom == "private") {
        extras.insert("private".into(), json!(true));
    }
    let stroke = node.find("stroke");
    let width = stroke.and_then(|stroke| stroke.find_f64("width"));
    if let Some(kind) = stroke.and_then(|stroke| stroke.find_str("type")) {
        if kind != "default" {
            extras.insert("stroke_type".into(), json!(kind));
        }
    }
    let rgba = |node: &SexpNode| -> Option<Value> {
        let color = node.find("color")?;
        Some(json!((1..=4)
            .map(|i| color.get_f64(i))
            .collect::<Option<Vec<f64>>>()?))
    };
    if let Some(color) = stroke.and_then(rgba) {
        extras.insert("stroke_color".into(), color);
    }
    let fill_node = node.find("fill");
    let fill = fill_node
        .and_then(|fill| fill.find_str("type"))
        .map(str::to_owned);
    if let Some(color) = fill_node.and_then(rgba) {
        extras.insert("fill_color".into(), color);
    }
    let mut item = Map::new();
    let mut put = |key: &str, value: Value| {
        item.insert(key.to_owned(), value);
    };
    let fill_on_open_shape = |extras: &mut Map<String, Value>| {
        if let Some(fill) = fill.as_deref().filter(|fill| *fill != "none") {
            extras.insert("fill".into(), json!(fill));
        }
    };
    match kind.as_str() {
        "rectangle" => {
            put("type", json!("rect"));
            put(
                "start",
                node.find("start")
                    .and_then(point)
                    .ok_or_else(|| missing("start"))?,
            );
            put(
                "end",
                node.find("end")
                    .and_then(point)
                    .ok_or_else(|| missing("end"))?,
            );
        }
        "polyline" => {
            let pts = points(&node).ok_or_else(|| missing("points"))?;
            if let [start, end] = pts.as_slice() {
                put("type", json!("line"));
                put("start", start.clone());
                put("end", end.clone());
                fill_on_open_shape(&mut extras);
            } else {
                put("type", json!("poly"));
                put("points", json!(pts));
            }
        }
        "arc" => {
            put("type", json!("arc"));
            put(
                "start",
                node.find("start")
                    .and_then(point)
                    .ok_or_else(|| missing("start"))?,
            );
            // A KiCad 6 arc gives its radius and angles instead of a middle point.
            if let Some(mid) = node.find("mid").and_then(point) {
                put("mid", mid);
            }
            put(
                "end",
                node.find("end")
                    .and_then(point)
                    .ok_or_else(|| missing("end"))?,
            );
            fill_on_open_shape(&mut extras);
        }
        "circle" => {
            put("type", json!("circle"));
            put(
                "center",
                node.find("center")
                    .and_then(point)
                    .ok_or_else(|| missing("center"))?,
            );
            put(
                "radius_mm",
                json!(node.find_f64("radius").ok_or_else(|| missing("radius"))?),
            );
        }
        "bezier" => {
            put("type", json!("bezier"));
            put(
                "points",
                json!(points(&node).ok_or_else(|| missing("points"))?),
            );
        }
        "text" | "text_box" => {
            put("type", json!(kind));
            put(
                "text",
                json!(node
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .ok_or_else(|| missing("its text"))?),
            );
            let at = node.find("at").ok_or_else(|| missing("a position"))?;
            put("at", point(at).ok_or_else(|| missing("a position"))?);
            // A symbol text's angle is in tenths of a degree, a text box's in
            // degrees (`parseSymbolText`, `parseSymbolTextBox`).
            let angle = at.get_f64(3).unwrap_or(0.0);
            put(
                "rotation",
                json!(if kind == "text" { angle / 10.0 } else { angle }),
            );
            if kind == "text_box" {
                if let Some(size) = node.find("size") {
                    put(
                        "size",
                        json!({ "x": size.get_f64(1), "y": size.get_f64(2) }),
                    );
                }
            } else if let Some(size) = node
                .find("effects")
                .and_then(|effects| effects.find("font"))
                .and_then(|font| font.find("size"))
            {
                put(
                    "font_size_mm",
                    json!({ "height": size.get_f64(1), "width": size.get_f64(2) }),
                );
            }
        }
        other => return Err(format!("'{other}' is not a graphic")),
    }
    if kind != "text" {
        if let Some(width) = width {
            put("stroke_width_mm", json!(width));
        }
    }
    if matches!(
        kind.as_str(),
        "rectangle" | "circle" | "bezier" | "text_box"
    ) || (kind == "polyline" && item.get("type") == Some(&json!("poly")))
    {
        if let Some(fill) = &fill {
            // KiCad also writes `color`, `hatch`, `reverse_hatch` and
            // `cross_hatch` (`formatFill`, `FILL_T`), which create_symbol's
            // fills cannot say. Such a fill is listed beside the item, whose
            // own fill reads `none`, so the item can always be written back
            // and a replace names the fill it drops.
            if SYMBOL_FILLS.contains(&fill.as_str()) {
                item.insert("fill".into(), json!(fill));
            } else {
                item.insert("fill".into(), json!("none"));
                extras.insert("fill".into(), json!(fill));
            }
        }
    }
    Ok((Value::Object(item), extras))
}

/// One graphic of a symbol, as listed.
#[derive(Debug, Clone, PartialEq)]
struct Graphic {
    unit: u32,
    body_style: u32,
    index: usize,
    item: Value,
    extras: Map<String, Value>,
}

impl Graphic {
    fn json(&self) -> Value {
        let mut entry = json!({
            "unit": self.unit,
            "body_style": self.body_style,
            "index": self.index,
            "item": self.item,
        });
        for (key, value) in &self.extras {
            entry[key] = value.clone();
        }
        entry
    }
}

/// Every graphic of the symbol `name` whose block is `symbol`, unit block by
/// unit block in file order.
fn graphics_of(source: &str, symbol: (usize, usize), name: &str) -> Result<Vec<Graphic>, String> {
    let mut listed = Vec::new();
    for unit in unit_blocks(source, symbol, name)? {
        for (index, child) in unit.graphics().into_iter().enumerate() {
            let (item, extras) = describe(&source[child.block.0..child.block.1])?;
            listed.push(Graphic {
                unit: unit.unit,
                body_style: unit.body_style,
                index,
                item,
                extras,
            });
        }
    }
    Ok(listed)
}

/// The symbol whose graphics `name` shows: itself, or the root its
/// `(extends …)` chain reaches, as KiCad draws a derived symbol with its
/// root's items (`LIB_SYMBOL::GetRootSymbol`). A chain that loops or names a
/// symbol the library lacks has no graphics to show.
fn graphics_owner(source: &str, name: &str) -> Result<(String, (usize, usize)), String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut current = name.to_owned();
    loop {
        let block = symbol_block(source, &current)
            .map_err(|_| format!("the library does not define '{current}' exactly once"))?;
        match extends(source, block.0, block.1) {
            None => return Ok((current, block)),
            Some(parent) => {
                if !seen.insert(current.clone()) {
                    return Err(format!("the extends chain of '{name}' loops"));
                }
                current = parent;
            }
        }
    }
}

/// What `get_symbol_info` adds for a library: the file's `revision`, and the
/// `graphics` the symbol `name` shows, each with its unit, body style, index
/// and item. `Err` says why the graphics cannot be listed.
pub(crate) fn listed_graphics(source: &str, name: &str) -> (String, Result<Vec<Value>, String>) {
    let revision = DocumentRevision::of(source).to_string();
    let graphics = graphics_owner(source, name).and_then(|(owner, block)| {
        graphics_of(source, block, &owner).map(|list| list.iter().map(Graphic::json).collect())
    });
    (revision, graphics)
}

pub(crate) fn tool() -> ToolDef {
    let mut graphics = crate::tools::library::symbol_graphics_schema();
    graphics["description"] = json!(
        "The items to write, in create_symbol's vocabulary: line, arc, rect, circle or poly, \
         points as {x, y} in the symbol's own millimetres with y up, stroke_width_mm, and fill \
         ('none', 'outline' or 'background') on rect, circle and poly. append takes one or more; \
         replace exactly one; delete none."
    );
    graphics["minItems"] = json!(1);
    tool!(
        "set_symbol_graphics",
        "Edit the drawing of an existing library symbol: append items to one unit and body \
         style, or replace or delete one item, chosen by its index. Run get_symbol_info first: \
         it lists each graphic with its unit, body_style and index, and the library's revision, \
         which this tool takes and refuses when the file changed since (a stale index never \
         reaches another item). Items are written in KiCad 10's layout and nothing else in the \
         file changes; KiCad's next save may order a unit's items differently, so list again \
         after it. A point off the 1.27 mm grid is written with a warning. A derived symbol \
         (extends) is refused: edit its root. Answers the items written or removed, the unit's \
         graphics after the edit, the new revision and the derived symbols that show the change.",
        json!({
            "type": "object",
            "properties": {
                "library_path": { "type": "string", "description": "Path to the .kicad_sym library" },
                "symbol_name": { "type": "string", "description": "The symbol, not a derived one" },
                "revision": { "type": "string", "description": "The revision get_symbol_info returned for this library" },
                "unit": { "type": "integer", "minimum": 0, "description": "The unit, as get_symbol_info lists it; 0 draws in every unit" },
                "body_style": { "type": "integer", "minimum": 0, "description": "The body style, as get_symbol_info lists it: 1, or 2 for a De Morgan body" },
                "mode": { "type": "string", "enum": ["append", "replace", "delete"] },
                "index": { "type": "integer", "minimum": 0, "description": "replace and delete: the graphic's index within its unit and body style, as get_symbol_info lists it" },
                "graphics": graphics
            },
            "required": ["library_path", "symbol_name", "revision", "unit", "body_style", "mode"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_set_symbol_graphics(args, ctx).await }
    )
}

#[derive(Debug)]
enum Edit {
    Append(Vec<Value>),
    Replace(usize, Value),
    Delete(usize),
}

impl Edit {
    fn mode(&self) -> &'static str {
        match self {
            Edit::Append(_) => "append",
            Edit::Replace(..) => "replace",
            Edit::Delete(_) => "delete",
        }
    }
}

#[derive(Debug)]
struct Request<'a> {
    symbol: &'a str,
    revision: &'a str,
    unit: u32,
    body_style: u32,
    edit: Edit,
}

fn whole(args: &Value, key: &str) -> Result<Option<u64>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid_arg(key, "must be a whole number, 0 or more")),
    }
}

fn parse_request(args: &Value) -> Result<Request<'_>, CallToolResult> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_arg(key, "missing or not a string"))
    };
    let symbol = text("symbol_name")?;
    let revision = text("revision")?;
    let part = |key: &str| -> Result<u32, CallToolResult> {
        let value = whole(args, key)?.ok_or_else(|| invalid_arg(key, "is required"))?;
        u32::try_from(value).map_err(|_| invalid_arg(key, "is too large"))
    };
    let unit = part("unit")?;
    let body_style = part("body_style")?;
    let index = whole(args, "index")?.map(|index| usize::try_from(index).unwrap_or(usize::MAX));
    let graphics = match args.get("graphics") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(items.clone()),
        Some(_) => {
            return Err(invalid_arg(
                "graphics",
                "must be an array of drawing primitives",
            ))
        }
    };
    if let Some(items) = &graphics {
        validate_graphics(items).map_err(|reason| invalid_arg("graphics", &reason))?;
    }
    let edit = match (text("mode")?, index, graphics) {
        ("append", Some(_), _) => {
            return Err(invalid_arg(
                "index",
                "append adds items after the unit's last shape; it takes no index",
            ))
        }
        ("append", None, Some(items)) if !items.is_empty() => Edit::Append(items),
        ("append", None, _) => {
            return Err(invalid_arg("graphics", "append needs one or more items"))
        }
        ("replace" | "delete", None, _) => {
            return Err(invalid_arg(
                "index",
                "replace and delete name the graphic by its index",
            ))
        }
        ("replace", Some(index), Some(items)) if items.len() == 1 => {
            Edit::Replace(index, items.into_iter().next().unwrap_or_default())
        }
        ("replace", Some(_), _) => {
            return Err(invalid_arg("graphics", "replace takes exactly one item"))
        }
        ("delete", Some(_), Some(_)) => {
            return Err(invalid_arg("graphics", "delete takes no items"))
        }
        ("delete", Some(index), None) => Edit::Delete(index),
        (other, ..) => {
            return Err(invalid_arg(
                "mode",
                &format!("'{other}' is not append, replace or delete"),
            ))
        }
    };
    Ok(Request {
        symbol,
        revision,
        unit,
        body_style,
        edit,
    })
}

async fn handle_set_symbol_graphics(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = super::get_path(args, "library_path")?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sym") {
        return Ok(unusable(&path, "it does not end in .kicad_sym"));
    }
    let request = match parse_request(args) {
        Ok(request) => request,
        Err(refusal) => return Ok(refusal),
    };
    let source = match read_consistent(&path) {
        Ok(source) => source,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&path),
                format!("Symbol library not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if DocumentRevision::of(&source).to_string() != request.revision {
        return Ok(stale(
            &request,
            "the library changed since get_symbol_info returned this revision",
        ));
    }
    let plan = match prepare(&path, &source, &request) {
        Ok(plan) => plan,
        Err(refusal) => return Ok(refusal),
    };
    let check = |text: &str| readback_mismatch(text, &source, request.symbol, &plan);
    if let Some(refusal) =
        write_checked(&path, &source, plan.replacement.clone(), OPERATION, check)?
    {
        return Ok(refusal);
    }
    let in_unit: Vec<Value> = plan
        .expected
        .iter()
        .filter(|graphic| (graphic.unit, graphic.body_style) == (request.unit, request.body_style))
        .map(Graphic::json)
        .collect();
    let mut body = json!({
        "success": true,
        "library_path": path.display().to_string(),
        "symbol": request.symbol,
        "unit": request.unit,
        "body_style": request.body_style,
        "mode": request.edit.mode(),
        "graphics": in_unit,
        "revision": DocumentRevision::of(&plan.replacement).to_string(),
        "derived_symbols": plan.derived,
    });
    for (key, value) in plan.report {
        body[key] = value;
    }
    if !plan.warnings.is_empty() {
        body["warnings"] = json!(plan.warnings);
    }
    Ok(CallToolResult::json(&body))
}

fn stale(request: &Request<'_>, reason: &str) -> CallToolResult {
    let target = format!(
        "symbol '{}' unit {} body style {}",
        request.symbol, request.unit, request.body_style
    );
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: target.clone(),
            reason: reason.to_owned(),
        },
        format!(
            "{OPERATION} refused for {target}: {reason}. Nothing was written; run get_symbol_info \
             again and pass its revision and index."
        ),
    )
}

/// The edit as planned: the library it writes, the span of `source` it
/// replaces and the length of what replaces it, every graphic of the symbol
/// as the written library must list them, and the answer's parts.
#[derive(Debug)]
struct Plan {
    replacement: String,
    span: (usize, usize),
    written_len: usize,
    expected: Vec<Graphic>,
    report: Vec<(&'static str, Value)>,
    warnings: Vec<String>,
    derived: Vec<String>,
}

/// How the file indents: the leading whitespace of the line `at` is on, and
/// one step of it, from the top-level symbol's line (KiCad 10 writes tabs,
/// KiCad 6 to 8 two spaces).
fn line_indent(source: &str, at: usize) -> &str {
    let line_start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let indent_end = source[line_start..at]
        .find(|c: char| c != ' ' && c != '\t')
        .map_or(at, |offset| line_start + offset);
    &source[line_start..indent_end]
}

/// The lines of a block, each as (depth, text), indented from `base` by
/// `step` per depth and joined with `newline`.
fn join(lines: &[(usize, String)], base: &str, step: &str, newline: &str) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, (depth, text))| {
            let indent = if i == 0 {
                String::new()
            } else {
                format!("{base}{}", step.repeat(*depth))
            };
            format!("{indent}{text}")
        })
        .collect::<Vec<_>>()
        .join(newline)
}

/// `(xy …)` points as KiCad's `Prettify` lays them out: on one line while
/// that line is shorter than 99 columns, an indent character counting one.
fn xy_lines(points: &[(f64, f64)], depth: usize, indent_width: usize) -> Vec<(usize, String)> {
    let mut lines: Vec<(usize, String)> = Vec::new();
    let mut column = 0;
    for &(x, y) in points {
        let token = format!("(xy {} {})", mm(x), mm(y));
        match lines.last_mut() {
            Some((_, line)) if column < 99 => {
                column += 1 + token.len();
                line.push(' ');
                line.push_str(&token);
            }
            _ => {
                column = indent_width + token.len();
                lines.push((depth, token));
            }
        }
    }
    lines
}

fn xy(value: &Value) -> (f64, f64) {
    (
        value["x"].as_f64().unwrap_or(0.0),
        value["y"].as_f64().unwrap_or(0.0),
    )
}

/// One item of `create_symbol`'s vocabulary in KiCad 10's layout, opening at
/// `base` (the item's own indent, already written) with `step` per depth.
fn item_text(item: &Value, base: &str, step: &str, newline: &str) -> String {
    let at = |key: &str| {
        let (x, y) = xy(&item[key]);
        format!("({key} {} {})", mm(x), mm(y))
    };
    let fill = item["fill"].as_str().unwrap_or("none");
    let width = item["stroke_width_mm"].as_f64().unwrap_or(0.0);
    let indent_width = |depth: usize| base.len() + step.len() * depth;
    let mut lines: Vec<(usize, String)> = Vec::new();
    match item["type"].as_str().unwrap_or_default() {
        kind @ ("line" | "poly") => {
            let pts: Vec<(f64, f64)> = if kind == "line" {
                vec![xy(&item["start"]), xy(&item["end"])]
            } else {
                item["points"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(xy)
                    .collect()
            };
            lines.push((0, "(polyline".into()));
            lines.push((1, "(pts".into()));
            lines.extend(xy_lines(&pts, 2, indent_width(2)));
            lines.push((1, ")".into()));
        }
        "rect" => {
            lines.push((0, "(rectangle".into()));
            lines.push((1, at("start")));
            lines.push((1, at("end")));
        }
        "arc" => {
            lines.push((0, "(arc".into()));
            lines.push((1, at("start")));
            lines.push((1, at("mid")));
            lines.push((1, at("end")));
        }
        "circle" => {
            lines.push((0, "(circle".into()));
            lines.push((1, at("center")));
            lines.push((
                1,
                format!("(radius {})", mm(item["radius_mm"].as_f64().unwrap_or(0.0))),
            ));
        }
        _ => {}
    }
    lines.extend([
        (1, "(stroke".to_owned()),
        (2, format!("(width {})", mm(width))),
        (2, "(type default)".to_owned()),
        (1, ")".to_owned()),
        (1, "(fill".to_owned()),
        (2, format!("(type {fill})")),
        (1, ")".to_owned()),
        (0, ")".to_owned()),
    ]);
    join(&lines, base, step, newline)
}

/// The item as the written library must list it: its numbers at KiCad's
/// resolution, and no fill on a line or arc, which `create_symbol`'s
/// vocabulary does not give them.
fn as_listed(item: &Value) -> Value {
    fn round(value: &Value) -> Value {
        match value {
            Value::Number(number) => number
                .as_f64()
                .map_or_else(|| value.clone(), |n| json!(to_schematic_resolution(n))),
            Value::Array(items) => Value::Array(items.iter().map(round).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), round(v))).collect())
            }
            other => other.clone(),
        }
    }
    round(item)
}

/// The points of an item a pin could be drawn to, each off the 1.27 mm grid
/// named in a warning. An arc's middle point and a radius are not such points.
fn off_grid_warnings(items: &[Value]) -> Vec<String> {
    let mut warnings = Vec::new();
    for (at, item) in items.iter().enumerate() {
        let kind = item["type"].as_str().unwrap_or_default();
        let named: Vec<(String, &Value)> = match kind {
            "rect" | "line" | "arc" => vec![
                ("start".into(), &item["start"]),
                ("end".into(), &item["end"]),
            ],
            "circle" => vec![("center".into(), &item["center"])],
            "poly" => item["points"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, p)| (format!("points[{i}]"), p))
                .collect(),
            _ => Vec::new(),
        };
        for (key, value) in named {
            let (x, y) = xy(value);
            let (x, y) = (to_schematic_resolution(x), to_schematic_resolution(y));
            if !(is_on_symbol_grid(x) && is_on_symbol_grid(y)) {
                warnings.push(format!(
                    "graphics[{at}] ({kind}) {key} ({}, {}) is off the 1.27 mm symbol grid",
                    fmt_f64(x),
                    fmt_f64(y)
                ));
            }
        }
    }
    warnings
}

fn prepare(path: &Path, source: &str, request: &Request<'_>) -> Result<Plan, CallToolResult> {
    let root = source.len() - source.trim_start().len();
    if header(source, root).map(|(head, _)| head) != Some("kicad_symbol_lib") {
        return Err(unusable(path, "it is not a (kicad_symbol_lib ...) file"));
    }
    let symbol = symbol_block(source, request.symbol)?;
    if let Some(parent) = extends(source, symbol.0, symbol.1) {
        return Err(invalid_arg(
            "symbol_name",
            &format!(
                "symbol '{}' extends '{parent}', whose graphics it shows; edit '{parent}'",
                request.symbol
            ),
        ));
    }
    let units =
        unit_blocks(source, symbol, request.symbol).map_err(|reason| unusable(path, &reason))?;
    let before =
        graphics_of(source, symbol, request.symbol).map_err(|reason| unusable(path, &reason))?;
    let wanted = (request.unit, request.body_style);
    let matching: Vec<&UnitBlock> = units
        .iter()
        .filter(|unit| (unit.unit, unit.body_style) == wanted)
        .collect();
    // A second layer: KiCad writes one block per unit and body style
    // (`SCH_IO_KICAD_SEXPR_LIB_CACHE::SaveSymbol` over
    // `LIB_SYMBOL::GetUnitDrawItems`), so no file KiCad saved reaches this.
    if matching.len() > 1 {
        return Err(unusable(
            path,
            &format!(
                "symbol '{}' has {} blocks for unit {} body style {}",
                request.symbol,
                matching.len(),
                request.unit,
                request.body_style
            ),
        ));
    }
    let target = matching.first().copied();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let symbol_indent = line_indent(source, symbol.0);
    let step = if symbol_indent.is_empty() {
        "\t"
    } else {
        symbol_indent
    };
    let blocks_named = || {
        let list: Vec<String> = units
            .iter()
            .map(|u| format!("unit {} body style {}", u.unit, u.body_style))
            .collect();
        if list.is_empty() {
            "it has none".to_owned()
        } else {
            format!("it has {}", list.join(", "))
        }
    };
    let target = match (target, &request.edit) {
        (None, Edit::Replace(index, _) | Edit::Delete(index)) => {
            return Err(invalid_arg(
                "unit",
                &format!(
                    "symbol '{}' has no block for unit {} body style {} (index {index}); {}",
                    request.symbol,
                    request.unit,
                    request.body_style,
                    blocks_named()
                ),
            ));
        }
        (target, _) => target,
    };
    let derived = derived_from(source, symbol.0);
    let mut expected = before.clone();
    let mut report: Vec<(&'static str, Value)> = Vec::new();
    let mut warnings = Vec::new();
    let reindex = |graphics: &mut Vec<Graphic>| {
        let mut counts = std::collections::BTreeMap::new();
        for graphic in graphics.iter_mut() {
            let next = counts
                .entry((graphic.unit, graphic.body_style))
                .or_insert(0usize);
            graphic.index = *next;
            *next += 1;
        }
    };
    let (span, written) = match &request.edit {
        Edit::Append(items) => {
            warnings.extend(off_grid_warnings(items));
            let new: Vec<Graphic> = items
                .iter()
                .map(|item| Graphic {
                    unit: request.unit,
                    body_style: request.body_style,
                    index: 0,
                    item: as_listed(item),
                    extras: Map::new(),
                })
                .collect();
            let (at, text, position_in_unit) = match target {
                Some(unit) => {
                    let base = format!("{}{step}", line_indent(source, unit.block.0));
                    let last_shape = unit
                        .children
                        .iter()
                        .rposition(|child| SHAPES.contains(&child.head.as_str()));
                    let anchor = match last_shape {
                        Some(i) => unit.children[i].block.1,
                        None => unit
                            .children
                            .iter()
                            .rfind(|child| child.head == "unit_name")
                            .map_or(unit.name_end, |child| child.block.1),
                    };
                    let position = unit
                        .graphics()
                        .iter()
                        .filter(|child| child.block.1 <= anchor)
                        .count();
                    let text: String = items
                        .iter()
                        .map(|item| {
                            format!("{newline}{base}{}", item_text(item, &base, step, newline))
                        })
                        .collect();
                    (anchor, text, position)
                }
                None => {
                    let unit_count = units.iter().map(|u| u.unit).max().unwrap_or(1).max(1);
                    if request.unit > unit_count {
                        return Err(invalid_arg(
                            "unit",
                            &format!(
                                "symbol '{}' has {unit_count} unit(s); unit {} would add one, which \
                                 this tool does not do",
                                request.symbol, request.unit
                            ),
                        ));
                    }
                    if request.body_style != 1
                        && !units.iter().any(|u| u.body_style == request.body_style)
                    {
                        return Err(invalid_arg(
                            "body_style",
                            &format!(
                                "symbol '{}' has no body style {}; {}",
                                request.symbol,
                                request.body_style,
                                blocks_named()
                            ),
                        ));
                    }
                    // KiCad writes unit blocks after the fields, ordered by unit,
                    // then body style (`saveSymbol`).
                    let after = units
                        .iter()
                        .filter(|u| (u.unit, u.body_style) < wanted)
                        .map(|u| u.block.1)
                        .max();
                    let anchor = match after {
                        Some(end) => end,
                        None => match units.iter().map(|u| u.block.0).min() {
                            Some(first) => {
                                // Before the first unit block: after the child before it.
                                symbol_children_end_before(source, symbol, first)
                            }
                            None => symbol_children_end_before(
                                source,
                                symbol,
                                embedded_fonts(source, symbol).unwrap_or(symbol.1 - 1),
                            ),
                        },
                    };
                    let unit_indent = format!("{symbol_indent}{step}");
                    let base = format!("{unit_indent}{step}");
                    let (_, raw_name) = header(source, symbol.0).unwrap_or(("", "\"\""));
                    let quoted = format!(
                        "{}_{}_{}\"",
                        &raw_name[..raw_name.len().saturating_sub(1)],
                        request.unit,
                        request.body_style
                    );
                    let items_text: String = items
                        .iter()
                        .map(|item| {
                            format!("{newline}{base}{}", item_text(item, &base, step, newline))
                        })
                        .collect();
                    let text = format!(
                        "{newline}{unit_indent}(symbol {quoted}{items_text}{newline}{unit_indent})"
                    );
                    (anchor, text, 0)
                }
            };
            // The list is in file order: the new items follow the graphics of
            // every unit block that ends before `at`, and those of their own
            // block before it.
            let insert_at = units
                .iter()
                .filter(|unit| unit.block.1 <= at)
                .map(|unit| unit.graphics().len())
                .sum::<usize>()
                + position_in_unit;
            for (offset, graphic) in new.into_iter().enumerate() {
                expected.insert(insert_at + offset, graphic);
            }
            reindex(&mut expected);
            let added: Vec<Value> = expected[insert_at..insert_at + items.len()]
                .iter()
                .map(Graphic::json)
                .collect();
            report.push(("added", json!(added)));
            ((at, at), text)
        }
        Edit::Replace(index, item) => {
            let Some(unit) = target else {
                return Err(out_of_range(request, *index, 0));
            };
            let graphics = unit.graphics();
            let child = graphics
                .get(*index)
                .ok_or_else(|| out_of_range(request, *index, graphics.len()))?;
            warnings.extend(off_grid_warnings(std::slice::from_ref(item)));
            let position = expected
                .iter()
                .position(|g| (g.unit, g.body_style) == wanted && g.index == *index)
                .unwrap_or_default();
            let previous = expected[position].clone();
            let dropped: Vec<String> = previous
                .extras
                .iter()
                .map(|(key, value)| match value {
                    Value::String(text) => format!("{key} {text}"),
                    _ => key.clone(),
                })
                .collect();
            if !dropped.is_empty() {
                warnings.push(format!(
                    "the replaced graphic had {}, which create_symbol's vocabulary cannot carry; \
                     the new item is written without them",
                    dropped.join(", ")
                ));
            }
            expected[position] = Graphic {
                item: as_listed(item),
                extras: Map::new(),
                ..previous.clone()
            };
            report.push(("previous", previous.json()));
            report.push(("current", expected[position].json()));
            let base = line_indent(source, child.block.0);
            (child.block, item_text(item, base, step, newline))
        }
        Edit::Delete(index) => {
            let Some(unit) = target else {
                return Err(out_of_range(request, *index, 0));
            };
            let graphics = unit.graphics();
            let child = graphics
                .get(*index)
                .ok_or_else(|| out_of_range(request, *index, graphics.len()))?;
            let position = expected
                .iter()
                .position(|g| (g.unit, g.body_style) == wanted && g.index == *index)
                .unwrap_or_default();
            let removed = expected.remove(position);
            reindex(&mut expected);
            report.push(("removed", removed.json()));
            let span = find_block_with_leading_whitespace(source, child.block.0)
                .ok_or_else(|| unusable(path, "it holds a graphic whose block does not close"))?;
            (span, String::new())
        }
    };
    let replacement = apply_edits(
        source.to_owned(),
        vec![if span.0 == span.1 {
            SexpEdit::insert(span.0, written.clone())
        } else {
            SexpEdit::replace(span.0, span.1, written.clone())
        }],
    );
    Ok(Plan {
        replacement,
        span,
        written_len: written.len(),
        expected,
        report,
        warnings,
        derived,
    })
}

fn out_of_range(request: &Request<'_>, index: usize, count: usize) -> CallToolResult {
    invalid_arg(
        "index",
        &format!(
            "unit {} body style {} of symbol '{}' has {count} graphic(s){}; index {index} is not one",
            request.unit,
            request.body_style,
            request.symbol,
            if count == 0 { String::new() } else { format!(", indices 0 to {}", count - 1) }
        ),
    )
}

/// The `(embedded_fonts …)` child of the symbol, before which KiCad writes
/// the unit blocks.
fn embedded_fonts(source: &str, symbol: (usize, usize)) -> Option<usize> {
    find_direct_child_blocks(&source[symbol.0..symbol.1], "symbol")
        .into_iter()
        .map(|(start, _)| symbol.0 + start)
        .find(|&start| {
            header(source, start).is_some_and(|(head, _)| head == "embedded_fonts")
                || skip_sexp_token(source, start + 1)
                    .is_some_and(|(from, to)| &source[from..to] == "embedded_fonts")
        })
}

/// Where the last direct child of the symbol that opens before `before` ends,
/// or the end of the symbol's name token when none does: a new unit block is
/// written there, after that child.
fn symbol_children_end_before(source: &str, symbol: (usize, usize), before: usize) -> usize {
    find_direct_child_blocks(&source[symbol.0..symbol.1], "symbol")
        .into_iter()
        .map(|(start, end)| (symbol.0 + start, symbol.0 + end))
        .filter(|&(start, _)| start < before)
        .map(|(_, end)| end)
        .max()
        .unwrap_or_else(|| {
            skip_sexp_token(source, symbol.0 + 1)
                .and_then(|(_, head_end)| skip_sexp_token(source, head_end))
                .map_or(symbol.0 + 1, |(_, name_end)| name_end)
        })
}

/// Why `text` is not the planned edit of `source`, or `None` when it is:
/// every byte outside the edited span is as it was, and the symbol's graphics
/// read as planned, the new items included.
fn readback_mismatch(text: &str, source: &str, symbol: &str, plan: &Plan) -> Option<String> {
    let (from, to) = plan.span;
    let tail = source.len() - to;
    if text.len() != source.len() - (to - from) + plan.written_len {
        return Some(format!(
            "the library is {} bytes long, not the {} the edit leaves",
            text.len(),
            source.len() - (to - from) + plan.written_len
        ));
    }
    if text.as_bytes()[..from] != source.as_bytes()[..from]
        || text.as_bytes()[text.len() - tail..] != source.as_bytes()[to..]
    {
        return Some("a part of the library outside the edited graphic changed".to_owned());
    }
    let block = match symbol_block(text, symbol) {
        Ok(block) => block,
        Err(_) => return Some(format!("symbol '{symbol}' cannot be found once")),
    };
    let found = match graphics_of(text, block, symbol) {
        Ok(found) => found,
        Err(reason) => return Some(format!("its graphics cannot be read: {reason}")),
    };
    if found.len() != plan.expected.len() {
        return Some(format!(
            "symbol '{symbol}' lists {} graphics, not the {} planned",
            found.len(),
            plan.expected.len()
        ));
    }
    found
        .iter()
        .zip(&plan.expected)
        .find(|(found, expected)| found != expected)
        .map(|(found, expected)| {
            format!(
                "graphic {} of unit {} body style {} reads {}, not {}",
                expected.index,
                expected.unit,
                expected.body_style,
                found.json(),
                expected.json()
            )
        })
}

#[cfg(test)]
#[path = "symbol_graphics_tests.rs"]
mod tests;
