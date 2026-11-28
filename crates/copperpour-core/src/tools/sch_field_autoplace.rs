//! Plan Reference and Value positions on the saved sheet or a prospective authoring edit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use copperpour_sexp::{
    geometry::{point_on_segment, transform_pin},
    parse_sexp,
    schematic::{
        extract_bus_entries, extract_buses, extract_labels, extract_sheet_pins, extract_wires,
        find_lib_symbol, parse_at, pin_endpoint, resolve_lib_pins_for_unit_and_body_style,
        symbol_geometry_for_instance, symbol_instance, PinSegment, SymbolBounds, SymbolGeometry,
        SymbolInstance, Wire,
    },
    text_bounds::{field_drawn_orientation, symbol_field_extent},
    writer::{
        apply_edits, direct_children_with_tag, read_consistent, write_atomic_if_unchanged, SexpEdit,
    },
    DocumentRevision, SexpError, SexpNode,
};
use serde::Serialize;
use serde_json::{json, Value};

use super::{
    get_path, invalid_arg, is_symbol_quarter_turn, mutation_outcome_uncertain, opt_str_list,
    sch_analysis::{
        component_overlap_pairs, overlap_findings_with_field_clearance, OVERLAP_TOLERANCE,
    },
    sch_layout_score::{
        decode_saved_source, layout_verdict, parse_saved_sheet, read_saved_bytes, LayoutBaseline,
    },
    ToolContext, ToolDef,
};
use crate::{
    mcp::{error::ToolErrorKind, protocol::CallToolResult},
    tool,
};

const OPERATION: &str = "autoplace_schematic_fields";
const ROTATE_OPERATION: &str = "rotate_schematic_component";

fn plan_blocked(operation: &str, reason: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: operation.to_owned(),
            reasons: vec![reason.to_owned()],
        },
        format!("{reason} Nothing was written."),
    )
}

fn overlap_findings(
    tree: &SexpNode,
    wires: &[Wire],
    tolerance: f64,
) -> (Vec<Value>, usize, Vec<String>) {
    // A visible power Reference is a placement obstacle even though the legacy report omits it.
    overlap_findings_with_field_clearance(tree, wires, tolerance, true)
}
const POSITION_TOLERANCE: f64 = 0.001;
const GAP_MM: f64 = 0.635;

#[cfg(test)]
#[path = "sch_field_autoplace_tests.rs"]
mod tests;

pub(super) fn opt_in_schema() -> Value {
    json!({"type":"boolean","default":false,"description":"Try clear horizontal Reference and Value placement in this write. Protected fields stay fixed."})
}

pub(super) fn approval_schema() -> Value {
    json!({
        "type":"array","items":{"type":"string"},"uniqueItems":true,
        "description":"Individual field approval IDs from that preview. Omit to preview."
    })
}

pub(super) fn definition() -> ToolDef {
    tool!(
        "autoplace_schematic_fields",
        "Preview clear horizontal Reference and Value placements on a saved sheet. No fields \
         move without explicit approval. To apply, return the preview revision and the individual \
         approval IDs for the fields you want to move. Does not move symbols or wires.",
        json!({
            "type": "object", "required": ["schematic"],
            "properties": {
                "schematic": {"type":"string","description":"Path to the saved .kicad_sch sheet."},
                "revision": {"type":"string","description":"Revision from the preview; required with approvals."},
                "approvals": approval_schema()
            }
        }),
        |args, ctx| async move { handle_autoplace_schematic_fields(args, ctx).await }
    )
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Position {
    x_mm: f64,
    y_mm: f64,
    rotation_degrees: f64,
    justify: Vec<String>,
}

impl Position {
    fn read(field: &SexpNode) -> Option<Self> {
        let at = field.find("at")?;
        let (x_mm, y_mm, rotation_degrees) = parse_at(field)?;
        if ![x_mm, y_mm, rotation_degrees]
            .into_iter()
            .all(f64::is_finite)
            || at.get(3).is_some_and(|_| at.get_f64(3).is_none())
        {
            return None;
        }
        Some(Self {
            x_mm,
            y_mm,
            rotation_degrees,
            justify: field
                .find("effects")
                .and_then(|effects| effects.find("justify"))
                .and_then(SexpNode::children)
                .into_iter()
                .flatten()
                .skip(1)
                .filter_map(SexpNode::as_str)
                .map(str::to_owned)
                .collect(),
        })
    }

    fn at(&self) -> String {
        let fmt = copperpour_schematic_editor::types::fmt_f64;
        format!(
            "(at {} {} {})",
            fmt(self.x_mm),
            fmt(self.y_mm),
            fmt(self.rotation_degrees)
        )
    }
}

fn property_name(node: &SexpNode) -> Option<&str> {
    node.get(1)?.as_str()
}

fn property<'a>(symbol: &'a SexpNode, name: &str) -> Option<&'a SexpNode> {
    let mut matches = symbol
        .find_all("property")
        .into_iter()
        .filter(|p| property_name(p) == Some(name));
    let node = matches.next()?;
    matches.next().is_none().then_some(node)
}

fn flag(node: &SexpNode, name: &str) -> bool {
    node.find(name)
        .is_some_and(|flag| flag.get(1).and_then(SexpNode::as_str) != Some("no"))
}

fn matches_default(old: &Position, library: &Position, symbol: &SymbolInstance) -> bool {
    let (x, y) = transform_pin(library.x_mm, library.y_mm, symbol.pin_transform());
    (old.x_mm - x).abs() < POSITION_TOLERANCE
        && (old.y_mm - y).abs() < POSITION_TOLERANCE
        && (old.rotation_degrees - library.rotation_degrees)
            .rem_euclid(180.0)
            .min((library.rotation_degrees - old.rotation_degrees).rem_euclid(180.0))
            < POSITION_TOLERANCE
        && old.justify == library.justify
}

#[derive(Serialize)]
struct FieldPlan {
    name: &'static str,
    reason: &'static str,
    old: Option<Position>,
    proposed: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<FieldEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<Value>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FieldEvidence {
    LibraryDefault,
    FieldsAutoplaced,
    NewlyPlaced,
}

type FieldEligibility = BTreeMap<(String, &'static str), FieldEvidence>;

#[derive(Serialize)]
struct SymbolPlan {
    symbol_uuid: String,
    reference: String,
    unit: u32,
    fields: Vec<FieldPlan>,
}

struct Placed<'a> {
    node: &'a SexpNode,
    instance: SymbolInstance,
    library: Option<&'a SexpNode>,
    geometry: Option<SymbolGeometry>,
}

impl Placed<'_> {
    fn field_evidence(&self, name: &str, position: &Position) -> Option<FieldEvidence> {
        let library_default = self
            .library
            .and_then(|lib| property(lib, name))
            .and_then(Position::read)
            .is_some_and(|lib| matches_default(position, &lib, &self.instance));
        if library_default {
            return Some(FieldEvidence::LibraryDefault);
        }
        let marker = self.node.find("fields_autoplaced")?;
        match marker.children()? {
            [_] => Some(FieldEvidence::FieldsAutoplaced),
            [_, value] if value.as_str() == Some("yes") => Some(FieldEvidence::FieldsAutoplaced),
            _ => None,
        }
    }

    fn measure(&self, field: &SexpNode) -> Option<SymbolBounds> {
        let lib = self.library?;
        let font = field.find("effects")?.find("font")?;
        let size = font.find("size")?;
        if font.find("face").is_some()
            || !(1..=2).all(|axis| {
                size.get_f64(axis)
                    .is_some_and(|n| (0.001..=250.0).contains(&n))
            })
            || field.get(2)?.as_str()?.contains("${")
        {
            return None;
        }
        Some(
            symbol_field_extent(field, &self.instance, Some(lib))
                .ok()??
                .run,
        )
    }
}

fn centered(field: &SexpNode, angle: f64) -> Option<SexpNode> {
    let SexpNode::List(mut children) = field.clone() else {
        return None;
    };
    let at = children.iter_mut().find(|n| n.head() == Some("at"))?;
    *at = parse_sexp(&format!("(at 0 0 {angle})")).ok()?;
    let SexpNode::List(effects) = children.iter_mut().find(|n| n.head() == Some("effects"))? else {
        return None;
    };
    effects.retain(|n| n.head() != Some("justify"));
    Some(SexpNode::List(children))
}

fn touches_pin(run: SymbolBounds, pin: &PinSegment) -> bool {
    run.segment_enters(pin.tip, pin.root)
        || [pin.tip, pin.root]
            .into_iter()
            .any(|(x, y)| x > run.min_x && x < run.max_x && y > run.min_y && y < run.max_y)
}

fn inside_body_or_pin(run: SymbolBounds, geometry: &SymbolGeometry) -> bool {
    geometry
        .body
        .is_some_and(|body| run.interiors_intersect(body))
        || geometry.pins.iter().any(|pin| touches_pin(run, pin))
}

fn field_crossings(findings: &[Value], uuid: &str, names: &[&str]) -> Vec<Value> {
    findings
        .iter()
        .filter(|f| {
            matches!(
                f["type"].as_str(),
                Some("wire_field_overlap" | "wire_label_overlap")
            ) && f["b"]["symbol_uuid"] == uuid
                && f["b"]["field"]
                    .as_str()
                    .is_some_and(|name| names.contains(&name))
        })
        .map(|f| f["a"].clone())
        .collect()
}

fn item_range(source: &str, tag: &str, uuid: &str) -> Option<(usize, usize)> {
    let mut blocks = direct_children_with_tag(source, "kicad_sch", tag)
        .into_iter()
        .filter(|&(start, end)| {
            parse_sexp(&source[start..end]).is_ok_and(|s| s.find_str("uuid") == Some(uuid))
        });
    let range = blocks.next()?;
    blocks.next().is_none().then_some(range)
}

fn move_field(source: &str, uuid: &str, name: &str, pos: &Position) -> Option<String> {
    let (start, end) = item_range(source, "symbol", uuid)?;
    let symbol = &source[start..end];
    let (p, q) = direct_children_with_tag(symbol, "symbol", "property")
        .into_iter()
        .find(|&(p, q)| {
            parse_sexp(&symbol[p..q]).is_ok_and(|node| property_name(&node) == Some(name))
        })?;
    let prop = &symbol[p..q];
    let at_ranges = direct_children_with_tag(prop, "property", "at");
    let [(at, stop)] = at_ranges.as_slice() else {
        return None;
    };
    let base = start + p;
    let mut edits = vec![SexpEdit::replace(base + *at, base + *stop, pos.at())];
    let (e, end) = *direct_children_with_tag(prop, "property", "effects").first()?;
    for (j, k) in direct_children_with_tag(&prop[e..end], "effects", "justify") {
        edits.push(SexpEdit::delete(base + e + j, base + e + k));
    }
    for (j, k) in direct_children_with_tag(symbol, "symbol", "fields_autoplaced") {
        edits.push(SexpEdit::delete(start + j, start + k));
    }
    Some(apply_edits(source.to_owned(), edits))
}

fn placed_symbols(tree: &SexpNode, measure_geometry: bool) -> Vec<Placed<'_>> {
    let libraries = tree
        .find("lib_symbols")
        .map(|libs| libs.find_all("symbol"))
        .unwrap_or_default();
    tree.find_all("symbol")
        .into_iter()
        .filter_map(|node| {
            let instance = symbol_instance(node)?;
            let library = find_lib_symbol(&libraries, &instance);
            let geometry = library
                .filter(|_| measure_geometry)
                .and_then(|lib| symbol_geometry_for_instance(lib, &instance));
            Some(Placed {
                node,
                instance,
                library,
                geometry,
            })
        })
        .collect()
}

type Point = (f64, f64);
type Segment = (Point, Point);

/// Read the drawn bus and tap segments without assigning any electrical role
/// to their endpoints. Incomplete or ambiguous records cannot prove clearance.
fn bus_obstacles(tree: &SexpNode) -> Option<Vec<Segment>> {
    let buses = extract_buses(tree);
    let bus_nodes = tree.find_all("bus");
    let unambiguous = |node: &SexpNode, tag: &str| node.find_all(tag).len() == 1;
    let has_pair = |node: &SexpNode| node.children().is_some_and(|items| items.len() == 3);
    if buses.len() != bus_nodes.len()
        || bus_nodes.iter().any(|node| match node.find("pts") {
            Some(pts) => {
                !unambiguous(node, "pts")
                    || node.find("start").is_some()
                    || node.find("end").is_some()
                    || !has_pair(pts)
                    || pts.find_all("xy").len() != 2
                    || !pts.find_all("xy").into_iter().all(has_pair)
            }
            None => ["start", "end"]
                .into_iter()
                .any(|tag| !unambiguous(node, tag) || !node.find(tag).is_some_and(has_pair)),
        })
    {
        return None;
    }
    if tree.find_all("bus_entry").iter().any(|node| {
        ["at", "size"]
            .into_iter()
            .any(|tag| !unambiguous(node, tag) || !node.find(tag).is_some_and(has_pair))
    }) {
        return None;
    }
    buses
        .into_iter()
        .map(|w| Some(((w.x1, w.y1), (w.x2, w.y2))))
        .chain(
            extract_bus_entries(tree)
                .into_iter()
                .map(|entry| entry.endpoints()),
        )
        .map(|segment| {
            let ((x1, y1), (x2, y2)) = segment?;
            ([x1, y1, x2, y2, x2 - x1, y2 - y1]
                .into_iter()
                .all(f64::is_finite)
                && (x1, y1) != (x2, y2))
                .then_some(((x1, y1), (x2, y2)))
        })
        .collect()
}

fn segment_bounds(((x1, y1), (x2, y2)): Segment) -> SymbolBounds {
    SymbolBounds {
        min_x: x1.min(x2),
        min_y: y1.min(y2),
        max_x: x1.max(x2),
        max_y: y1.max(y2),
    }
}

/// `route` is cardinal; bus entries and bus strokes need not be. Test their
/// actual segments, not their enclosing rectangles, with room for coordinate
/// rounding. The padded box puts even a touch on a route endpoint inside.
fn bus_contacts_route(route: Segment, obstacles: &[Segment]) -> bool {
    let mut bounds = segment_bounds(route);
    bounds.min_x -= POSITION_TOLERANCE;
    bounds.max_x += POSITION_TOLERANCE;
    bounds.min_y -= POSITION_TOLERANCE;
    bounds.max_y += POSITION_TOLERANCE;
    let inside = |(x, y)| {
        (bounds.min_x..=bounds.max_x).contains(&x) && (bounds.min_y..=bounds.max_y).contains(&y)
    };
    obstacles
        .iter()
        .any(|&(a, b)| inside(a) || inside(b) || bounds.segment_enters(a, b))
}

fn dedicated_power_lead(
    source: &str,
    baseline: &LayoutBaseline<'_>,
    tree: &SexpNode,
    placed: &[Placed<'_>],
    wires: &[Wire],
    crossings: &[Value],
) -> Option<Value> {
    let first = crossings.first()?;
    let id = first["uuid"].as_str()?;
    if crossings.iter().any(|wire| wire["uuid"] != id) {
        return None;
    }
    let lead = wires.iter().find(|wire| wire.uuid.as_deref() == Some(id))?;
    let from = (lead.x1, lead.y1);
    let to = (lead.x2, lead.y2);
    let bounds = segment_bounds((from, to));
    const STUB_MM: f64 = 2.54;
    if (from.0 != to.0 && from.1 != to.1) || (from.0 - to.0).hypot(from.1 - to.1) <= STUB_MM {
        return None;
    }
    let on_lead = |(x, y)| point_on_segment(x, y, from.0, from.1, to.0, to.1, POSITION_TOLERANCE);
    let pins: Vec<Vec<PinSegment>> = placed
        .iter()
        .map(|s| {
            let transform = s.instance.pin_transform();
            resolve_lib_pins_for_unit_and_body_style(
                s.library?,
                s.instance.unit,
                super::symbol_body_style(s.node),
            )?
            .iter()
            .map(|pin| {
                let angle = pin.rotation.to_radians();
                let tip = pin_endpoint(pin, transform);
                let root = transform_pin(
                    pin.local_x + pin.length * angle.cos(),
                    pin.local_y + pin.length * angle.sin(),
                    transform,
                );
                [tip.0, tip.1, root.0, root.1]
                    .into_iter()
                    .all(f64::is_finite)
                    .then_some(PinSegment { tip, root })
            })
            .collect()
        })
        .collect::<Option<_>>()?;
    let attached: Vec<_> = pins
        .iter()
        .enumerate()
        .flat_map(|(i, pins)| {
            pins.iter()
                .filter(|pin| on_lead(pin.tip))
                .map(move |pin| (i, pin))
        })
        .collect();
    let [(a, a_pin), (b, b_pin)] = attached.as_slice() else {
        return None;
    };
    let is_power = |i: usize| {
        placed[i]
            .library
            .is_some_and(|lib| lib.find("power").is_some())
    };
    let (power_index, pin, power_pin) = match (is_power(*a), is_power(*b)) {
        (false, true) => (*b, a_pin, b_pin),
        (true, false) => (*a, b_pin, a_pin),
        _ => return None,
    };
    let power = &placed[power_index];
    let same = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).hypot(p.1 - q.1) < POSITION_TOLERANCE;
    if pins[power_index].len() != 1
        || ![(from, to), (to, from)]
            .into_iter()
            .any(|(a, b)| same(pin.tip, a) && same(power_pin.tip, b))
        || !same(power_pin.tip, (power.instance.x, power.instance.y))
    {
        return None;
    }
    let anchors: Vec<_> = extract_labels(tree)
        .into_iter()
        .map(|l| (l.x, l.y))
        .chain(extract_sheet_pins(tree))
        .chain(
            ["junction", "no_connect", "directive_label", "netclass_flag"]
                .into_iter()
                .flat_map(|tag| tree.find_all(tag))
                .filter_map(parse_at)
                .map(|(x, y, _)| (x, y)),
        )
        .collect();
    if anchors.iter().copied().any(on_lead)
        || wires
            .iter()
            .filter(|wire| !std::ptr::eq(*wire, lead))
            .any(|w| !bounds.separated(segment_bounds(((w.x1, w.y1), (w.x2, w.y2)))))
    {
        return None;
    }
    let bus_geometry = bus_obstacles(tree)?;
    if bus_contacts_route((from, to), &bus_geometry) {
        return None;
    }
    let dx = pin.tip.0 - pin.root.0;
    let dy = pin.tip.1 - pin.root.1;
    let power_body = power.geometry.as_ref()?.body?;
    // Only recommend an upright symbol that already faces away from the pin.
    if dx.abs() > POSITION_TOLERANCE
        || !((dy < 0.0 && power_body.max_y <= power_pin.tip.1 + POSITION_TOLERANCE)
            || (dy > 0.0 && power_body.min_y >= power_pin.tip.1 - POSITION_TOLERANCE))
    {
        return None;
    }
    let destination = (pin.tip.0, round_mm(pin.tip.1 + dy.signum() * STUB_MM));
    if bus_contacts_route((pin.tip, destination), &bus_geometry) {
        return None;
    }
    let on_stub = |(x, y)| {
        point_on_segment(
            x,
            y,
            pin.tip.0,
            pin.tip.1,
            destination.0,
            destination.1,
            POSITION_TOLERANCE,
        )
    };
    if anchors.iter().copied().any(on_stub)
        || pins
            .iter()
            .flatten()
            .any(|other| !std::ptr::eq(other, *pin) && on_stub(other.tip))
    {
        return None;
    }
    // Neither a junction-less contact nor a pinless extension necessarily
    // changes the layout verdict. The replacement lead must stay dedicated.
    let stub_bounds = segment_bounds((pin.tip, destination));
    if wires
        .iter()
        .filter(|wire| !std::ptr::eq(*wire, lead))
        .any(|w| !stub_bounds.separated(segment_bounds(((w.x1, w.y1), (w.x2, w.y2)))))
    {
        return None;
    }
    let power_uuid = power.instance.uuid.as_deref()?;
    let after = parse_sexp(&power_lead_trial(
        source,
        power_uuid,
        id,
        pin.tip,
        destination,
    )?)
    .ok()?;
    if baseline.verdict(&after) != "improved"
        || !field_collisions(&after).is_subset(&field_collisions(tree))
    {
        return None;
    }
    let (findings, _, _) = overlap_findings(&after, &extract_wires(&after), OVERLAP_TOLERANCE);
    if findings.iter().any(|finding| {
        (matches!(
            finding["type"].as_str(),
            Some("wire_field_overlap" | "wire_label_overlap")
        ) && finding["a"]["uuid"] == id)
            || (finding["type"] == "wire_label_overlap"
                && finding["b"]["symbol_uuid"] == power_uuid)
    }) {
        return None;
    }
    Some(json!({
        "reason": "move_power_to_pin",
        "power_symbol_uuid": power_uuid,
        "power_reference": power.instance.reference,
        "wire_uuid": id,
        "pin": {"x_mm": pin.tip.0, "y_mm": pin.tip.1},
        "destination": {"x_mm": destination.0, "y_mm": destination.1},
        "stub_length_mm": STUB_MM,
    }))
}

fn power_lead_trial(
    source: &str,
    power_uuid: &str,
    wire_uuid: &str,
    pin: (f64, f64),
    destination: (f64, f64),
) -> Option<String> {
    // The trial exists only to ask the shared gate. #770 owns applying the move.
    let (symbol_start, symbol_end) = item_range(source, "symbol", power_uuid)?;
    let symbol = &source[symbol_start..symbol_end];
    let (x, y, _) = parse_at(&parse_sexp(symbol).ok()?)?;
    let (dx, dy) = (destination.0 - x, destination.1 - y);
    let mut at_ranges = direct_children_with_tag(symbol, "symbol", "at");
    for (start, end) in direct_children_with_tag(symbol, "symbol", "property") {
        at_ranges.extend(
            direct_children_with_tag(&symbol[start..end], "property", "at")
                .into_iter()
                .map(|(a, b)| (start + a, start + b)),
        );
    }
    let fmt = copperpour_schematic_editor::types::fmt_f64;
    let mut edits = Vec::new();
    for (start, end) in at_ranges {
        let at = parse_sexp(&symbol[start..end]).ok()?;
        let suffix = at.get(3).and_then(SexpNode::as_str).unwrap_or("0");
        edits.push(SexpEdit::replace(
            symbol_start + start,
            symbol_start + end,
            format!(
                "(at {} {} {suffix})",
                fmt(at.get_f64(1)? + dx),
                fmt(at.get_f64(2)? + dy)
            ),
        ));
    }
    let (wire_start, wire_end) = item_range(source, "wire", wire_uuid)?;
    let (start, end) =
        *direct_children_with_tag(&source[wire_start..wire_end], "wire", "pts").first()?;
    edits.push(SexpEdit::replace(
        wire_start + start,
        wire_start + end,
        format!(
            "(pts (xy {} {}) (xy {} {}))",
            fmt(pin.0),
            fmt(pin.1),
            fmt(destination.0),
            fmt(destination.1)
        ),
    ));
    Some(apply_edits(source.to_owned(), edits))
}

// The planner uses the saved angles to measure obstacles. Do not certify
// a drawing if any placed symbol has an angle we cannot measure reliably.
fn saved_angles_are_measurable(tree: &SexpNode) -> bool {
    tree.find_all("symbol").into_iter().all(|symbol| {
        symbol.find("at").is_some_and(|at| {
            at.get(3).is_none() || at.get_f64(3).is_some_and(is_symbol_quarter_turn)
        })
    })
}

fn options(
    source: &str,
    tree: &SexpNode,
    selected: impl Fn(&str) -> bool,
    authoring_eligible: Option<&FieldEligibility>,
    saved_angles_measurable: bool,
) -> Vec<SymbolPlan> {
    let geometry_allowed = saved_angles_measurable && saved_angles_are_measurable(tree);
    let placed = placed_symbols(tree, geometry_allowed);
    let wires = extract_wires(tree);
    let findings = if geometry_allowed {
        overlap_findings(tree, &wires, OVERLAP_TOLERANCE).0
    } else {
        Vec::new()
    };
    let baseline = std::cell::OnceCell::new();
    let mut reports = Vec::new();
    for part in &placed {
        let Some(uuid) = part.instance.uuid.as_deref() else {
            continue;
        };
        if !selected(uuid) {
            continue;
        }
        let mut fields = Vec::new();
        let mut movers = Vec::new();
        let geometry = part.geometry.as_ref().filter(|g| g.body.is_some());
        for name in ["Reference", "Value"] {
            let Some(field) = property(part.node, name) else {
                continue;
            };
            let old = Position::read(field);
            let lib_field = part.library.and_then(|lib| property(lib, name));
            let evidence = geometry_allowed
                .then(|| match authoring_eligible {
                    Some(fields) => fields.get(&(uuid.to_owned(), name)).copied(),
                    None => old.as_ref().and_then(|pos| part.field_evidence(name, pos)),
                })
                .flatten();
            let mut reason = "cannot_measure";
            let crossings = field_crossings(&findings, uuid, &[name]);
            let suggestion = (!crossings.is_empty())
                .then(|| {
                    dedicated_power_lead(
                        source,
                        baseline.get_or_init(|| LayoutBaseline::new(tree)),
                        tree,
                        &placed,
                        &wires,
                        &crossings,
                    )
                })
                .flatten();
            if flag(field, "do_not_autoplace")
                || lib_field.is_some_and(|f| flag(f, "do_not_autoplace"))
            {
                reason = "do_not_autoplace";
            } else if flag(field, "hide") || field.find("effects").is_some_and(|e| flag(e, "hide"))
            {
                reason = "hidden";
            } else if let Some(((old, run), _)) =
                old.as_ref().zip(part.measure(field)).zip(geometry)
            {
                let overlaps_text = findings.iter().any(|f| {
                    f["type"] == "text_overlap"
                        && [&f["a"], &f["b"]]
                            .into_iter()
                            .any(|side| side["symbol_uuid"] == uuid && side["field"] == name)
                });
                if suggestion.is_some() {
                    reason = "move_power_to_pin";
                } else if evidence.is_none() {
                    reason = "positioned";
                } else if crossings.is_empty()
                    && !overlaps_text
                    && !placed
                        .iter()
                        .filter_map(|p| p.geometry.as_ref())
                        .any(|g| inside_body_or_pin(run, g))
                    && field_drawn_orientation(old.rotation_degrees, part.instance.rotation)
                        == "horizontal"
                {
                    reason = "already_clear";
                } else {
                    reason = "no_clear_side";
                    movers.push((fields.len(), name, field));
                }
            }
            fields.push(FieldPlan {
                name,
                reason,
                old,
                proposed: None,
                evidence,
                approval: None,
                suggestion,
            });
        }
        if let Some((geometry, body)) = geometry.and_then(|g| g.body.map(|body| (g, body))) {
            if !movers.is_empty() {
                let angle = if field_drawn_orientation(0.0, part.instance.rotation) == "horizontal"
                {
                    0.0
                } else {
                    90.0
                };
                let probes: Option<Vec<_>> = movers
                    .iter()
                    .map(|(_, _, field)| part.measure(&centered(field, angle)?))
                    .collect();
                if let Some(probes) = probes {
                    let mut sides = [0, 1, 2, 3]; // above, right, below, left
                    sides.sort_by_key(|side| {
                        geometry
                            .pins
                            .iter()
                            .filter(|pin| {
                                let (dx, dy) = (pin.tip.0 - pin.root.0, pin.tip.1 - pin.root.1);
                                let pin_side = if dx.abs() >= dy.abs() {
                                    if dx > 0.0 {
                                        1
                                    } else {
                                        3
                                    }
                                } else if dy > 0.0 {
                                    2
                                } else {
                                    0
                                };
                                pin_side == *side
                            })
                            .count()
                    });
                    let names = movers.iter().map(|(_, name, _)| *name).collect::<Vec<_>>();
                    let mut candidate = None;
                    let mut labels_neighbour = false;
                    for side in sides {
                        let cx = (body.min_x + body.max_x) / 2.0;
                        let cy = (body.min_y + body.max_y) / 2.0;
                        let height: f64 = probes.iter().map(|r| r.max_y - r.min_y + GAP_MM).sum();
                        let width = probes.iter().map(|r| r.max_x - r.min_x).fold(0.0, f64::max);
                        let (x, mut y) = match side {
                            0 => (cx, body.min_y - height),
                            1 => (body.max_x + GAP_MM + width / 2.0, cy - height / 2.0),
                            2 => (cx, body.max_y + GAP_MM),
                            _ => (body.min_x - GAP_MM - width / 2.0, cy - height / 2.0),
                        };
                        let mut trial = source.to_owned();
                        let mut positions = Vec::new();
                        for ((idx, name, _), probe) in movers.iter().zip(&probes) {
                            let pos = Position {
                                x_mm: round_mm(x - (probe.min_x + probe.max_x) / 2.0),
                                y_mm: round_mm(y - probe.min_y),
                                rotation_degrees: angle,
                                justify: Vec::new(),
                            };
                            y += probe.max_y - probe.min_y + GAP_MM;
                            let Some(next) = move_field(&trial, uuid, name, &pos) else {
                                break;
                            };
                            positions.push((*idx, pos));
                            trial = next;
                        }
                        if positions.len() == movers.len()
                            && parse_sexp(&trial).is_ok_and(|after| {
                                let (clear, neighbour) = clearance(&after, uuid, &names);
                                labels_neighbour |= neighbour;
                                clear
                                    && matches!(
                                        baseline
                                            .get_or_init(|| LayoutBaseline::new(tree))
                                            .verdict(&after),
                                        "improved" | "unchanged"
                                    )
                            })
                        {
                            candidate = Some(positions);
                            break;
                        }
                    }
                    if candidate.is_none() && labels_neighbour {
                        for (index, _, _) in &movers {
                            fields[*index].reason = "would_label_neighbour";
                        }
                    }
                    for (idx, pos) in candidate.into_iter().flatten() {
                        let field = &mut fields[idx];
                        field.reason = "clear_position";
                        field.proposed = Some(pos);
                    }
                }
            }
        }
        reports.push(SymbolPlan {
            symbol_uuid: uuid.to_owned(),
            reference: part.instance.reference.clone(),
            unit: part.instance.unit,
            fields,
        });
    }
    reports
}

fn round_mm(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// Project the shared findings onto Reference/Value contacts, in either
/// direction for text on text. Include protected text as an obstacle.
fn reported_field_contacts(finding: &Value) -> impl Iterator<Item = (&Value, &Value, &Value)> {
    let a = &finding["a"];
    let b = &finding["b"];
    [(a, b, &finding["bounds_b"]), (b, a, &finding["bounds_a"])]
        .into_iter()
        .filter(move |(field, _, _)| {
            matches!(
                finding["type"].as_str(),
                Some("text_overlap" | "wire_field_overlap" | "wire_label_overlap")
            ) && field["kind"] == "field"
                && matches!(field["field"].as_str(), Some("Reference" | "Value"))
        })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FieldObstacle {
    Reported(String),
    OwnBody,
    // The library unit and its ordered pin list do not change during a turn.
    Pin(String, usize),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FieldCollision {
    symbol: String,
    field: String,
    obstacle: FieldObstacle,
}

/// Compare *which objects* collide, not counts or moving bounds. Include
/// stationary fields: a turned symbol's pins or body can hit those too.
fn field_collisions(tree: &SexpNode) -> BTreeSet<FieldCollision> {
    let wires = extract_wires(tree);
    let (findings, _, _) = overlap_findings(tree, &wires, OVERLAP_TOLERANCE);
    let mut collisions = BTreeSet::new();
    for (field, obstacle, bounds) in findings.iter().flat_map(reported_field_contacts) {
        let Some(symbol) = field["symbol_uuid"].as_str() else {
            continue;
        };
        let identity = match obstacle["kind"].as_str() {
            Some("field") if obstacle["symbol_uuid"].is_string() => {
                json!(["field", obstacle["symbol_uuid"], obstacle["field"]])
            }
            Some("symbol_body") if obstacle["symbol_uuid"].is_string() => {
                json!(["symbol_body", obstacle["symbol_uuid"]])
            }
            Some("wire" | "label") if obstacle["uuid"].is_string() => {
                json!([obstacle["kind"], obstacle["uuid"]])
            }
            // UUID-less labels of the same net can sit in different places.
            // Neither labels nor wires move in a rotation.
            Some("label") => json!([obstacle, bounds]),
            _ => obstacle.clone(),
        };
        collisions.insert(FieldCollision {
            symbol: symbol.to_owned(),
            field: field["field"].as_str().unwrap_or_default().to_owned(),
            obstacle: FieldObstacle::Reported(identity.to_string()),
        });
    }

    let placed = placed_symbols(tree, true);
    for own in &placed {
        let Some(uuid) = own.instance.uuid.as_deref() else {
            continue;
        };
        for name in ["Reference", "Value"] {
            let Some(run) = property(own.node, name).and_then(|field| own.measure(field)) else {
                continue;
            };
            let collision = |obstacle| FieldCollision {
                symbol: uuid.to_owned(),
                field: name.to_owned(),
                obstacle,
            };
            if own
                .geometry
                .as_ref()
                .and_then(|g| g.body)
                .is_some_and(|body| run.interiors_intersect(body))
            {
                collisions.insert(collision(FieldObstacle::OwnBody));
            }
            for part in &placed {
                let (Some(id), Some(geometry)) = (&part.instance.uuid, &part.geometry) else {
                    continue;
                };
                for (index, pin) in geometry.pins.iter().enumerate() {
                    if touches_pin(run, pin) {
                        collisions.insert(collision(FieldObstacle::Pin(id.clone(), index)));
                    }
                }
            }
        }
    }
    collisions
}

/// Return whether the fields are clear and whether ownership was the only objection.
fn clearance(tree: &SexpNode, uuid: &str, names: &[&str]) -> (bool, bool) {
    let wires = extract_wires(tree);
    let (findings, _, _) = overlap_findings(tree, &wires, OVERLAP_TOLERANCE);
    if findings
        .iter()
        .flat_map(reported_field_contacts)
        .any(|(field, _, _)| {
            field["symbol_uuid"] == uuid
                && field["field"].as_str().is_some_and(|n| names.contains(&n))
        })
    {
        return (false, false);
    }
    let placed = placed_symbols(tree, true);
    let Some(own) = placed
        .iter()
        .find(|p| p.instance.uuid.as_deref() == Some(uuid))
    else {
        return (false, false);
    };
    let mut labels_neighbour = false;
    let unobstructed = names.iter().all(|name| {
        property(own.node, name)
            .and_then(|field| own.measure(field))
            .is_some_and(|run| {
                let distance_to = |body: SymbolBounds| {
                    let (x, y) = ((run.min_x + run.max_x) / 2.0, (run.min_y + run.max_y) / 2.0);
                    (x - x.clamp(body.min_x, body.max_x)).hypot(y - y.clamp(body.min_y, body.max_y))
                };
                let Some(body) = own.geometry.as_ref().and_then(|g| g.body) else {
                    return false;
                };
                if run.interiors_intersect(body) {
                    return false;
                }
                placed.iter().all(|part| {
                    part.geometry.as_ref().is_some_and(|g| {
                        if part.instance.uuid.as_deref() != Some(uuid)
                            && g.body
                                .is_some_and(|other| distance_to(body) >= distance_to(other))
                        {
                            labels_neighbour = true;
                        }
                        !g.pins.iter().any(|pin| touches_pin(run, pin))
                    })
                })
            })
    });
    (
        unobstructed && !labels_neighbour,
        unobstructed && labels_neighbour,
    )
}

fn verified_positions(
    tree: &SexpNode,
    edits: &[(String, &'static str, Position)],
) -> Option<Vec<Value>> {
    edits.iter().map(|(uuid, field, expected)| {
        let node = tree.find_all("symbol").into_iter().find(|n| n.find_str("uuid") == Some(uuid))?;
        let observed = Position::read(property(node, field)?)?;
        (observed == *expected).then(|| json!({
            "symbol_uuid": uuid, "unit": node.find("unit").and_then(|n| n.get(1)).and_then(SexpNode::as_str).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1),
            "reference": property(node, "Reference").and_then(|n| n.get(2)).and_then(SexpNode::as_str),
            "field": field, "position": observed
        }))
    }).collect()
}

/// Offered fields left unapproved whose only evidence was KiCad's
/// `fields_autoplaced` marker. An approved write on the same symbol clears that
/// marker, so the next preview reports them `positioned`.
fn withdrawn_offers(plans: &[SymbolPlan], edits: &[FieldEdit]) -> Vec<Value> {
    let approved = edits
        .iter()
        .map(|(uuid, name, _)| (uuid.as_str(), *name))
        .collect::<BTreeSet<_>>();
    plans
        .iter()
        .filter(|symbol| approved.iter().any(|(uuid, _)| *uuid == symbol.symbol_uuid))
        .flat_map(|symbol| {
            symbol
                .fields
                .iter()
                .filter(|field| {
                    field.approval.is_some()
                        && matches!(field.evidence, Some(FieldEvidence::FieldsAutoplaced))
                        && !approved.contains(&(symbol.symbol_uuid.as_str(), field.name))
                })
                .map(|field| {
                    json!({
                        "symbol_uuid": symbol.symbol_uuid, "reference": symbol.reference,
                        "unit": symbol.unit, "field": field.name,
                    })
                })
        })
        .collect()
}

fn approved_candidate(
    source: &str,
    tree: &SexpNode,
    edits: &[(String, &'static str, Position)],
) -> Option<String> {
    if edits.is_empty() {
        return Some(source.to_owned());
    }
    let mut candidate = source.to_owned();
    for (uuid, name, pos) in edits {
        candidate = move_field(&candidate, uuid, name, pos)?;
    }
    let after = parse_sexp(&candidate).ok()?;
    verified_positions(&after, edits)?;
    if edits
        .iter()
        .any(|(uuid, name, _)| !clearance(&after, uuid, &[*name]).0)
        || !matches!(layout_verdict(tree, &after), "improved" | "unchanged")
    {
        return None;
    }
    Some(candidate)
}

pub(super) struct PreviewAuthorization {
    revision: String,
    approvals: Option<Vec<String>>,
}

/// Check the revision of the exact bytes before decoding or resolving targets.
pub(super) fn read_preview_source(
    args: &Value,
    path: &Path,
) -> anyhow::Result<Result<(String, PreviewAuthorization), CallToolResult>> {
    if !path.is_file() {
        return Ok(Err(CallToolResult::error_kind(
            ToolErrorKind::file_not_found(path),
            format!("Schematic '{}' not found", path.display()),
        )));
    }
    let bytes = match read_saved_bytes(path) {
        Ok(bytes) => bytes,
        Err(error) => return Ok(Err(error)),
    };
    let revision = document_revision(path, &bytes)?;
    let approvals = match requested_approvals(args, &revision, path) {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    Ok(decode_saved_source(path, bytes).map(|source| {
        (
            source,
            PreviewAuthorization {
                revision,
                approvals,
            },
        )
    }))
}

fn saved_field_evidence(before: &SexpNode, selected: &BTreeSet<String>) -> FieldEligibility {
    let mut eligible = BTreeMap::new();
    for part in placed_symbols(before, true) {
        let Some(uuid) = part
            .instance
            .uuid
            .as_ref()
            .filter(|u| selected.contains(*u))
        else {
            continue;
        };
        for name in ["Reference", "Value"] {
            if let Some(evidence) = property(part.node, name)
                .and_then(Position::read)
                .and_then(|pos| part.field_evidence(name, &pos))
            {
                eligible.insert((uuid.clone(), name), evidence);
            }
        }
    }
    eligible
}

pub(super) fn autoplace_authoring(
    before: &str,
    mut source: String,
    selected: &[String],
) -> (String, AuthoringFields) {
    let eligible = selected
        .iter()
        .flat_map(|uuid| {
            ["Reference", "Value"].map(|name| ((uuid.clone(), name), FieldEvidence::NewlyPlaced))
        })
        .collect();
    let mut reports = Vec::new();
    // Use the original bytes, not the editor's potentially normalized angles.
    let saved_angles_measurable = !selected.is_empty()
        && parse_sexp(before).is_ok_and(|tree| saved_angles_are_measurable(&tree));
    for uuid in selected {
        let Ok(tree) = parse_sexp(&source) else { break };
        let mut plans = options(
            &source,
            &tree,
            |id| id == uuid,
            Some(&eligible),
            saved_angles_measurable,
        );
        let edits = plans
            .iter()
            .flat_map(|plan| {
                plan.fields
                    .iter()
                    .filter_map(|f| Some((uuid.clone(), f.name, f.proposed.clone()?)))
            })
            .collect::<Vec<_>>();
        if !edits.is_empty() {
            if let Some(candidate) = approved_candidate(&source, &tree, &edits) {
                source = candidate;
            } else {
                for field in plans.iter_mut().flat_map(|plan| &mut plan.fields) {
                    if field.proposed.take().is_some() {
                        field.reason = "no_clear_side";
                    }
                }
            }
        }
        reports.extend(plans);
    }
    (
        source,
        AuthoringFields {
            reports,
            expected_count: selected.len(),
        },
    )
}

pub(super) struct AuthoringFields {
    reports: Vec<SymbolPlan>,
    expected_count: usize,
}

impl AuthoringFields {
    pub(super) fn observe(
        self,
        saved: &str,
        path: &Path,
        expected: &str,
        operation: &str,
    ) -> Result<Value, CallToolResult> {
        let observed = (saved == expected).then(|| parse_sexp(saved).ok()).flatten()
            .and_then(|tree| {
                if self.reports.len() != self.expected_count {
                    return None;
                }
                self.reports.iter().map(|report| {
                    let node = tree.find_all("symbol").into_iter()
                        .find(|node| node.find_str("uuid") == Some(report.symbol_uuid.as_str()))?;
                    let fields = report.fields.iter().map(|field| {
                        let position = property(node, field.name).and_then(Position::read);
                        if field.proposed.as_ref().or(field.old.as_ref()) != position.as_ref() {
                            return None;
                        }
                        let mut observed = json!({
                            "name": field.name,
                            "reason": if field.proposed.is_some() { "placed" } else { field.reason },
                            "position": position,
                        });
                        if let Some(suggestion) = &field.suggestion {
                            observed["suggestion"] = suggestion.clone();
                        }
                        Some(observed)
                    }).collect::<Option<Vec<_>>>()?;
                    Some(json!({
                        "symbol_uuid": report.symbol_uuid, "reference": report.reference,
                        "unit": report.unit, "fields": fields,
                    }))
                }).collect::<Option<Vec<_>>>().map(Value::Array)
            });
        observed.ok_or_else(|| {
            mutation_outcome_uncertain(
                path,
                operation,
                "Could not verify the saved field placements.",
            )
        })
    }
}

fn bind_approvals(plans: &mut [SymbolPlan], revision: &str, scope: &Value) {
    for symbol in plans {
        for field in &mut symbol.fields {
            if let Some(position) = &field.proposed {
                field.approval = Some(
                    DocumentRevision::of(
                        &json!([revision, scope, symbol.symbol_uuid, field.name, position])
                            .to_string(),
                    )
                    .to_string(),
                );
            }
        }
    }
}

type FieldEdit = (String, &'static str, Position);

fn approved_edits(
    plans: &[SymbolPlan],
    approvals: Vec<String>,
) -> Result<Vec<FieldEdit>, CallToolResult> {
    let mut seen = BTreeSet::new();
    approvals
        .into_iter()
        .map(|id| {
            if !seen.insert(id.clone()) {
                return Err(invalid_arg("approvals", "duplicate placement approval"));
            }
            plans
                .iter()
                .find_map(|symbol| {
                    let field = symbol
                        .fields
                        .iter()
                        .find(|f| f.approval.as_deref() == Some(&id))?;
                    Some((
                        symbol.symbol_uuid.clone(),
                        field.name,
                        field.proposed.clone()?,
                    ))
                })
                .ok_or_else(|| {
                    invalid_arg(
                        "approvals",
                        "unknown placement; preview again and approve only returned IDs",
                    )
                })
        })
        .collect()
}

fn requested_approvals(
    args: &Value,
    revision: &str,
    path: &Path,
) -> Result<Option<Vec<String>>, CallToolResult> {
    let approvals = opt_str_list(args, "approvals")?;
    if approvals.is_some() {
        let Some(expected) = args["revision"].as_str() else {
            return Err(invalid_arg(
                "revision",
                "apply requires the revision from the preview",
            ));
        };
        if revision != expected {
            return Err(field_preview_conflict(path));
        }
    } else if !args["revision"].is_null() {
        return Err(invalid_arg(
            "revision",
            "pass a revision only with approvals",
        ));
    }
    Ok(approvals)
}

pub(super) fn field_preview_conflict(path: &Path) -> CallToolResult {
    let shown = copperpour_sexp::paths::display_path(path);
    CallToolResult::error_kind(
        ToolErrorKind::Conflict {
            paths: vec![shown.clone()],
        },
        format!("The schematic '{shown}' changed. Nothing was written. Reload it and preview again before approving."),
    )
}

pub(super) enum RotationFields {
    Preview(CallToolResult),
    Apply(String, RotationReadback),
}

pub(super) struct RotationReadback(Vec<FieldEdit>, Vec<Value>);

impl RotationReadback {
    /// The saved positions of the approved fields and the withdrawn offers.
    pub(super) fn observe(
        self,
        path: &Path,
        saved: &str,
        candidate: &str,
    ) -> Result<(Vec<Value>, Vec<Value>), CallToolResult> {
        (saved == candidate)
            .then(|| parse_sexp(saved).ok())
            .flatten()
            .and_then(|tree| verified_positions(&tree, &self.0))
            .map(|positions| (positions, self.1))
            .ok_or_else(|| {
                mutation_outcome_uncertain(
                    path,
                    ROTATE_OPERATION,
                    "Could not verify the saved turn and field placements.",
                )
            })
    }
}

pub(super) fn rotation_fields(
    args: &Value,
    before_source: &str,
    authorization: PreviewAuthorization,
    source: String,
    selected: &BTreeSet<String>,
    rotation: f64,
) -> anyhow::Result<Result<RotationFields, CallToolResult>> {
    let PreviewAuthorization {
        revision,
        approvals,
    } = authorization;
    let tree = parse_sexp(&source)?;
    let before = parse_sexp(before_source)?;
    if !saved_angles_are_measurable(&before) {
        return Ok(Err(plan_blocked(
            ROTATE_OPERATION,
            "Could not measure the sheet using quarter-turn symbol angles. Correct or resave the symbol orientations in KiCad before previewing the turn.",
        )));
    }
    let baseline = LayoutBaseline::new(&before);
    let carried_verdict = baseline.verdict(&tree);
    if carried_verdict == "blocked" {
        return Ok(Err(plan_blocked(
            ROTATE_OPERATION,
            "Could not prove that the opt-in turn preserves the saved schematic's electrical semantics.",
        )));
    }
    let eligible = saved_field_evidence(&before, selected);
    let mut plans = options(
        &source,
        &tree,
        |uuid| selected.contains(uuid),
        Some(&eligible),
        true,
    );
    // JSON preserves the sign of a float zero. Equivalent turns must bind
    // the same token and field approvals, even for negative full turns.
    let rotation = rotation + 0.0;
    let scope = json!(["rotate", selected, rotation]);
    let turn = DocumentRevision::of(&json!([revision, &scope]).to_string()).to_string();
    bind_approvals(&mut plans, &revision, &scope);
    let Some(approvals) = approvals else {
        if !args["turn"].is_null() {
            return Ok(Err(invalid_arg(
                "turn",
                "pass a turn token only with approvals",
            )));
        }
        let orientations = |tree: &SexpNode| {
            tree.find_all("symbol")
                .into_iter()
                .filter_map(|node| {
                    let id = node.find_str("uuid")?;
                    let (_, _, rotation) = parse_at(node)?;
                    Some((
                        id.to_owned(),
                        json!({
                            "rotation_degrees": rotation, "mirror": node.find_str("mirror"),
                        }),
                    ))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let old = orientations(&before);
        let new = orientations(&tree);
        let turns: Vec<_> = plans
            .iter()
            .map(|plan| {
                json!({
                    "symbol_uuid": plan.symbol_uuid, "reference": plan.reference, "unit": plan.unit,
                    "old": old.get(&plan.symbol_uuid), "proposed": new.get(&plan.symbol_uuid),
                })
            })
            .collect();
        return Ok(Ok(RotationFields::Preview(CallToolResult::json(&json!({
            "source": "saved", "revision": revision, "turn": turn, "written": false,
            "rotation_degrees": rotation, "turns": turns, "symbols": plans,
            "layout_verdict_without_field_changes": carried_verdict,
        })))));
    };
    if args["turn"].as_str() != Some(turn.as_str()) {
        return Ok(Err(invalid_arg(
            "turn",
            "repeat the previewed turn and pass its token; nothing was written",
        )));
    }
    let edits = match approved_edits(&plans, approvals) {
        Ok(edits) => edits,
        Err(error) => return Ok(Err(error)),
    };
    let layout_refusal = || {
        plan_blocked(
            ROTATE_OPERATION,
            "The turn and approved placements do not meet the saved sheet's layout and electrical gate.",
        )
    };
    let Some(candidate) = approved_candidate(&source, &tree, &edits) else {
        return Ok(Err(layout_refusal()));
    };
    let after = parse_sexp(&candidate)?;
    if !matches!(baseline.verdict(&after), "improved" | "unchanged") {
        return Ok(Err(layout_refusal()));
    }
    if !field_collisions(&after).is_subset(&field_collisions(&before)) {
        return Ok(Err(plan_blocked(
            ROTATE_OPERATION,
            "The turn and approved placements introduce a new field collision.",
        )));
    }
    let changed = selected.iter().map(String::as_str).collect::<Vec<_>>();
    if !component_overlap_pairs(&after, &changed)
        .is_subset(&component_overlap_pairs(&before, &changed))
    {
        return Ok(Err(plan_blocked(
            ROTATE_OPERATION,
            "The turn introduces a new component overlap.",
        )));
    }
    let withdrawn = withdrawn_offers(&plans, &edits);
    Ok(Ok(RotationFields::Apply(
        candidate,
        RotationReadback(edits, withdrawn),
    )))
}

async fn handle_autoplace_schematic_fields(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let (
        source,
        PreviewAuthorization {
            revision,
            approvals,
        },
    ) = match read_preview_source(args, &path)? {
        Ok(input) => input,
        Err(error) => return Ok(error),
    };
    let tree = match parse_saved_sheet(&path, &source) {
        Ok(tree) => tree,
        Err(error) => return Ok(error),
    };
    let mut plans = options(&source, &tree, |_| true, None, true);
    bind_approvals(&mut plans, &revision, &json!(OPERATION));
    let Some(approvals) = approvals else {
        return Ok(CallToolResult::json(
            &json!({"source":"saved", "revision":revision, "written":false, "symbols":plans}),
        ));
    };
    let edits = match approved_edits(&plans, approvals) {
        Ok(edits) => edits,
        Err(error) => return Ok(error),
    };
    if edits.is_empty() {
        return Ok(CallToolResult::json(
            &json!({"source":"saved", "revision":revision, "written":false, "placements":[]}),
        ));
    }
    let Some(candidate) = approved_candidate(&source, &tree, &edits) else {
        return Ok(plan_blocked(
            OPERATION,
            "Approved placements are not clear together.",
        ));
    };
    match write_atomic_if_unchanged(&path, &source, &candidate) {
        Ok(()) => {}
        Err(SexpError::Conflict { path }) => return Ok(field_preview_conflict(&path)),
        Err(error) => return Err(error.into()),
    }
    let observed = read_consistent(&path).ok().and_then(|saved| {
        let positions = verified_positions(&parse_sexp(&saved).ok()?, &edits)?;
        if saved != candidate {
            return None;
        }
        Some((document_revision(&path, saved.as_bytes()).ok()?, positions))
    });
    let Some((revision, positions)) = observed else {
        return Ok(mutation_outcome_uncertain(
            &path,
            OPERATION,
            "Could not verify saved field placements.",
        ));
    };
    Ok(CallToolResult::json(&json!({
        "source": "saved", "revision": revision, "written": true, "placements": positions,
        "withdrawn_offers": withdrawn_offers(&plans, &edits),
    })))
}

fn document_revision(path: &Path, source: &[u8]) -> anyhow::Result<String> {
    let canonical = path.canonicalize()?;
    Ok(format!(
        "{}/{}",
        DocumentRevision::of(&canonical.to_string_lossy()),
        DocumentRevision::of_bytes(source)
    ))
}
