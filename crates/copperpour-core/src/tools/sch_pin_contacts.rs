//! Drawing-style checks that do not change electrical or overlap analysis.

use super::sch_connectivity::{pt_key, ConnectivityIndex, PlacedPin, COINCIDENT_TOLERANCE};
use super::{drawn_symbol_pins, get_path, invalid_arg, ToolDef, UNRESOLVED_PIN_GEOMETRY};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use copperpour_sexp::{
    geometry::{point_on_segment, points_coincident, round6},
    schematic::{extract_buses, extract_labels, extract_wires, parse_at, read_schematic, Wire},
    SexpNode,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const DEFAULT_MIN_WIRE_MM: f64 = 2.54;
type Point = (f64, f64);
const DIRECTIVE_TAGS: [&str; 2] = ["netclass_flag", "directive_label"];

fn anchor(node: &SexpNode) -> Option<Point> {
    let (x, y, _) = parse_at(node)?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn directives(tree: &SexpNode) -> Option<Vec<Directive<'_>>> {
    DIRECTIVE_TAGS
        .into_iter()
        .flat_map(|kind| tree.find_all(kind))
        .map(|node| {
            Some(Directive {
                at: anchor(node)?,
                uuid: node.find_str("uuid"),
            })
        })
        .collect()
}

struct Directive<'a> {
    at: Point,
    uuid: Option<&'a str>,
}

pub(super) fn definition() -> ToolDef {
    tool!(
        "find_pin_contacts",
        "Report coincident visible pins on distinct symbols and short, branch-free \
         pin-to-pin wire runs on one saved schematic sheet. These are drawing-style \
         findings, not overlap or electrical errors. Does not write or need KiCad.",
        json!({
            "type": "object",
            "properties": {
                "schematic": {"type": "string", "description": "Path to a saved .kicad_sch sheet"},
                "min_wire_mm": {
                    "type": ["number", "null"], "minimum": 0.0, "default": DEFAULT_MIN_WIRE_MM,
                    "description": "Minimum wire run length in mm, compared at six decimal places of measured length. Zero reports contacts only."
                }
            },
            "required": ["schematic"]
        }),
        |args, _ctx| async move { handle_find_pin_contacts(args) }
    )
}

fn handle_find_pin_contacts(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let min_wire_mm = match args["min_wire_mm"].as_f64() {
        Some(n) if n >= 0.0 && n.is_finite() => n,
        None if args["min_wire_mm"].is_null() => DEFAULT_MIN_WIRE_MM,
        _ => {
            return Ok(invalid_arg(
                "min_wire_mm",
                "expected a nonnegative number in mm",
            ))
        }
    };
    let (_, tree) = read_schematic(&path)?;
    let malformed = |reason: &str| {
        CallToolResult::error_kind(
            ToolErrorKind::MalformedFile {
                path: path.display().to_string(),
                reason: reason.to_owned(),
            },
            format!("Cannot check {}: {reason}", path.display()),
        )
    };
    if tree.head() != Some("kicad_sch") {
        return Ok(malformed("expected a kicad_sch document"));
    }
    let wires = extract_wires(&tree);
    let (contacts, short_wires) = match pin_contact_findings(&tree, &wires, min_wire_mm) {
        Ok(findings) => findings,
        Err(reason) => return Ok(malformed(reason)),
    };
    Ok(CallToolResult::json(&json!({
        "schematic": path,
        "contact_count": contacts.len(),
        "contacts": contacts,
        "short_wire_count": short_wires.len(),
        "short_wires": short_wires,
    })))
}

pub(super) fn pin_contact_findings(
    tree: &SexpNode,
    wires: &[Wire],
    min_wire_mm: f64,
) -> Result<(Vec<Value>, Vec<Value>), &'static str> {
    let pins = drawn_symbol_pins(tree).ok_or(UNRESOLVED_PIN_GEOMETRY)?;
    if wires.len() != tree.find_all("wire").len()
        || wires
            .iter()
            .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
    {
        return Err("one or more wire segments have unreadable endpoints");
    }
    if ["label", "global_label", "hierarchical_label"]
        .into_iter()
        .flat_map(|kind| tree.find_all(kind))
        .any(|label| anchor(label).is_none())
    {
        return Err("one or more labels have unreadable anchors");
    }
    let Some(directives) = directives(tree) else {
        return Err("one or more directive labels have unreadable anchors");
    };
    let labels = extract_labels(tree);
    let buses = extract_buses(tree);
    let index = ConnectivityIndex::with_placed_pins(
        tree,
        wires,
        &buses,
        &labels,
        COINCIDENT_TOLERANCE,
        pins,
    );
    Ok((
        pin_contacts(&index, wires, &directives, false),
        short_pin_wires(&index, wires, &directives, min_wire_mm),
    ))
}

pub(super) fn default_contact_counts(tree: &SexpNode, wires: &[Wire]) -> Option<(usize, usize)> {
    pin_contact_findings(tree, wires, DEFAULT_MIN_WIRE_MM)
        .ok()
        .map(|(contacts, short_wires)| (contacts.len(), short_wires.len()))
}

pub(super) fn clearance_pin_contacts(tree: &SexpNode, wires: &[Wire]) -> (Vec<Value>, Vec<String>) {
    let (pins, unresolved) = clearance_pin_inventory(tree);
    let labels = extract_labels(tree);
    let buses = extract_buses(tree);
    let index = ConnectivityIndex::with_placed_pins(
        tree,
        wires,
        &buses,
        &labels,
        COINCIDENT_TOLERANCE,
        pins,
    );
    // Directives do not affect whether two pin tips coincide.
    (pin_contacts(&index, wires, &[], true), unresolved)
}

pub(super) fn clearance_pin_inventory(
    tree: &SexpNode,
) -> (super::PlacedPinsByReference, Vec<String>) {
    let mut pins = Vec::new();
    let mut unresolved = Vec::new();
    for (node, placed) in tree
        .find_all("symbol")
        .into_iter()
        .zip(super::drawn_symbol_pin_inventory(tree))
    {
        match placed {
            Some(placed) => pins.push(placed),
            None => unresolved.push(
                node.find_all("property")
                    .into_iter()
                    .find(|property| {
                        property.get(1).and_then(SexpNode::as_str) == Some("Reference")
                    })
                    .and_then(|property| property.get(2).and_then(SexpNode::as_str))
                    .filter(|reference| !reference.is_empty())
                    .unwrap_or("<unknown symbol>")
                    .to_owned(),
            ),
        }
    }
    (pins, unresolved)
}

fn near(a: Point, b: Point) -> bool {
    points_coincident(a.0, a.1, b.0, b.1, COINCIDENT_TOLERANCE)
}

fn point(at: Point) -> Value {
    json!({"x": round6(at.0), "y": round6(at.1)})
}

fn pin_info(pin: &PlacedPin) -> Value {
    json!({
        "reference": pin.reference,
        "unit": pin.unit,
        "pin_number": pin.pin.number,
        "symbol_uuid": pin.symbol_uuid,
        "is_power_symbol": pin.is_power_symbol,
    })
}

fn visible_pins_at(index: &ConnectivityIndex<'_>, at: Point) -> Vec<usize> {
    index
        .pin_indices_at(at.0, at.1)
        .into_iter()
        .filter(|&i| !index.placed_pins()[i].pin.hidden)
        .collect()
}

fn spans_symbols(pins: &[PlacedPin], a: &[usize], b: &[usize]) -> bool {
    let (Some(&first), Some(_)) = (a.first(), b.first()) else {
        return false;
    };
    a.iter()
        .chain(b)
        .any(|&i| pins[i].symbol_index != pins[first].symbol_index)
}

fn pin_contacts(
    index: &ConnectivityIndex<'_>,
    wires: &[Wire],
    directives: &[Directive<'_>],
    stable_point: bool,
) -> Vec<Value> {
    let pins = index.placed_pins();
    let neighborhoods: Vec<_> = pins
        .iter()
        .map(|pin| {
            if pin.pin.hidden {
                Vec::new()
            } else {
                visible_pins_at(index, pin.at)
            }
        })
        .collect();
    let mut seen = HashSet::new();
    let mut contacts = Vec::new();
    for (pin, matches) in pins.iter().zip(&neighborhoods) {
        let at = pin.at;
        // A superset's center must itself be within reach of this center.
        let strict_subset = matches.iter().any(|&i| {
            let other = &neighborhoods[i];
            other.len() > matches.len()
                && matches.iter().all(|pin| other.binary_search(pin).is_ok())
        });
        if !spans_symbols(pins, matches, matches) || strict_subset || !seen.insert(matches) {
            continue;
        }
        // Only a pin with this exact neighborhood can serve as its center.
        let at = if stable_point {
            matches
                .iter()
                .filter(|&&i| &neighborhoods[i] == matches)
                .map(|&i| pins[i].at)
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)))
                .unwrap_or(at)
        } else {
            at
        };
        let detail = index.point_detail(at.0, at.1);
        let labels = index.labels().iter().filter(|label| near(at, (label.x, label.y))).map(|label| json!({
            "uuid": label.uuid, "net": label.net, "kind": format!("{:?}", label.kind),
            "point": point((label.x, label.y))
        })).chain(directives.iter().filter(|directive| near(at, directive.at)).map(|directive| json!({
            "uuid": directive.uuid, "net": null, "kind": "DirectiveLabel", "point": point(directive.at)
        }))).collect::<Vec<_>>();
        let contact = json!({
            "kind": "pin_contact",
            "point": point(at),
            "pins": matches.iter().map(|&i| {
                let pin = &pins[i];
                let mut info = pin_info(pin);
                if stable_point {
                    // Pin numbers can repeat at different locations in one
                    // unit. The library-local tip survives a placement move.
                    info["_local_tip"] = json!([pin.pin.local_x, pin.pin.local_y]);
                }
                info
            }).collect::<Vec<_>>(),
            "wires": wires.iter().filter(|wire| touches(at, wire)).map(|wire| json!({
                "uuid": wire.uuid, "start": point((wire.x1, wire.y1)), "end": point((wire.x2, wire.y2))
            })).collect::<Vec<_>>(),
            "junctions": detail.junctions.into_iter().map(point).collect::<Vec<_>>(),
            "labels": labels,
        });
        contacts.push(contact);
    }
    contacts
}

fn diagonal(wire: &Wire) -> bool {
    (wire.x2 - wire.x1).abs() >= COINCIDENT_TOLERANCE
        && (wire.y2 - wire.y1).abs() >= COINCIDENT_TOLERANCE
}

pub(super) fn touches(at: Point, wire: &Wire) -> bool {
    if diagonal(wire) {
        // The shared axis-aligned wire index does not cover diagonals.
        let (dx, dy) = (wire.x2 - wire.x1, wire.y2 - wire.y1);
        let t = ((at.0 - wire.x1) * dx + (at.1 - wire.y1) * dy) / (dx * dx + dy * dy);
        let t = t.clamp(0.0, 1.0);
        near(at, (wire.x1 + t * dx, wire.y1 + t * dy))
    } else {
        point_on_segment(
            at.0,
            at.1,
            wire.x1,
            wire.y1,
            wire.x2,
            wire.y2,
            COINCIDENT_TOLERANCE,
        )
    }
}

pub(super) fn touches_interior(at: Point, wire: &Wire) -> bool {
    let key = pt_key(at.0, at.1);
    if key == pt_key(wire.x1, wire.y1) || key == pt_key(wire.x2, wire.y2) {
        return false;
    }
    let (dx, dy) = (wire.x2 - wire.x1, wire.y2 - wire.y1);
    let along = (at.0 - wire.x1) * dx + (at.1 - wire.y1) * dy;
    along > 0.0 && along < dx * dx + dy * dy && touches(at, wire)
}

pub(super) struct WireChain {
    pub(super) ends: [Point; 2],
    pub(super) indices: Vec<usize>,
}

pub(super) fn wire_chains(
    index: &ConnectivityIndex<'_>,
    wires: &[Wire],
    additional_attachment: impl Fn(Point) -> bool,
) -> Vec<WireChain> {
    let ends: Vec<_> = wires
        .iter()
        .flat_map(|w| [(w.x1, w.y1), (w.x2, w.y2)])
        .collect();
    let diagonals: Vec<_> = wires.iter().filter(|wire| diagonal(wire)).collect();
    let mut joints = HashMap::<_, Vec<usize>>::new();
    for (end, &(x, y)) in ends.iter().enumerate() {
        joints.entry(pt_key(x, y)).or_default().push(end);
    }
    let mut neighbors = vec![None; ends.len()];
    for ends_at_joint in joints.values() {
        let &[a, b] = ends_at_joint.as_slice() else {
            continue;
        };
        if a / 2 != b / 2
            && [ends[a], ends[b]].into_iter().all(|at| {
                !index.wire_run_joint_has_attachment(at.0, at.1)
                    && !additional_attachment(at)
                    && !diagonals.iter().any(|wire| touches_interior(at, wire))
            })
        {
            neighbors[a] = Some(b);
            neighbors[b] = Some(a);
        }
    }
    let mut chains = Vec::new();
    for (start, &at) in ends.iter().enumerate() {
        if neighbors[start].is_some() {
            continue;
        }
        let mut run = Vec::new();
        let mut end = start;
        loop {
            run.push(end / 2);
            let other_end = end ^ 1;
            match neighbors[other_end] {
                Some(next) => end = next,
                None => {
                    end = other_end;
                    break;
                }
            }
        }
        if end > start {
            chains.push(WireChain {
                ends: [at, ends[end]],
                indices: run,
            });
        }
    }
    chains
}

fn short_pin_wires(
    index: &ConnectivityIndex<'_>,
    wires: &[Wire],
    directives: &[Directive<'_>],
    min_wire_mm: f64,
) -> Vec<Value> {
    if min_wire_mm == 0.0 {
        return Vec::new();
    }
    let pins = index.placed_pins();
    let terminals_at = |at: Point| {
        let mut terminals: Vec<_> = index.wire_run_pins_at(at.0, at.1).collect();
        terminals.sort_unstable();
        terminals
    };
    let mut short = Vec::new();
    for chain in wire_chains(index, wires, |at| {
        directives.iter().any(|directive| near(at, directive.at))
    }) {
        let [at, end] = chain.ends;
        let start_pins = terminals_at(at);
        let end_pins = terminals_at(end);
        if !spans_symbols(pins, &start_pins, &end_pins) {
            continue;
        }
        let run = chain
            .indices
            .into_iter()
            .map(|i| &wires[i])
            .collect::<Vec<_>>();
        let length_mm: f64 = run
            .iter()
            .map(|wire| (wire.x2 - wire.x1).hypot(wire.y2 - wire.y1))
            .sum();
        // Use KiCad's saved precision so a 2.54 mm wire does not round below the minimum.
        let length_mm = round6(length_mm);
        if length_mm >= min_wire_mm {
            continue;
        }
        if !run.iter().any(|wire| {
            index.junctions().any(|at| touches_interior(at, wire))
                || directives
                    .iter()
                    .any(|directive| touches_interior(directive.at, wire))
                || index
                    .labels()
                    .iter()
                    .any(|label| touches_interior((label.x, label.y), wire))
        }) {
            let wire_uuids: Vec<_> = run.into_iter().map(|wire| wire.uuid.as_deref()).collect();
            short.push(json!({
                "kind": "short_pin_wire",
                "length_mm": length_mm,
                "wire_uuids": wire_uuids,
                "start": {"point": point(at), "pins": start_pins.iter().map(|&i| pin_info(&pins[i])).collect::<Vec<_>>()},
                "end": {"point": point(end), "pins": end_pins.iter().map(|&i| pin_info(&pins[i])).collect::<Vec<_>>()},
            }));
        }
    }
    short
}

#[cfg(test)]
#[path = "pin_contact_tests.rs"]
mod tests;
