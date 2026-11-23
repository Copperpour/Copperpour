use super::inventory::{Inventory, Part, Point};
use super::poses::{set_at, span, Pose, MIN_WIRE};
use super::topology::{coincides, endpoints, on_wire, Terminal, Topology};
use crate::tools::sch_analysis::{overlap_findings, OVERLAP_TOLERANCE};
use crate::tools::sch_wiring::{insert_wire_with_junctions, route_between, RouteGuard};
use copperpour_schematic_editor::sexp::{writer, SexpNode as Editable};
use copperpour_sexp::schematic::{extract_junctions, extract_wires, parse_at};
use copperpour_sexp::{parse_sexp, SexpNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub(super) struct Changes<'a> {
    pub poses: BTreeMap<&'a str, Pose>,
    pub removed: BTreeSet<&'a str>,
    pub notes: BTreeMap<&'a str, Point>,
    pub redraw: BTreeSet<(i64, i64)>,
    /// Attachments explicitly grouped into one visible rail. Keys are the
    /// retained drawn island; its member islands must already share a net.
    pub groups: BTreeMap<(i64, i64), BTreeSet<(i64, i64)>>,
    pub rail_routes: BTreeMap<(i64, i64), Vec<[Point; 2]>>,
    pub rail_taps: BTreeMap<(i64, i64), Point>,
}

pub(super) fn to_source(node: &SexpNode) -> String {
    fn editable(node: &SexpNode) -> Editable {
        match node {
            SexpNode::Atom(a) => Editable::Atom(a.clone()),
            SexpNode::Str(s) => Editable::Str(s.clone()),
            SexpNode::List(children) => Editable::List(children.iter().map(editable).collect()),
        }
    }
    writer::write_with_indent(&editable(node), writer::KICAD_INDENT)
}

pub(super) fn build(
    tree: &SexpNode,
    inventory: &Inventory<'_>,
    topology: &Topology<'_, '_>,
    changes: &Changes<'_>,
) -> Result<SexpNode, String> {
    let mut affected = changes.redraw.clone();
    for (id, p) in &inventory.parts {
        if changes.poses.contains_key(id) || changes.removed.contains(id) {
            affected.extend(p.pins.iter().map(|p| p.drawn));
        }
    }
    for (retained, grouped) in &changes.groups {
        if !changes.rail_taps.contains_key(retained) {
            affected.insert(*retained);
        }
        affected.extend(grouped);
    }
    let mut removed_wires = BTreeSet::new();
    let mut removed_junctions = Vec::new();
    let mut routes: BTreeMap<_, Vec<(Point, Point, bool)>> = BTreeMap::new();
    let mut marker_moves = Vec::new();
    for id in affected {
        let island = &topology.islands[&id];
        if ["netclass_flag", "directive_label"]
            .into_iter()
            .flat_map(|tag| tree.find_all(tag))
            .any(|item| {
                parse_at(item).is_none_or(|(x, y, _)| {
                    let at = (x, y);
                    island.wires.iter().any(|wire| on_wire(at, endpoints(wire)))
                        || island.terminals.iter().any(|t| coincides(at, t.tip()))
                })
            })
        {
            return Err("cannot redraw a wire island with a net-class directive".into());
        }
        removed_wires.extend(island.wires.iter().filter_map(|w| w.uuid.as_deref()));
        removed_junctions.extend(&island.junctions);
        let group = changes
            .groups
            .iter()
            .find(|(_, members)| members.contains(&id))
            .map_or(id, |(&retained, _)| retained);
        let points = routes.entry(group).or_default();
        for end in &island.terminals {
            let tip = match end {
                Terminal::Pin(part, _) if changes.removed.contains(part.id.uuid) => continue,
                Terminal::Pin(part, pin) => changes
                    .poses
                    .get(part.id.uuid)
                    .map_or(pin.tip, |pose| pose.tip(part, pin)),
                Terminal::Label(_) => end.tip(),
            };
            if !coincides(end.tip(), tip)
                && topology
                    .no_connects
                    .iter()
                    .any(|p| coincides(*p, end.tip()))
            {
                if island.terminals.len() != 1 || !island.wires.is_empty() {
                    return Err("a moved pin has a no-connect marker on a drawn net".into());
                }
                marker_moves.push((end.tip(), tip));
            }
            let is_pin = matches!(end, Terminal::Pin(..));
            if let Some((_, original, has_pin)) = points
                .iter_mut()
                .find(|(target, _, _)| coincides(*target, tip))
            {
                // A label already anchored on a pin does not create another
                // route endpoint. Do not allow a move to create a new contact.
                if !coincides(*original, end.tip()) || (*has_pin && is_pin) {
                    return Err("two terminals would directly touch after placement".into());
                }
                *has_pin |= is_pin;
                continue;
            }
            points.push((tip, end.tip(), is_pin));
        }
        if island.wires.iter().any(|wire| {
            endpoints(wire).into_iter().any(|tip| {
                !island.terminals.iter().any(|t| coincides(t.tip(), tip))
                    && !island
                        .wires
                        .iter()
                        .any(|other| !std::ptr::eq(*wire, *other) && on_wire(tip, endpoints(other)))
            })
        }) {
            return Err("a wire island has a dangling end without an attachment".into());
        }
    }

    let mut candidate = tree.clone();
    let SexpNode::List(children) = &mut candidate else {
        return Err("expected a schematic tree".into());
    };
    children.retain(|item| {
        let id = item.find_str("uuid").unwrap_or_default();
        match item.head() {
            Some("symbol") => !changes.removed.contains(id),
            Some("wire") => !removed_wires.contains(id),
            Some("junction") => !parse_at(item)
                .is_some_and(|(x, y, _)| removed_junctions.iter().any(|p| coincides(*p, (x, y)))),
            _ => true,
        }
    });
    for item in children.iter_mut() {
        let id = item.find_str("uuid").unwrap_or_default().to_owned();
        match item.head() {
            Some("symbol") => {
                if let Some(pose) = changes.poses.get(id.as_str()) {
                    *item = pose.relocate(&inventory.parts[id.as_str()])?;
                }
            }
            Some("text") => {
                if let Some(&(x, y)) = changes.notes.get(id.as_str()) {
                    let (_, _, angle) = parse_at(item).ok_or("invalid note position")?;
                    set_at(item, (x, y, angle)).ok_or("invalid note position")?;
                }
            }
            Some("no_connect") => {
                if let Some((x, y, angle)) = parse_at(item) {
                    if let Some((_, (x, y))) =
                        marker_moves.iter().find(|(p, _)| coincides(*p, (x, y)))
                    {
                        set_at(item, (*x, *y, angle)).ok_or("invalid no-connect position")?;
                    }
                }
            }
            _ => {}
        }
    }
    let mut content = to_source(&candidate);
    for (group, terminals) in routes {
        let mut points: Vec<_> = terminals.into_iter().map(|(tip, _, _)| tip).collect();
        if let Some(segments) = changes.rail_routes.get(&group) {
            if segments.iter().any(|[a, b]| span(*a, *b) + 1e-6 < MIN_WIRE) {
                return Err("rail requires a wire shorter than 2.54 mm".into());
            }
            let (start, ends) = points
                .split_first()
                .ok_or("rail has no surviving terminals")?;
            let unrouted = parse_sexp(&content).map_err(|e| e.to_string())?;
            let guard = RouteGuard::new(&unrouted, *start, ends)
                .ok_or("cannot resolve pin geometry before routing the local rail")?;
            for &[(x1, y1), (x2, y2)] in segments {
                content = insert_wire_with_junctions(content, x1, y1, x2, y2);
            }
            if let Some(reason) = guard
                .refusal("local rail route", &content)
                .map_err(|e| e.to_string())?
            {
                return Err(reason);
            }
            continue;
        }
        let Some(seed) = changes
            .rail_taps
            .get(&group)
            .copied()
            .or_else(|| points.pop())
        else {
            continue;
        };
        let mut joined = vec![seed];
        while !points.is_empty() {
            let mut options: Vec<_> = points
                .iter()
                .enumerate()
                .flat_map(|(i, &from)| joined.iter().map(move |&to| (span(from, to), i, from, to)))
                .collect();
            options.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut selected = None;
            let mut blocker = "no drawn connection between moved terminals".to_owned();
            for (_, i, from, to) in options {
                match route_between(&content, from.0, from.1, to.0, to.1)
                    .map_err(|e| e.to_string())?
                {
                    Ok(route)
                        if route
                            .path
                            .windows(2)
                            .all(|p| span(p[0], p[1]) + 1e-6 >= MIN_WIRE) =>
                    {
                        selected = Some((i, route.content));
                        break;
                    }
                    Ok(_) => blocker = "route would need a segment shorter than 2.54 mm".into(),
                    Err(refusal) => blocker = refusal.reasons().join("; "),
                }
            }
            let (i, routed) = selected.ok_or(blocker)?;
            content = routed;
            joined.push(points.remove(i));
        }
    }
    parse_sexp(&content).map_err(|e| e.to_string())
}

pub(super) fn clear_visible_fields(
    tree: &mut SexpNode,
    movable: &BTreeSet<&str>,
    anchor: &Part<'_>,
) {
    fn fields_of(finding: &Value) -> impl Iterator<Item = (&str, &str)> {
        ["a", "b"].into_iter().filter_map(|side| {
            let owner = &finding[side];
            Some((owner["symbol_uuid"].as_str()?, owner["field"].as_str()?))
        })
    }
    let wires = extract_wires(tree);
    let findings = |tree: &SexpNode| overlap_findings(tree, &wires, OVERLAP_TOLERANCE).0;
    let mut collisions = findings(tree);
    let mut pending: BTreeSet<_> = collisions
        .iter()
        .flat_map(fields_of)
        .filter(|(uuid, name)| movable.contains(uuid) && matches!(*name, "Reference" | "Value"))
        .map(|(uuid, name)| (uuid.to_owned(), name.to_owned()))
        .collect();
    let compact = |name: &str| {
        anchor
            .node
            .find_all("property")
            .into_iter()
            .find(|f| f.get(1).and_then(SexpNode::as_str) == Some(name))
            .and_then(parse_at)
            .is_some_and(|(x, y, _)| {
                (y - anchor.inst.y).abs().max((x - anchor.inst.x).abs()) > 4.0 * MIN_WIRE
            })
    };
    if movable.contains(anchor.id.uuid) && ["Reference", "Value"].into_iter().any(compact) {
        pending.extend(
            ["Reference", "Value"].map(|name| (anchor.id.uuid.to_owned(), name.to_owned())),
        );
    }
    for (uuid, name) in pending {
        let compact_anchor = uuid == anchor.id.uuid && compact(&name);
        if !compact_anchor
            && !collisions
                .iter()
                .flat_map(fields_of)
                .any(|(a, b)| a == uuid && b == name)
        {
            continue;
        }
        let Some((symbol, field, (x, y, angle))) = tree
            .children()
            .into_iter()
            .flatten()
            .enumerate()
            .find(|(_, s)| s.find_str("uuid") == Some(&uuid))
            .and_then(|(i, s)| {
                s.children()?
                    .iter()
                    .enumerate()
                    .find(|(_, f)| {
                        f.head() == Some("property")
                            && f.get(1).and_then(SexpNode::as_str) == Some(&name)
                    })
                    .and_then(|(j, f)| Some((i, j, parse_at(f)?)))
            })
        else {
            continue;
        };
        let initial = tree.clone();
        for offset in 1..=5 {
            let mm = f64::from(offset) * super::poses::GRID;
            let mut found = false;
            for (dx, dy) in [
                (mm, 0.0),
                (-mm, 0.0),
                (0.0, mm),
                (0.0, -mm),
                (mm, mm),
                (-mm, mm),
                (mm, -mm),
                (-mm, -mm),
            ] {
                if compact_anchor
                    && (y + dy - anchor.inst.y)
                        .abs()
                        .max((x + dx - anchor.inst.x).abs())
                        >= (y - anchor.inst.y).abs().max((x - anchor.inst.x).abs())
                {
                    continue;
                }
                let mut candidate = initial.clone();
                let SexpNode::List(items) = &mut candidate else {
                    break;
                };
                let SexpNode::List(fields) = &mut items[symbol] else {
                    break;
                };
                if set_at(&mut fields[field], (x + dx, y + dy, angle)).is_none() {
                    continue;
                }
                fields.retain(|n| n.head() != Some("fields_autoplaced"));
                let result = findings(&candidate);
                if (result.len() < collisions.len()
                    || (compact_anchor && result.len() == collisions.len()))
                    && !result
                        .iter()
                        .flat_map(fields_of)
                        .any(|(a, b)| a == uuid && b == name)
                {
                    *tree = candidate;
                    collisions = result;
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
    }
}

/// Each item is read from the checked candidate, not echoed from the search request.
/// Wire and junction additions are described by geometry, so random intermediate IDs
/// from the router cannot make the preview nondeterministic.
pub(super) fn diff(before: &SexpNode, after: &SexpNode) -> Value {
    let index = |tree: &SexpNode| -> BTreeMap<String, SexpNode> {
        tree.children()
            .into_iter()
            .flatten()
            .filter_map(|n| Some((n.find_str("uuid")?.to_owned(), n.clone())))
            .collect()
    };
    let was = index(before);
    let now = index(after);
    let added_wires: Vec<_> = extract_wires(after)
        .iter()
        .filter(|wire| {
            wire.uuid
                .as_ref()
                .is_some_and(|uuid| !was.contains_key(uuid))
        })
        .map(endpoints)
        .collect();
    let old_dots = extract_junctions(before);
    let new_dots = extract_junctions(after);
    let mut removed_wires = Vec::new();
    let mut removed_power = Vec::new();
    let mut removed_junctions = Vec::new();
    let mut poses = Vec::new();
    let mut fields = Vec::new();
    let mut notes = Vec::new();
    let mut no_connects = Vec::new();
    for (id, old) in &was {
        let new = now.get(id);
        match (old.head(), new) {
            (Some("wire"), None) => removed_wires.push(id),
            (Some("junction"), None)
                if parse_at(old).is_some_and(|(x, y, _)| !new_dots.contains(&(x, y))) =>
            {
                removed_junctions.push(id);
            }
            (Some("symbol"), None) => removed_power.push(id),
            (Some("symbol"), Some(new)) => {
                let unit = new.find_f64("unit").map_or(1, |v| v as u32);
                if let Some((x, y, rotation)) =
                    parse_at(new).filter(|at| Some(*at) != parse_at(old))
                {
                    poses.push(
                        json!({"uuid": id, "unit": unit, "x": x, "y": y, "rotation": rotation}),
                    );
                }
                let old_fields = old.find_all("property");
                for f in new.find_all("property") {
                    let key = f.get(1).and_then(SexpNode::as_str);
                    let old_field = old_fields
                        .iter()
                        .find(|f| f.get(1).and_then(SexpNode::as_str) == key);
                    if let Some((x, y, rotation)) =
                        parse_at(f).filter(|at| Some(*at) != old_field.and_then(|f| parse_at(f)))
                    {
                        fields.push(json!({"uuid": id, "unit": unit, "field": key, "x": x, "y": y, "rotation": rotation}));
                    }
                }
            }
            (Some("text" | "no_connect"), Some(new)) => {
                if let Some((x, y, _)) = parse_at(new).filter(|at| Some(*at) != parse_at(old)) {
                    let observed = json!({"uuid": id, "x": x, "y": y});
                    if old.head() == Some("text") {
                        notes.push(observed);
                    } else {
                        no_connects.push(observed);
                    }
                }
            }
            _ => {}
        }
    }
    json!({
        "poses": poses, "field_moves": fields, "note_moves": notes, "no_connect_moves": no_connects,
        "remove_wire_uuids": removed_wires, "add_wires": added_wires,
        "remove_junction_uuids": removed_junctions,
        "add_junctions": new_dots.into_iter().filter(|p| !old_dots.contains(p)).collect::<Vec<_>>(),
        "removed_power_symbol_uuids": removed_power
    })
}
