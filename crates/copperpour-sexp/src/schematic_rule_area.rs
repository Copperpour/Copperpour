//! Saved schematic rule areas and their box collision test.

use crate::schematic::{
    parse_at, parse_lib_pin, parse_subsymbol_unit, rule_area_outline, symbol_instance,
    symbol_local_bounds_for_unit, SymbolBounds,
};
use crate::text_bounds::{repeated_style_setting, rule_area_field_bounds};
use crate::transform_pin;
use crate::SexpNode;
use std::collections::BTreeSet;

type Point = (f64, f64);
const PRECISION: f64 = 0.0001; // one schematic internal unit

pub struct RuleArea<'a> {
    pub node: &'a SexpNode,
    pub uuid: Option<&'a str>,
    outline: Option<Vec<Point>>,
}

impl<'a> RuleArea<'a> {
    pub fn from_node(node: &'a SexpNode) -> Self {
        let polyline = node.find_all("polyline").last().copied();
        Self {
            node,
            uuid: polyline.and_then(|poly| poly.find_str("uuid")),
            outline: rule_area_outline(node),
        }
    }

    /// `Some(true)` proves intersection, `Some(false)` proves separation.
    /// The strip within one saved unit of the edge is left undecided.
    pub fn collides(&self, bounds: SymbolBounds) -> Option<bool> {
        let outline = self.outline.as_ref()?;
        if intersects(grow(bounds, -PRECISION), outline) {
            Some(true)
        } else if !intersects(grow(bounds, 2.0 * PRECISION), outline) {
            Some(false)
        } else {
            None
        }
    }

    /// Directives attach by the label's connection point, not its text box.
    pub fn attaches(&self, directive: &SexpNode) -> Option<bool> {
        let outline = self.outline.as_ref()?;
        // A repeated position cannot prove where the directive attaches.
        if directive.find_all("at").len() != 1 {
            return None;
        }
        let (x, y, _) = parse_at(directive)?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let distance = outline
            .iter()
            .zip(outline.iter().cycle().skip(1))
            .map(|(&(ax, ay), &(bx, by))| {
                let (dx, dy) = (bx - ax, by - ay);
                let span = dx * dx + dy * dy;
                let t = if span == 0.0 {
                    0.0
                } else {
                    (((x - ax) * dx + (y - ay) * dy) / span).clamp(0.0, 1.0)
                };
                (x - ax - t * dx).hypot(y - ay - t * dy)
            })
            .fold(f64::INFINITY, f64::min);
        // KiCad uses five internal units. Allow for rounding of both endpoints
        // and the anchor; points near the tolerance cannot prove either state.
        if distance <= 3.0 * PRECISION {
            Some(true)
        } else if distance > 7.0 * PRECISION {
            Some(false)
        } else {
            None
        }
    }
}

pub struct SymbolBox {
    core: Option<SymbolBounds>,
    outer: Option<SymbolBounds>,
}

impl SymbolBox {
    pub fn classify(&self, area: &RuleArea<'_>) -> Option<bool> {
        if self.core.and_then(|bounds| area.collides(bounds)) == Some(true) {
            Some(true)
        } else if self.outer.and_then(|bounds| area.collides(bounds)) == Some(false) {
            Some(false)
        } else {
            None
        }
    }
}

/// Include all saved library blocks so duplicate definitions stay ambiguous.
pub fn area_library(tree: &SexpNode) -> Vec<&SexpNode> {
    tree.find_all("lib_symbols")
        .into_iter()
        .flat_map(|block| block.find_all("symbol"))
        .collect()
}

/// Resolve the embedded definition for both area geometry and the power exemption.
pub fn find_unique_lib_symbol<'a>(
    libraries: &[&'a SexpNode],
    symbol: &SexpNode,
) -> Option<&'a SexpNode> {
    if repeated_setter(symbol, &["lib_id"]) || repeated_setter(symbol, &["lib_name"]) {
        return None;
    }
    let instance = symbol_instance(symbol)?;
    let key = instance.lib_symbol_name();
    let normalized = key.replace("{slash}", "/");
    let mut candidates = libraries.iter().copied().filter(|node| {
        node.get(1)
            .and_then(SexpNode::as_str)
            .is_some_and(|name| name.replace("{slash}", "/") == normalized)
    });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

pub fn symbol_box<'a>(
    symbol: &SexpNode,
    libraries: &[&SexpNode],
    reference: Option<&'a str>,
    field_override: impl Fn(&str) -> Option<&'a str>,
) -> SymbolBox {
    if !symbol_transform_is_ordered(symbol) || repeated_setter(symbol, &["at", "mirror"]) {
        return SymbolBox {
            core: None,
            outer: None,
        };
    }
    // KiCad can replace earlier fields. A superseded anchor cannot prove
    // collision or separation.
    let fields = symbol.find_all("property");
    let mut names = BTreeSet::new();
    if fields.iter().any(|field| {
        let private = matches!(field.get(1), Some(SexpNode::Atom(key)) if key == "private");
        field
            .get(1 + usize::from(private))
            .and_then(SexpNode::as_str)
            .is_some_and(|name| !names.insert(name.to_ascii_lowercase()))
    }) {
        return SymbolBox {
            core: None,
            outer: None,
        };
    }
    fn body(
        symbol: &SexpNode,
        libraries: &[&SexpNode],
    ) -> Option<(SymbolBounds, Option<SymbolBounds>)> {
        let instance = symbol_instance(symbol)?;
        if repeated_setter(symbol, &["unit", "body_style", "convert"])
            || mixed_body_style_setters(symbol)
        {
            return None;
        }
        if !instance.x.is_finite()
            || !instance.y.is_finite()
            || !matches!(instance.rotation, 0.0 | 90.0 | 180.0 | 270.0)
        {
            return None;
        }
        let library = find_unique_lib_symbol(libraries, symbol)?;
        let style = symbol
            .find_str("body_style")
            .or_else(|| symbol.find_str("convert"))
            .map_or(Some(1), |style| style.parse().ok())?;
        if !area_body_style_is_declared(library, style) {
            return None;
        }
        let transform = instance.pin_transform();
        let mut core = None;
        let mut complete = true;
        // The existing unit envelope includes pins and alternate styles. Do
        // not use it as a certain core: hidden and alternate pins need not
        // participate in KiCad's native box.
        let local = symbol_local_bounds_for_unit(library, instance.unit)?;
        let bounds = SymbolBounds::spanning(
            transform_pin(local.min_x, local.min_y, transform),
            transform_pin(local.max_x, local.max_y, transform),
        );
        let mut stroke = 0.0_f64;
        let mut pin_margin = 0.381 + PRECISION; // TARGET_PIN_RADIUS plus rounding
        for node in std::iter::once(library).chain(library.find_all("symbol")) {
            if std::ptr::eq(node, library) {
                if !root_drawings_match(instance.unit, style) {
                    continue;
                }
            } else {
                let name = node.get(1)?.as_str()?;
                let unit = parse_subsymbol_unit(name)?;
                let body_style: u32 = name.rsplit('_').next()?.parse().ok()?;
                if unit != 0 && unit != instance.unit || body_style != 0 && body_style != style {
                    continue;
                }
            }
            for item in node.children().unwrap_or_default() {
                if item.get(1).and_then(SexpNode::as_str) == Some("private") {
                    continue;
                }
                if repeated_setter(
                    item,
                    &[
                        "start", "mid", "end", "center", "radius", "pts", "stroke", "at", "length",
                    ],
                ) {
                    // Both the inner proof and the outer envelope rely on
                    // single setters. Do not use the earlier geometry.
                    return None;
                }
                if item.find("stroke").is_some_and(repeated_style_setting) {
                    return None;
                }
                if item.head() == Some("pin") {
                    if repeated_pin_visibility(item) {
                        return None;
                    }
                    let pin = parse_lib_pin(item)?;
                    if pin.hidden {
                        continue;
                    }
                    if !pin.length.is_finite()
                        || !pin.local_x.is_finite()
                        || !pin.local_y.is_finite()
                        || !matches!(pin.rotation, 0.0 | 90.0 | 180.0 | 270.0)
                    {
                        return None;
                    }
                    let angle = pin.rotation.to_radians();
                    let tip = transform_pin(pin.local_x, pin.local_y, transform);
                    let root = transform_pin(
                        pin.local_x + pin.length * angle.cos(),
                        pin.local_y + pin.length * angle.sin(),
                        transform,
                    );
                    include(&mut core, SymbolBounds::spanning(tip, root));
                    match item.get(2).and_then(SexpNode::as_str) {
                        Some("line" | "output_low") => {}
                        Some(
                            "inverted" | "clock" | "inverted_clock" | "input_low" | "clock_low"
                            | "edge_clock_high" | "non_logic",
                        ) => {
                            // The embedded library pins have no schematic parent.
                            // PIN_LAYOUT_CACHE uses half the saved text size for
                            // their decorations, which may extend twice that far.
                            for field in ["name", "number"] {
                                let records = item.find_all(field);
                                let effects = records.first().and_then(|node| node.find("effects"));
                                if records.len() > 1 || effects.is_some_and(repeated_style_setting)
                                {
                                    return None;
                                }
                                let size = effects
                                    .and_then(|node| node.find("font"))
                                    .and_then(|node| node.find_f64("size"));
                                match size {
                                    Some(size) if size.is_finite() && size >= 0.0 => {
                                        pin_margin = pin_margin.max(size + 2.0 * PRECISION);
                                    }
                                    _ => complete = false,
                                }
                            }
                        }
                        _ => complete = false,
                    }
                    continue;
                }
                let point = |node: &SexpNode| {
                    let (x, y) = (node.get_f64(1)?, node.get_f64(2)?);
                    (x.is_finite() && y.is_finite()).then(|| transform_pin(x, y, transform))
                };
                let draw_points = match item.head() {
                    Some("rectangle") => ["start", "end"]
                        .iter()
                        .map(|tag| item.find(tag).and_then(point))
                        .collect(),
                    Some("arc") => ["start", "mid", "end"]
                        .iter()
                        .map(|tag| item.find(tag).and_then(point))
                        .collect(),
                    Some("circle") => {
                        let center = item.find("center").and_then(point);
                        let radius = item
                            .find_f64("radius")
                            .filter(|r| r.is_finite() && *r >= 0.0);
                        match center.zip(radius) {
                            Some(((x, y), r)) => vec![Some((x - r, y - r)), Some((x + r, y + r))],
                            None => vec![None],
                        }
                    }
                    Some("polyline" | "bezier") => {
                        let points = item
                            .find("pts")
                            .map(|pts| pts.find_all("xy"))
                            .unwrap_or_default();
                        if points.len() < 2 {
                            complete = false;
                        }
                        let end = points.last().and_then(|node| point(node));
                        let start = points.first().and_then(|node| point(node));
                        if item.head() == Some("bezier") {
                            complete &= points.len() == 4
                                && points.iter().all(|node| point(node).is_some());
                            vec![start, end]
                        } else {
                            points.into_iter().map(point).collect()
                        }
                    }
                    Some("text" | "text_box" | "extends") => {
                        complete = false;
                        continue;
                    }
                    _ => continue,
                };
                for p in draw_points {
                    if let Some(p) = p {
                        include(&mut core, SymbolBounds::spanning(p, p));
                    } else {
                        complete = false;
                    }
                }
                if let Some(width) = item.find("stroke").and_then(|s| s.find_f64("width")) {
                    complete &= width.is_finite();
                    stroke = stroke.max(width.abs());
                }
            }
        }
        Some((
            core?,
            complete.then(|| grow(bounds, stroke.max(pin_margin))),
        ))
    }

    let (mut core, mut outer) = body(symbol, libraries)
        .map(|(core, outer)| (Some(core), outer))
        .unwrap_or_default();
    for field in fields {
        if discarded_simulation_field(field) {
            continue;
        }
        match parse_at(symbol)
            .ok_or("invalid symbol position")
            .and_then(|(_, _, angle)| {
                rule_area_field_bounds(field, Some(angle), |name| {
                    if name.eq_ignore_ascii_case("Reference") {
                        reference
                    } else {
                        field_override(name)
                    }
                })
            }) {
            Ok(Some((inner, field_outer))) => {
                if let Some(inner) = inner {
                    include(&mut core, inner);
                }
                if let Some(bounds) = outer.as_mut() {
                    *bounds = join(*bounds, field_outer);
                }
            }
            Ok(None) => {}
            Err(_) => outer = None,
        }
    }
    SymbolBox { core, outer }
}

pub fn sheet_box(sheet: &SexpNode) -> SymbolBox {
    let body = (|| {
        if repeated_setter(sheet, &["at", "size", "stroke"]) {
            return None;
        }
        let (x, y, _) = parse_at(sheet)?;
        let size = sheet.find("size")?;
        let (w, h) = (size.get_f64(1)?, size.get_f64(2)?);
        [x, y, w, h]
            .iter()
            .all(|v| v.is_finite())
            .then(|| SymbolBounds::spanning((x, y), (x + w, y + h)))
    })();
    let mut core = body;
    let mut outer = body
        .zip(sheet.find("stroke").and_then(|stroke| {
            (!repeated_style_setting(stroke))
                .then(|| stroke.find_f64("width"))
                .flatten()
        }))
        .filter(|(_, width)| width.is_finite() && *width > 0.0)
        .map(|(bounds, width)| grow(bounds, width / 2.0));
    // SCH_SHEET merges all field boxes, including hidden fields.
    for field in sheet.find_all("property") {
        match rule_area_field_bounds(field, None, |_| None) {
            Ok(Some((inner, extent))) => {
                if let Some(inner) = inner {
                    include(&mut core, inner);
                }
                outer = outer.map(|bounds| join(bounds, extent));
            }
            Ok(None) => {}
            Err(_) => outer = None,
        }
    }
    SymbolBox { core, outer }
}

fn discarded_simulation_field(field: &SexpNode) -> bool {
    let private = matches!(field.get(1), Some(SexpNode::Atom(name)) if name == "private");
    field
        .get(1 + usize::from(private))
        .and_then(SexpNode::as_str)
        .is_some_and(|name| matches!(name, "Sim.Enable" | "Spice_Netlist_Enabled"))
}

fn mixed_body_style_setters(symbol: &SexpNode) -> bool {
    // KiCad applies the legacy and current spellings to the same value in file order.
    symbol.find("body_style").is_some() && symbol.find("convert").is_some()
}

fn symbol_transform_is_ordered(symbol: &SexpNode) -> bool {
    // `at` resets the transform. Fields are stored in the transform active
    // when they are parsed, then drawn using the final transform.
    symbol
        .children()
        .unwrap_or_default()
        .iter()
        .filter_map(|node| match node.head() {
            Some("at") => Some(0),
            Some("mirror") => Some(1),
            Some("property") => Some(2),
            _ => None,
        })
        .is_sorted()
}

fn root_drawings_match(unit: u32, style: u32) -> bool {
    // Direct library drawings are loaded in unit 1, style 1, not as common.
    unit == 1 && style == 1
}

fn area_body_style_is_declared(library: &SexpNode, style: u32) -> bool {
    if style == 1 {
        return true;
    }
    // KiCad prunes drawings above the declared style count. Do not infer
    // legacy alternate styles or draw a selection from conflicting records.
    if style == 0 || repeated_setter(library, &["body_styles"]) {
        return false;
    }
    let Some(names) = library.find("body_styles").and_then(SexpNode::children) else {
        return false;
    };
    if names
        .iter()
        .skip(1)
        .any(|node| matches!(node, SexpNode::Atom(tag) if tag == "demorgan"))
    {
        style == 2
    } else {
        names.iter().skip(1).all(|node| node.as_str().is_some())
            && style as usize <= names.len().saturating_sub(1)
    }
}

fn repeated_pin_visibility(pin: &SexpNode) -> bool {
    // A bare hide and a (hide yes/no) record are setters for the same value.
    pin.children()
        .unwrap_or_default()
        .iter()
        .skip(3)
        .filter(|node| match node {
            SexpNode::Atom(tag) => tag == "hide",
            _ => node.head() == Some("hide"),
        })
        .take(2)
        .count()
        > 1
}

fn repeated_setter(node: &SexpNode, tags: &[&str]) -> bool {
    tags.iter().any(|tag| node.find_all(tag).len() > 1)
}

fn include(bounds: &mut Option<SymbolBounds>, next: SymbolBounds) {
    *bounds = Some(bounds.map_or(next, |bounds| join(bounds, next)));
}

fn join(mut a: SymbolBounds, b: SymbolBounds) -> SymbolBounds {
    a.include(b.min_x, b.min_y);
    a.include(b.max_x, b.max_y);
    a
}

fn intersects(bounds: SymbolBounds, polygon: &[Point]) -> bool {
    if bounds.min_x >= bounds.max_x || bounds.min_y >= bounds.max_y {
        return false;
    }
    let contains = |(x, y): Point| {
        bounds.min_x < x && x < bounds.max_x && bounds.min_y < y && y < bounds.max_y
    };
    let edges = || polygon.iter().zip(polygon.iter().cycle().skip(1));
    if polygon.iter().copied().any(contains) || edges().any(|(&a, &b)| bounds.segment_enters(a, b))
    {
        return true;
    }
    let (x, y) = (
        (bounds.min_x + bounds.max_x) / 2.0,
        (bounds.min_y + bounds.max_y) / 2.0,
    );
    edges()
        .filter(|&(&(ax, ay), &(bx, by))| {
            (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay)
        })
        .count()
        % 2
        != 0
}

fn grow(b: SymbolBounds, by: f64) -> SymbolBounds {
    SymbolBounds {
        min_x: b.min_x - by,
        min_y: b.min_y - by,
        max_x: b.max_x + by,
        max_y: b.max_y + by,
    }
}
