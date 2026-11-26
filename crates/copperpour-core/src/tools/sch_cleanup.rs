//! Plan one saved sheet's cleanup without saving the candidate.

use super::sch_analysis::added_clearance_findings;
use super::sch_block_move::Region;
use super::sch_connectivity::{
    exact_drawn_net_graph, schematic_pin_endpoint, schematic_point, schematic_point_on_wire,
    ConnectivityIndex, PlacedPin, COINCIDENT_TOLERANCE,
};
use super::sch_layout_score::{self, collinear_overlap, Score};
use super::sch_pin_contacts::{touches, touches_interior, wire_chains, WireChain};
use super::sch_wiring::{
    apply_no_connect_carries, library_power_clearance_reason, ordered_route_paths,
    plan_drawn_no_connect_carry, route_paths, select_power_symbol_rotation, RouteGuard,
};
use super::{
    drawn_symbol_pins, get_path, invalid_arg, opt_str_list, require_f64, stub_direction, ToolDef,
    UNRESOLVED_PIN_GEOMETRY,
};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use copperpour_schematic_editor as cse;
use copperpour_sexp::geometry::{points_coincident, round6};
use copperpour_sexp::schematic::{
    extract_all_net_labels, extract_buses, extract_sheet_pins, extract_wires, parse_at,
    pin_outward_direction, Label, LabelKind, SymbolInstance, Wire,
};
use copperpour_sexp::writer::find_direct_child_blocks;
use copperpour_sexp::{apply_edits, parse_sexp, DocumentRevision, SexpEdit, SexpNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "sch_cleanup_tests.rs"]
mod tests;

type Point = (f64, f64);
const GRID: f64 = 2.54;
const MAX_CLEANUP_STEPS: usize = 256;

pub(super) fn definition() -> ToolDef {
    tool!(
        "plan_schematic_cleanup",
        "Preview a net-preserving cleanup of a region on one saved schematic. Only \
         listed symbols may move, plus power symbols and no-connects carried on \
         their pins. Does not write or need KiCad.",
        json!({
            "type": "object",
            "properties": {
                "schematic": {"type": "string", "description": "Path to a saved .kicad_sch sheet"},
                "x1": {"type": "number", "description": "First region corner, x in mm"},
                "y1": {"type": "number", "description": "First region corner, y in mm"},
                "x2": {"type": "number", "description": "Opposite region corner, x in mm"},
                "y2": {"type": "number", "description": "Opposite region corner, y in mm"},
                "movable": {
                    "type": ["array", "null"], "items": {"type": "string"},
                    "default": [],
                    "description": "References permitted to move or, for redundant power symbols, be removed"
                }
            },
            "required": ["schematic", "x1", "y1", "x2", "y2"]
        }),
        |args: &Value, _ctx| {
            let args = args.clone();
            async move {
                tokio::task::spawn_blocking(move || handle_plan_schematic_cleanup(&args)).await?
            }
        }
    )
}

fn handle_plan_schematic_cleanup(args: &Value) -> anyhow::Result<CallToolResult> {
    plan_cleanup(args, MAX_CLEANUP_STEPS)
}

fn plan_cleanup(args: &Value, step_limit: usize) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let coordinates = ["x1", "y1", "x2", "y2"].map(|key| {
        require_f64(args, key).and_then(|n| {
            n.is_finite()
                .then_some(n)
                .ok_or_else(|| invalid_arg(key, "expected a finite coordinate in mm"))
        })
    });
    let [x1, y1, x2, y2] = match coordinates {
        [Ok(x1), Ok(y1), Ok(x2), Ok(y2)] => [x1, y1, x2, y2],
        _ => {
            return Ok(coordinates
                .into_iter()
                .find_map(Result::err)
                .expect("failed coordinate"))
        }
    };
    let movable = match opt_str_list(args, "movable") {
        Ok(list) => list.unwrap_or_default().into_iter().collect(),
        Err(error) => return Ok(error),
    };
    let (source, tree) = match sch_layout_score::read_saved_sheet(&path) {
        Ok(sheet) => sheet,
        Err(error) => return Ok(error),
    };
    if let Err(reason) = Sheet::new(&tree) {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::MalformedFile {
                path: path.display().to_string(),
                reason: reason.to_owned(),
            },
            format!(
                "Cannot plan cleanup for {}: {reason}. Nothing was written.",
                path.display()
            ),
        ));
    }
    let region = Region::new(x1, y1, x2, y2);
    let Some((steps, skipped, _)) = plan_with_candidate(&source, region, &movable, step_limit)?
    else {
        let reason = format!("the region still has cleanup steps after {step_limit} edits");
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: "plan_schematic_cleanup".to_owned(),
                reasons: vec![reason.clone()],
            },
            format!(
                "Cleanup did not converge: {reason}. Select a smaller region. Nothing was written."
            ),
        ));
    };
    Ok(CallToolResult::json(&json!({
        "schematic": path,
        "source": "saved_schematic",
        "revision": DocumentRevision::of(&source).to_string(),
        "steps": steps,
        "skipped": skipped,
    })))
}

struct Sheet<'a> {
    tree: &'a SexpNode,
    wires: Vec<Wire>,
    buses: Vec<Wire>,
    labels: Vec<Label>,
    placed: super::PlacedPinsByReference,
    attachments: Vec<(&'static str, Point)>,
    // UUIDs are required for replaying exactly the checked edit, not for matching positions.
    symbols: BTreeMap<&'a str, &'a SexpNode>,
}

impl<'a> Sheet<'a> {
    fn new(tree: &'a SexpNode) -> Result<Self, &'static str> {
        let mut ids = BTreeSet::new();
        for tag in ["symbol", "wire", "junction", "no_connect"] {
            for node in tree.find_all(tag) {
                if !node
                    .find_str("uuid")
                    .is_some_and(|id| !id.is_empty() && ids.insert(id))
                {
                    return Err(
                        "a placed symbol, wire, junction or no-connect lacks a unique UUID",
                    );
                }
            }
        }
        let wires = extract_wires(tree);
        let buses = extract_buses(tree);
        for (segments, tag) in [(&wires, "wire"), (&buses, "bus")] {
            if segments.len() != tree.find_all(tag).len()
                || segments
                    .iter()
                    .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
            {
                return Err("a wire or bus has unreadable endpoints");
            }
        }
        let mut attachments = Vec::new();
        for tag in [
            "label",
            "global_label",
            "hierarchical_label",
            "directive_label",
            "netclass_flag",
            "junction",
            "no_connect",
            "bus_entry",
        ] {
            for item in tree.find_all(tag) {
                let Some((x, y, _)) =
                    parse_at(item).filter(|(x, y, _)| x.is_finite() && y.is_finite())
                else {
                    return Err("a connection item has an unreadable position");
                };
                attachments.push((tag, (x, y)));
            }
        }
        attachments.extend(
            extract_sheet_pins(tree)
                .into_iter()
                .map(|at| ("sheet pin", at)),
        );
        attachments.extend(
            super::sch_connectivity::bus_entry_wire_ends(tree, &buses, COINCIDENT_TOLERANCE)
                .into_iter()
                .map(|at| ("bus entry", at)),
        );
        let placed = drawn_symbol_pins(tree).ok_or(UNRESOLVED_PIN_GEOMETRY)?;
        Ok(Self {
            tree,
            wires,
            buses,
            labels: extract_all_net_labels(tree),
            placed,
            attachments,
            symbols: tree
                .find_all("symbol")
                .into_iter()
                .filter_map(|node| Some((node.find_str("uuid")?, node)))
                .collect(),
        })
    }

    fn index(&self) -> ConnectivityIndex<'_> {
        ConnectivityIndex::with_placed_pins(
            self.tree,
            &self.wires,
            &self.buses,
            &self.labels,
            COINCIDENT_TOLERANCE,
            self.placed.clone(),
        )
    }

    fn chains(&self, index: &ConnectivityIndex<'_>) -> Vec<WireChain> {
        wire_chains(index, &self.wires, |at| {
            index.has_pin(at.0, at.1)
                || index.on_bus(at.0, at.1)
                || self.attachments.iter().any(|&(_, p)| near(at, p))
        })
    }

    fn tapped(&self, chain: &WireChain, index: &ConnectivityIndex<'_>) -> bool {
        chain.indices.iter().any(|&i| {
            let wire = &self.wires[i];
            self.attachments
                .iter()
                .any(|&(_, p)| touches_interior(p, wire))
                || index
                    .placed_pins()
                    .iter()
                    .any(|p| touches_interior(p.at, wire))
                || self.wires.iter().enumerate().any(|(j, w)| {
                    !chain.indices.contains(&j)
                        && (ends(w).into_iter().any(|p| touches_interior(p, wire))
                            || collinear_overlap(w, wire))
                })
                || self.buses.iter().any(|bus| {
                    ends(bus).into_iter().any(|p| touches(p, wire))
                        || ends(wire).into_iter().any(|p| touches(p, bus))
                })
        })
    }
}

fn near(a: Point, b: Point) -> bool {
    points_coincident(a.0, a.1, b.0, b.1, COINCIDENT_TOLERANCE)
}

fn same_point(a: Point, b: Point) -> bool {
    schematic_point(a.0, a.1) == schematic_point(b.0, b.1)
}

fn rounded((x, y): Point) -> Point {
    (round6(x), round6(y))
}

fn ends(w: &Wire) -> [Point; 2] {
    [(w.x1, w.y1), (w.x2, w.y2)]
}

fn length(a: Point, b: Point) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn label(pin: &PlacedPin) -> String {
    format!("{} pin {}", pin.reference, pin.pin.number)
}

fn no_connect_change(before: &Sheet<'_>, after: &Sheet<'_>) -> Option<String> {
    let old_markers = before.tree.find_all("no_connect");
    if old_markers.is_empty() {
        return None;
    }
    let owners = |sheet: &Sheet<'_>, at| {
        sheet
            .placed
            .iter()
            .flat_map(|(symbol, pins)| {
                pins.iter()
                    .enumerate()
                    .filter(move |(_, (pin, transform))| {
                        same_point(at, schematic_pin_endpoint(pin, *transform))
                    })
                    .map(|(ordinal, _)| (symbol.uuid.clone(), symbol.unit, ordinal))
            })
            .collect::<BTreeSet<_>>()
    };
    let connected = |sheet: &Sheet<'_>, at| {
        sheet.wires.iter().any(|w| schematic_point_on_wire(at, w))
            || sheet.labels.iter().any(|l| same_point(at, (l.x, l.y)))
            || sheet
                .attachments
                .iter()
                .any(|&(tag, p)| tag == "sheet pin" && same_point(at, p))
    };
    for marker in after.tree.find_all("no_connect") {
        let uuid = marker.find_str("uuid")?;
        let from = location(
            old_markers
                .iter()
                .find(|old| old.find_str("uuid") == Some(uuid))?,
        );
        let to = location(marker);
        let was = owners(before, from);
        let now = owners(after, to);
        if was != now || (connected(after, to) && !connected(before, from)) {
            return Some(format!("would change what no-connect {uuid} protects"));
        }
    }
    None
}

fn id(pin: &PlacedPin) -> &str {
    pin.symbol_uuid
        .as_deref()
        .expect("sheet validates symbol UUIDs")
}

fn power_or_flag(pin: &PlacedPin) -> bool {
    pin.is_power_symbol || pin.reference.starts_with("#FLG") || pin.reference.starts_with("#PWR")
}

fn is_flag(inst: &SymbolInstance) -> bool {
    inst.reference.starts_with("#FLG") || inst.lib_id.rsplit(':').next() == Some("PWR_FLAG")
}

fn path_length(path: &[Point]) -> f64 {
    round6(path.windows(2).map(|p| length(p[0], p[1])).sum())
}

fn location(node: &SexpNode) -> Point {
    let (x, y, _) = parse_at(node).expect("validated position");
    (x, y)
}

fn check_clearance(before: &SexpNode, after: &SexpNode, changed: &[&str]) -> Result<(), String> {
    let report = added_clearance_findings(before, after, changed);
    if !report.bounds_unresolved.is_empty() {
        return Err(format!(
            "cannot check clearance for {}",
            report.bounds_unresolved.join(", ")
        ));
    }
    if let Some(finding) = report.findings.first() {
        return Err(format!(
            "would add a {} finding",
            finding["type"].as_str().unwrap_or("clearance")
        ));
    }
    Ok(())
}

// A draft is also the operation list for replay. Never expose the transient
// UUIDs of the wires and junctions generated for validation.
struct Draft {
    source: String,
    step: Value,
    rank: ([i64; 4], u64, usize, u64),
    new_wires: Vec<String>,
    new_junctions: Vec<String>,
    first_removed_wire: Option<String>,
    power_reference: Option<String>,
}

struct Edit {
    wire_ids: Vec<String>,
    symbols: Vec<(String, Point, f64)>,
    removed_power: Option<String>,
}

impl Edit {
    fn new(sheet: &Sheet<'_>, run: &[usize]) -> Self {
        Self {
            wire_ids: run
                .iter()
                .filter_map(|&i| sheet.wires[i].uuid.clone())
                .collect(),
            symbols: Vec::new(),
            removed_power: None,
        }
    }

    fn prepare(&self, source: &str, before: &SexpNode) -> anyhow::Result<(String, Value, Value)> {
        let mut edits = Vec::new();
        let mut moves = Vec::new();
        for (start, end) in find_direct_child_blocks(source, "kicad_sch") {
            let block = &source[start..end];
            if !["(symbol", "(wire"]
                .iter()
                .any(|tag| block.starts_with(tag))
            {
                continue;
            }
            let node = parse_sexp(block)?;
            let Some(uuid) = node.find_str("uuid") else {
                continue;
            };
            if self.wire_ids.iter().any(|id| id == uuid)
                || self.removed_power.as_deref() == Some(uuid)
            {
                edits.push(SexpEdit::delete(start, end));
            } else if let Some((_, to, angle)) = self.symbols.iter().find(|(id, ..)| id == uuid) {
                let raw = cse::sexp::parser::parse(block)?;
                let mut symbol = cse::Symbol::from_sexp(&raw)?;
                let from = symbol.position();
                symbol.set_rotation(*angle);
                symbol.move_to(to.0, to.1);
                edits.push(SexpEdit::replace(
                    start,
                    end,
                    cse::sexp::writer::write(&symbol.to_sexp()),
                ));
                moves.push(json!({
                    "uuid": uuid, "reference": symbol.reference(),
                    "from": rounded(from), "to": rounded(*to),
                    "dx": round6(to.0 - from.0), "dy": round6(to.1 - from.1), "rotation": angle
                }));
            }
        }
        let source = apply_edits(source.to_owned(), edits);
        let carries = if moves.is_empty() {
            Vec::new()
        } else {
            plan_drawn_no_connect_carry(before, &parse_sexp(&source)?)
                .map_err(|err| anyhow::anyhow!("cannot carry the no-connect markers: {err:?}"))?
        };
        let markers = carries
            .iter()
            .map(|m| {
                json!({
                    "uuid": m.uuid, "from": m.from, "to": m.to
                })
            })
            .collect::<Vec<_>>();
        let source = apply_no_connect_carries(source, &carries)
            .ok_or_else(|| anyhow::anyhow!("cannot locate the carried no-connect marker"))?;
        Ok((source, json!(moves), json!(markers)))
    }
}

struct Planner<'a> {
    source: &'a str,
    sheet: &'a Sheet<'a>,
    index: &'a ConnectivityIndex<'a>,
    score: Score,
    region: Region,
}

struct Proposal<'a> {
    edit: Edit,
    bend: &'a str,
    route: &'a [Point],
    length_before: f64,
    kind: &'a str,
    power_ended: bool,
    retained_tips: &'a [Point],
}

impl Planner<'_> {
    fn check(&self, proposal: Proposal<'_>) -> anyhow::Result<Result<Draft, String>> {
        let Proposal {
            edit,
            bend,
            route,
            length_before,
            kind,
            power_ended,
            retained_tips,
        } = proposal;
        if route.windows(2).any(|p| near(p[0], p[1])) {
            return Ok(Err("the route would have a zero-length leg".into()));
        }
        let (stripped, moves, markers) = match edit.prepare(self.source, self.sheet.tree) {
            Ok(parts) => parts,
            Err(e) => return Ok(Err(e.to_string())),
        };
        let source = if let [a, .., b] = route {
            let base = parse_sexp(&stripped)?;
            let Some(guard) = RouteGuard::for_drawn_pins(&base, *a, &[*b]) else {
                return Ok(Err(UNRESOLVED_PIN_GEOMETRY.into()));
            };
            let remaining = Sheet::new(&base).map_err(anyhow::Error::msg)?;
            if let Some(reason) = route_bus_obstruction(&remaining, route) {
                return Ok(Err(reason));
            }
            let source = super::sch_wiring::draw_route_with_drawn_pins(&stripped, route)?;
            if let Some(reason) = guard.refusal(bend, &source)? {
                return Ok(Err(reason));
            }
            if kind == "power_stub" {
                if let Some(reason) = stub_obstruction(&remaining, *a, *b, &edit) {
                    return Ok(Err(reason));
                }
            } else if let Some(item) = new_route_attachment(&remaining, route, retained_tips, &edit)
            {
                let name = if power_ended { "power lead" } else { "route" };
                return Ok(Err(format!("the {bend} {name} would touch {item}")));
            }
            if let Some(item) = moved_pin_attachment(&remaining, &edit, [*a, *b]) {
                return Ok(Err(item));
            }
            source
        } else {
            stripped
        };
        let old_dots = self
            .sheet
            .tree
            .find_all("junction")
            .into_iter()
            .map(location)
            .filter(|&at| {
                self.sheet.wires.iter().any(|w| {
                    w.uuid.as_ref().is_some_and(|id| edit.wire_ids.contains(id)) && touches(at, w)
                }) || edit.removed_power.as_deref().is_some_and(|id| {
                    self.index
                        .placed_pins()
                        .iter()
                        .any(|pin| pin.symbol_uuid.as_deref() == Some(id) && near(at, pin.at))
                })
            })
            .collect::<Vec<_>>();
        let (source, _, _) = super::sch_wiring::reconcile_junctions_at(source, &old_dots);
        let tree = parse_sexp(&source)?;
        let after = match Sheet::new(&tree) {
            Ok(sheet) => sheet,
            Err(e) => return Ok(Err(e.into())),
        };
        if let Some(reason) = no_connect_change(self.sheet, &after) {
            return Ok(Err(reason));
        }
        let changes =
            sch_layout_score::cleanup_gate(self.sheet.tree, &tree, edit.removed_power.as_slice());
        if !changes.is_empty() {
            return Ok(Err(changes.join("; ")));
        }
        let old_wires = self
            .sheet
            .wires
            .iter()
            .filter_map(|w| w.uuid.as_deref())
            .collect::<BTreeSet<_>>();
        let new_wires = after
            .wires
            .iter()
            .filter_map(|w| {
                w.uuid
                    .as_ref()
                    .filter(|id| !old_wires.contains(id.as_str()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let changed = edit
            .wire_ids
            .iter()
            .chain(edit.removed_power.iter())
            .chain(edit.symbols.iter().map(|(uuid, ..)| uuid))
            .chain(&new_wires)
            .map(String::as_str)
            .collect::<Vec<_>>();
        if let Err(reason) = check_clearance(self.sheet.tree, &tree, &changed) {
            return Ok(Err(reason));
        }
        let length_after = path_length(route);
        if !route.is_empty() && length_after < GRID {
            return Ok(Err(
                "the route would leave less than 2.54 mm between pin tips".into(),
            ));
        }
        if length_after >= length_before
            && edit.removed_power.is_none()
            && added_clearance_findings(&tree, self.sheet.tree, &changed)
                .findings
                .is_empty()
        {
            return Ok(Err("the run would not improve".into()));
        }
        let Some(score_rank) = sch_layout_score::cleanup_rank(&self.score, &tree) else {
            return Ok(Err("the layout score would not improve".into()));
        };
        let old_junctions = self.sheet.tree.find_all("junction");
        let new_junctions = tree.find_all("junction");
        let added = new_junctions
            .iter()
            .filter(|j| !old_junctions.contains(j))
            .collect::<Vec<_>>();
        let removed = old_junctions
            .iter()
            .filter(|j| !new_junctions.contains(j))
            .filter_map(|j| j.find_str("uuid"))
            .collect::<Vec<_>>();
        let distance_moved: f64 = edit
            .symbols
            .iter()
            .map(|(id, p, _)| length(location(self.sheet.symbols[id.as_str()]), *p))
            .sum();
        let extends_outside_region = route.iter().any(|&p| !self.region.contains(p))
            || added
                .iter()
                .any(|node| !self.region.contains(location(node)))
            || edit.symbols.iter().any(|(id, to, _)| {
                !self.region.contains(*to)
                    || !self
                        .region
                        .contains(location(self.sheet.symbols[id.as_str()]))
            })
            || markers.as_array().into_iter().flatten().any(|m| {
                ["from", "to"].into_iter().any(|k| {
                    !self
                        .region
                        .contains((m[k][0].as_f64().unwrap(), m[k][1].as_f64().unwrap()))
                })
            });
        let new_junctions = added
            .iter()
            .filter_map(|j| j.find_str("uuid").map(str::to_owned))
            .collect();
        let added = added
            .iter()
            .map(|node| rounded(location(node)))
            .collect::<Vec<_>>();
        let rank = (
            score_rank,
            (length_after * 1e6).round() as u64,
            route.len().saturating_sub(2),
            (distance_moved * 1e6).round() as u64,
        );
        Ok(Ok(Draft {
            source,
            rank,
            new_wires,
            new_junctions,
            first_removed_wire: edit.wire_ids.first().cloned(),
            power_reference: None,
            step: json!({
                "kind": kind, "wires_removed": edit.wire_ids, "symbols_moved": moves,
                "no_connects_moved": markers, "route": route,
                "junctions_added": added, "junctions_removed": removed,
                "extends_outside_region": extends_outside_region,
                "power_symbols_removed": edit.removed_power.into_iter().collect::<Vec<_>>(),
                "length_before_mm": length_before, "length_after_mm": length_after
            }),
        }))
    }

    fn pin_outward(&self, pin: &PlacedPin) -> f64 {
        pin_outward_direction(
            &pin.pin,
            self.sheet.placed[pin.symbol_index].0.pin_transform(),
        )
    }

    fn on_point(&self, p: Point) -> impl Iterator<Item = &PlacedPin> {
        self.index
            .pin_indices_at(p.0, p.1)
            .into_iter()
            .map(|i| &self.index.placed_pins()[i])
    }

    fn pin_terminals(&self, p: Point) -> impl Iterator<Item = &PlacedPin> {
        self.on_point(p)
            .filter(|pin| !power_or_flag(pin) && !pin.pin.hidden)
    }

    fn pin_terminal(&self, p: Point) -> Option<&PlacedPin> {
        let mut pins = self.pin_terminals(p);
        let first = pins.next()?;
        pins.next().is_none().then_some(first)
    }

    fn ordinary(
        &self,
        run: &WireChain,
        movable: &BTreeSet<String>,
    ) -> anyhow::Result<(Option<Draft>, Vec<Value>)> {
        let [a, b] = run.ends;
        let terminal = |at| {
            self.pin_terminals(at)
                .next()
                .or_else(|| self.on_point(at).find(|p| power_or_flag(p)))
        };
        let (Some(a), Some(b)) = (terminal(a), terminal(b)) else {
            return Ok((None, vec![]));
        };
        if near(a.at, b.at) {
            return Ok((None, vec![]));
        }
        let (a, b) = if (&a.reference, &a.pin.number, id(a)) <= (&b.reference, &b.pin.number, id(b))
        {
            (a, b)
        } else {
            (b, a)
        };
        let run_name = format!("{} to {}", label(a), label(b));
        let power_ended = power_or_flag(a) || power_or_flag(b);
        let skip = |reason: String| json!({"run": run_name, "reason": reason});
        if power_ended
            && [a, b].into_iter().any(|pin| {
                let at = pin.at;
                self.on_point(at).count() != 1
                    || self
                        .sheet
                        .attachments
                        .iter()
                        .any(|&(_, point)| near(at, point))
                    || self
                        .sheet
                        .wires
                        .iter()
                        .enumerate()
                        .any(|(i, wire)| !run.indices.contains(&i) && touches(at, wire))
            })
        {
            return Ok((
                None,
                vec![skip("the power lead has another attachment".into())],
            ));
        }
        if self.sheet.tapped(run, self.index) {
            return Ok((
                None,
                vec![skip(
                    "the run has an interior attachment or overlapping wire".into(),
                )],
            ));
        }
        let length_before = self.chain_length(run);
        let mut best: Option<Draft> = None;
        let mut reasons = Vec::new();
        let mut skipped = Vec::new();
        let mut consider =
            |edit, bend, path: &[Point], retained_tips: &[Point]| -> anyhow::Result<()> {
                match self.check(Proposal {
                    edit,
                    bend,
                    route: path,
                    length_before,
                    kind: "reroute",
                    power_ended,
                    retained_tips,
                })? {
                    Ok(draft) if best.as_ref().is_none_or(|other| draft.rank < other.rank) => {
                        best = Some(draft)
                    }
                    Err(reason) if !reasons.contains(&reason) => reasons.push(reason),
                    _ => {}
                }
                Ok(())
            };
        for (bend, path) in ordered_route_paths(self.sheet.tree, rounded(a.at), rounded(b.at)).0 {
            consider(Edit::new(self.sheet, &run.indices), bend, &path, &run.ends)?;
        }
        for (moving, fixed) in [(a, b), (b, a)] {
            if power_or_flag(moving) {
                continue;
            }
            if self.pin_terminals(moving.at).count() != 1
                || (!power_or_flag(fixed) && self.pin_terminals(fixed.at).count() != 1)
            {
                for listed in self
                    .pin_terminals(moving.at)
                    .filter(|p| movable.contains(&p.reference))
                {
                    skipped.push(json!({
                        "reference": listed.reference, "run": run_name,
                        "reason": "a shared pin tip prevents translating this run"
                    }));
                }
                continue;
            }
            if !movable.contains(&moving.reference) {
                continue;
            }
            if movable.contains(&fixed.reference) || moving.symbol_index == fixed.symbol_index {
                skipped.push(json!({
                    "reference": moving.reference, "run": run_name,
                    "reason": "both ends are movable; interacting symbol moves are not planned"
                }));
                continue;
            }
            let satellites = match self.satellites(moving, run) {
                Ok(satellites) => satellites,
                Err(reason) => {
                    skipped.push(
                        json!({"reference": moving.reference, "run": run_name, "reason": reason}),
                    );
                    continue;
                }
            };
            let d = stub_direction("auto", Some(self.pin_outward(fixed)));
            let fixed_at = rounded(fixed.at);
            let mut offsets = (2_i32..=8)
                .flat_map(|k| (-4_i32..=4).map(move |j| (k, j)))
                .collect::<Vec<_>>();
            offsets.sort_by_key(|&(k, j)| (k + j.abs(), k, j));
            for (k, j) in offsets {
                let at = rounded((
                    fixed_at.0 + GRID * (f64::from(k) * d.dx - f64::from(j) * d.dy),
                    fixed_at.1 + GRID * (f64::from(k) * d.dy + f64::from(j) * d.dx),
                ));
                let dx = at.0 - moving.at.0;
                let dy = at.1 - moving.at.1;
                if near(at, moving.at) {
                    continue;
                }
                for (bend, path) in route_paths(at, fixed_at) {
                    let mut edit = Edit::new(self.sheet, &run.indices);
                    edit.symbols = satellites
                        .iter()
                        .map(|&i| {
                            let inst = &self.sheet.placed[i].0;
                            (
                                inst.uuid.clone().expect("sheet validates symbol UUIDs"),
                                rounded((inst.x + dx, inst.y + dy)),
                                inst.rotation,
                            )
                        })
                        .collect();
                    consider(edit, bend, &path, &[fixed_at])?;
                }
            }
        }
        if best.is_none() {
            skipped.push(skip(reasons.join("; ")));
        }
        Ok((best, skipped))
    }

    fn chain_length(&self, run: &WireChain) -> f64 {
        round6(
            run.indices
                .iter()
                .map(|&i| {
                    let [a, b] = ends(&self.sheet.wires[i]);
                    length(a, b)
                })
                .sum(),
        )
    }

    fn satellites(&self, moving: &PlacedPin, run: &WireChain) -> Result<BTreeSet<usize>, String> {
        let mut satellites = BTreeSet::from([moving.symbol_index]);
        let moving_id = id(moving);
        if self
            .sheet
            .placed
            .iter()
            .filter(|(s, _)| s.reference == moving.reference)
            .count()
            != 1
        {
            return Err(format!(
                "{} is not a single placed symbol",
                moving.reference
            ));
        }
        for own in self
            .index
            .placed_pins()
            .iter()
            .filter(|p| id(p) == moving_id)
        {
            let at = own.at;
            let primary = std::ptr::eq(own, moving);
            if self
                .sheet
                .wires
                .iter()
                .enumerate()
                .any(|(i, w)| touches(at, w) && (!primary || !run.indices.contains(&i)))
            {
                for other_run in self.sheet.chains(self.index) {
                    let [a, b] = other_run.ends;
                    let opposite = if near(at, a) {
                        Some(b)
                    } else if near(at, b) {
                        Some(a)
                    } else {
                        None
                    };
                    if let Some(other) = opposite.and_then(|p| self.pin_terminal(p)) {
                        if !other_run.indices.iter().any(|i| run.indices.contains(i)) {
                            return Err(format!(
                                "{} also carries the run to {}",
                                label(own),
                                label(other)
                            ));
                        }
                    }
                }
                return Err(format!("{} also carries another wire", label(own)));
            }
            if self
                .sheet
                .attachments
                .iter()
                .any(|&(tag, p)| tag != "no_connect" && near(at, p))
                || self.sheet.buses.iter().any(|w| touches(at, w))
            {
                return Err(format!("{} has an attachment that cannot move", label(own)));
            }
            for other in self.on_point(at).filter(|p| id(p) != moving_id) {
                let (symbol, pins) = &self.sheet.placed[other.symbol_index];
                if !other.is_power_symbol
                    || pins.len() != 1
                    || is_flag(symbol)
                    || self.on_point(at).filter(|p| id(p) != moving_id).count() != 1
                {
                    return Err(format!("{} also touches {}", label(own), label(other)));
                }
                satellites.insert(other.symbol_index);
            }
        }
        Ok(satellites)
    }

    fn power(
        &self,
        power: &SymbolInstance,
        power_pin: &PlacedPin,
        runs: &[WireChain],
    ) -> anyhow::Result<Result<Draft, String>> {
        let loose = power_pin.at;
        if self
            .sheet
            .wires
            .iter()
            .filter(|w| touches(loose, w))
            .count()
            > 1
        {
            return Ok(Err(
                "the power symbol touches a shared or tapped lead".into()
            ));
        }
        let lead = || {
            runs.iter().find_map(|run| {
                if near(loose, run.ends[0]) {
                    Some((run, run.ends[1]))
                } else if near(loose, run.ends[1]) {
                    Some((run, run.ends[0]))
                } else {
                    None
                }
            })
        };
        let (run, tip) = if self
            .pin_terminal(loose)
            .is_some_and(|pin| same_point(loose, pin.at))
        {
            (None, loose)
        } else {
            match lead() {
                Some((run, tip)) if !near(loose, tip) => (Some(run), tip),
                _ => return Ok(Err("the power symbol has no isolated lead to a pin".into())),
            }
        };
        if !self.region.contains(tip) || run.is_some_and(|run| !self.inside(run)) {
            return Ok(Err("the power lead leaves the region".into()));
        }
        let Some(fixed) = self.pin_terminal(tip) else {
            return Ok(Err(
                "the lead does not end at a single component pin; it may be tapped".into(),
            ));
        };
        let others = |at| {
            self.on_point(at)
                .any(|p| id(p) != id(power_pin) && id(p) != id(fixed))
        };
        let run_indices = run.map_or(&[][..], |r| r.indices.as_slice());
        if (run.is_some() && self.on_point(loose).any(|p| id(p) != id(power_pin)))
            || run.is_some_and(|r| self.sheet.tapped(r, self.index))
        {
            return Ok(Err("the power lead is tapped or stacked".into()));
        }
        let attached_wires = self
            .sheet
            .wires
            .iter()
            .enumerate()
            .filter(|(i, w)| !run_indices.contains(i) && touches(tip, w))
            .map(|(_, w)| w);
        let extra_wire = attached_wires.clone().next().is_some();
        let forbidden = self.sheet.attachments.iter().any(|&(tag, p)| {
            (near(p, loose) || near(p, tip)) && !(tag == "junction" && extra_wire && near(p, tip))
        }) || self
            .sheet
            .buses
            .iter()
            .any(|w| touches(tip, w) || touches(loose, w));
        if forbidden {
            return Ok(Err(format!(
                "{} or its lead has another attachment",
                label(fixed)
            )));
        }
        let mut edit = Edit::new(self.sheet, run_indices);
        let length_before = run.map_or(0.0, |r| self.chain_length(r));
        if extra_wire {
            if !self
                .on_point(loose)
                .any(|p| p.is_power_symbol && id(p) != id(power_pin))
                && self.redundant(power_pin, tip, run_indices)
            {
                edit.removed_power = Some(id(power_pin).to_owned());
                return self.check(Proposal {
                    edit,
                    bend: "",
                    route: &[],
                    length_before,
                    kind: "remove_redundant_power",
                    power_ended: false,
                    retained_tips: &[],
                });
            }
            return Ok(Err(format!("{} also carries another wire", label(fixed))));
        }
        if others(tip) {
            return Ok(Err(format!("{} also touches another symbol", label(fixed))));
        }
        let outward = self.pin_outward(fixed);
        let end = stub_direction("auto", Some(outward)).end(tip, GRID);
        let route = [rounded(tip), end];
        let uuid = id(power_pin);
        let (origin, angle) = power_placement(power, power_pin, end, outward, |origin| {
            // Inspect the proposed stub, not a second copy of the symbol on its old lead.
            let mut trial = Edit::new(self.sheet, run_indices);
            trial.symbols.push((uuid.to_owned(), origin, 0.0));
            let (stripped, _, _) = match trial.prepare(self.source, self.sheet.tree) {
                Ok(parts) => parts,
                Err(error) => return Ok(Some(error.to_string())),
            };
            let source = super::sch_wiring::draw_route_with_drawn_pins(&stripped, &route)?;
            let candidate = parse_sexp(&source)?;
            Ok(library_power_clearance_reason(
                self.sheet.tree,
                &candidate,
                uuid,
                tip,
            ))
        })?;
        edit.symbols.push((uuid.to_owned(), origin, angle));
        self.check(Proposal {
            edit,
            bend: "power stub",
            route: &route,
            length_before,
            kind: "power_stub",
            power_ended: false,
            retained_tips: &[],
        })
    }

    fn redundant(&self, power: &PlacedPin, tip: Point, removed: &[usize]) -> bool {
        let remaining = self
            .sheet
            .wires
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed.contains(i))
            .map(|(_, w)| w.clone())
            .collect::<Vec<_>>();
        let Some(driver) = self
            .sheet
            .labels
            .iter()
            .find(|l| l.kind == LabelKind::PowerSymbol && l.uuid.as_deref() == Some(id(power)))
        else {
            return false;
        };
        let labels = self
            .sheet
            .labels
            .iter()
            .filter(|l| l.uuid.as_deref() != Some(id(power)))
            .cloned()
            .collect::<Vec<_>>();
        let mut drawn = exact_drawn_net_graph(
            self.sheet.tree,
            &remaining,
            &copperpour_sexp::schematic::extract_labels(self.sheet.tree),
        );
        let root = drawn.root_at(tip.0, tip.1);
        labels.iter().any(|l| {
            l.kind == LabelKind::PowerSymbol
                && l.net == driver.net
                && drawn.root_at(l.x, l.y) == root
        })
    }

    fn inside(&self, chain: &WireChain) -> bool {
        chain.indices.iter().all(|&i| {
            ends(&self.sheet.wires[i])
                .into_iter()
                .all(|p| self.region.contains(p))
        })
    }
}

fn power_placement(
    power: &SymbolInstance,
    pin: &PlacedPin,
    end: Point,
    outward: f64,
    library_blocked: impl FnOnce(Point) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<(Point, f64)> {
    let mut transform = power.pin_transform();
    transform.comp_x = 0.0;
    transform.comp_y = 0.0;
    let facing = copperpour_sexp::transform_direction(
        outward,
        copperpour_sexp::PinTransform {
            rotation_deg: 0.0,
            ..transform
        },
    );
    let origin = |angle| {
        let offset = copperpour_sexp::schematic::pin_endpoint(
            &pin.pin,
            copperpour_sexp::PinTransform {
                rotation_deg: angle,
                ..transform
            },
        );
        rounded((end.0 - offset.0, end.1 - offset.1))
    };
    let (angle, _) =
        select_power_symbol_rotation(&pin.pin, facing, || library_blocked(origin(0.0)))?;
    Ok((origin(angle), angle))
}

fn stub_obstruction(sheet: &Sheet<'_>, from: Point, to: Point, edit: &Edit) -> Option<String> {
    power_route_obstruction(sheet, from, to, &[from], &edit.symbols)
        .map(|item| format!("the power stub would touch {item}"))
}

fn power_route_obstruction(
    sheet: &Sheet<'_>,
    from: Point,
    to: Point,
    terminals: &[Point],
    moved: &[(String, Point, f64)],
) -> Option<String> {
    let stub = Wire {
        x1: from.0,
        y1: from.1,
        x2: to.0,
        y2: to.1,
        uuid: None,
    };
    let touches_stub = |p| !terminals.iter().any(|&end| near(p, end)) && touches(p, &stub);
    let index = sheet.index();
    for pin in index.placed_pins() {
        if touches_stub(pin.at)
            && !moved
                .iter()
                .any(|(id, ..)| Some(id.as_str()) == pin.symbol_uuid.as_deref())
        {
            return Some(label(pin));
        }
    }
    for &(tag, p) in &sheet.attachments {
        if touches_stub(p) {
            return Some(format!("a {tag}"));
        }
    }
    if sheet.wires.iter().chain(&sheet.buses).any(|w| {
        [from, to]
            .into_iter()
            .any(|end| touches_stub(end) && touches(end, w))
            || ends(w).into_iter().any(touches_stub)
            || collinear_overlap(&stub, w)
    }) {
        return Some("another wire or bus".into());
    }
    None
}

fn route_bus_obstruction(sheet: &Sheet<'_>, route: &[Point]) -> Option<String> {
    for segment in route.windows(2) {
        let [(x1, y1), (x2, y2)] = [segment[0], segment[1]];
        let leg = Wire {
            x1,
            y1,
            x2,
            y2,
            uuid: None,
        };
        if sheet
            .attachments
            .iter()
            .any(|&(tag, p)| tag == "bus entry" && touches(p, &leg))
            || sheet.buses.iter().any(|bus| {
                ends(&leg).into_iter().any(|p| touches(p, bus))
                    || ends(bus).into_iter().any(|p| touches(p, &leg))
                    || collinear_overlap(&leg, bus)
            })
        {
            return Some("the route would attach to a bus or bus entry".into());
        }
    }
    None
}

fn new_route_attachment(
    sheet: &Sheet<'_>,
    route: &[Point],
    retained_tips: &[Point],
    edit: &Edit,
) -> Option<String> {
    let moved = |uuid: &str| edit.symbols.iter().any(|(id, ..)| id == uuid);
    let new_point = |p| !retained_tips.iter().any(|&t| same_point(t, p));
    let route_end = |p| same_point(route[0], p) || same_point(route[route.len() - 1], p);
    for leg in route.windows(2) {
        let segment = Wire {
            x1: leg[0].0,
            y1: leg[0].1,
            x2: leg[1].0,
            y2: leg[1].1,
            uuid: None,
        };
        let new_contact = |p| new_point(p) && touches(p, &segment);
        if sheet.wires.iter().any(|wire| {
            ends(wire).into_iter().any(new_contact)
                || leg.iter().any(|&p| {
                    touches(p, wire)
                        && (new_contact(p)
                            || (schematic_point_on_wire(p, wire)
                                && !ends(wire).into_iter().any(|tip| same_point(p, tip))
                                && !sheet.attachments.iter().any(|&(kind, at)| {
                                    matches!(
                                        kind,
                                        "junction"
                                            | "label"
                                            | "global_label"
                                            | "hierarchical_label"
                                    ) && same_point(p, at)
                                })))
                })
        }) {
            return Some("another wire".into());
        }
        if let Some((kind, _)) = sheet
            .attachments
            .iter()
            .find(|&&(kind, p)| kind != "no_connect" && new_contact(p))
        {
            return Some(format!("a {kind}"));
        }
        for (symbol, pins) in &sheet.placed {
            for (pin, transform) in pins {
                let at = schematic_pin_endpoint(pin, *transform);
                if new_contact(at) && !(route_end(at) && symbol.uuid.as_deref().is_some_and(moved))
                {
                    return Some(format!("{} pin {}", symbol.reference, pin.number));
                }
            }
        }
    }
    None
}

// The route checks its own tips. Check every other moved pin against the
// retained sheet: the net gate cannot see a carried pin picking up a bare wire.
fn moved_pin_attachment(sheet: &Sheet<'_>, edit: &Edit, route_tips: [Point; 2]) -> Option<String> {
    if edit.symbols.is_empty() {
        return None;
    }
    let moved = |symbol: &SymbolInstance| {
        edit.symbols
            .iter()
            .any(|(id, ..)| Some(id) == symbol.uuid.as_ref())
    };
    let mut stationary = BTreeSet::new();
    for (symbol, pins) in &sheet.placed {
        if !moved(symbol) {
            stationary.extend(pins.iter().map(|(pin, transform)| {
                let (x, y) = schematic_pin_endpoint(pin, *transform);
                schematic_point(x, y)
            }));
        }
    }
    for (symbol, pins) in &sheet.placed {
        if !moved(symbol) {
            continue;
        }
        for (pin, transform) in pins {
            let at = schematic_pin_endpoint(pin, *transform);
            if route_tips.into_iter().any(|tip| same_point(at, tip)) {
                continue;
            }
            let item = if sheet
                .wires
                .iter()
                .chain(&sheet.buses)
                .any(|wire| touches(at, wire))
            {
                "another wire or bus"
            } else if stationary.contains(&schematic_point(at.0, at.1)) {
                "another pin"
            } else if let Some(&(kind, _)) = sheet.attachments.iter().find(|&&(kind, point)| {
                // A carried marker is checked by identity after the route is drawn.
                kind != "no_connect" && same_point(at, point)
            }) {
                kind
            } else {
                continue;
            };
            return Some(format!(
                "{} pin {} would touch {item}",
                symbol.reference, pin.number
            ));
        }
    }
    None
}

type Plan = (Vec<Value>, Vec<Value>, String);

fn plan_with_candidate(
    source: &str,
    region: Region,
    movable: &BTreeSet<String>,
    step_limit: usize,
) -> anyhow::Result<Option<Plan>> {
    let original = parse_sexp(source)?;
    let mut allocated = original
        .children()
        .into_iter()
        .flatten()
        .filter_map(|node| node.find_str("uuid").map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let mut serial = 0_u64;
    let revision = DocumentRevision::of(source).hash();
    let mut next_id = || loop {
        serial += 1;
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&revision.to_be_bytes());
        bytes[8..].copy_from_slice(&serial.to_be_bytes());
        let uuid = uuid::Builder::from_custom_bytes(bytes)
            .into_uuid()
            .to_string();
        if allocated.insert(uuid.clone()) {
            break uuid;
        }
    };
    let mut current = source.to_owned();
    let mut steps = Vec::new();
    let mut move_skips = Vec::new();
    let mut cleaned_wires = BTreeSet::new();
    let mut cleaned_power = BTreeSet::new();
    loop {
        let tree = parse_sexp(&current)?;
        let sheet = Sheet::new(&tree).map_err(anyhow::Error::msg)?;
        let index = sheet.index();
        let planner = Planner {
            source: &current,
            sheet: &sheet,
            index: &index,
            score: sch_layout_score::measure(&tree),
            region,
        };
        let runs = sheet.chains(&index);
        let mut skipped = Vec::new();
        let mut next = None;
        let mut power_leads = BTreeMap::new();
        for (symbol_index, (symbol, _)) in sheet.placed.iter().enumerate() {
            if !movable.contains(&symbol.reference) {
                continue;
            }
            let pins = index
                .placed_pins()
                .iter()
                .filter(|p| p.symbol_index == symbol_index)
                .collect::<Vec<_>>();
            if !pins.iter().any(|p| region.contains(p.at)) {
                continue;
            }
            let outcome = if is_flag(symbol) {
                Err("PWR_FLAG never moves".into())
            } else if let [pin] = pins.as_slice() {
                if !pin.is_power_symbol {
                    continue;
                }
                if sheet
                    .placed
                    .iter()
                    .filter(|(s, _)| s.reference == symbol.reference)
                    .count()
                    != 1
                {
                    Err("the reference names more than one placed power symbol".into())
                } else {
                    planner.power(symbol, pin, &runs)?
                }
            } else {
                if !pins.iter().any(|p| p.is_power_symbol) {
                    continue;
                }
                Err("the power symbol does not have one resolved pin".into())
            };
            match outcome {
                Ok(mut draft) => {
                    draft.power_reference = Some(symbol.reference.clone());
                    if let Some(wire) = draft.first_removed_wire.take() {
                        power_leads.insert(wire, draft);
                    } else {
                        next = Some(draft);
                        break;
                    }
                }
                Err(reason) if !cleaned_power.contains(&symbol.reference) => {
                    skipped.push(json!({"reference": symbol.reference, "reason": reason}))
                }
                Err(_) => {}
            }
        }
        if next.is_none() {
            for run in runs.iter().filter(|r| planner.inside(r)) {
                let (mut draft, reasons) = planner.ordinary(run, movable)?;
                if let Some(power) = sheet.wires[run.indices[0]]
                    .uuid
                    .as_ref()
                    .and_then(|wire| power_leads.remove(wire))
                {
                    if draft
                        .as_ref()
                        .is_none_or(|ordinary| power.rank < ordinary.rank)
                    {
                        draft = Some(power);
                    }
                }
                let cleaned = run.indices.iter().any(|&i| {
                    sheet.wires[i]
                        .uuid
                        .as_ref()
                        .is_some_and(|id| cleaned_wires.contains(id))
                });
                skipped.extend(
                    reasons
                        .into_iter()
                        .filter(|item| !cleaned || !item["reference"].is_null()),
                );
                if draft.is_some() {
                    for reason in skipped
                        .iter()
                        .filter(|reason| !reason["reference"].is_null())
                    {
                        if !move_skips.contains(reason) {
                            move_skips.push(reason.clone());
                        }
                    }
                    next = draft;
                    break;
                }
            }
        }
        let Some(mut draft) = next else {
            for reason in move_skips {
                if !skipped.contains(&reason)
                    && !steps
                        .iter()
                        .flat_map(|step: &Value| {
                            step["symbols_moved"].as_array().into_iter().flatten()
                        })
                        .any(|m| m["reference"] == reason["reference"])
                {
                    skipped.push(reason);
                }
            }
            return Ok(Some((steps, skipped, current)));
        };
        // One final search distinguishes convergence at the limit from a
        // partial plan. Never return a prefix that appears ready for replay.
        if steps.len() >= step_limit {
            return Ok(None);
        }
        if let Some(reference) = draft.power_reference.take() {
            cleaned_power.insert(reference);
        }
        // A later step can replace a previous route after another run moves
        // out of the way. Give its additions stable IDs so replay needs no
        // knowledge of the scratch UUIDs issued by the wire writer.
        let mut stable_ids = |scratch: Vec<String>| {
            scratch
                .into_iter()
                .map(|id| {
                    let replacement = next_id();
                    draft.source = draft.source.replace(
                        &format!("(uuid \"{id}\")"),
                        &format!("(uuid \"{replacement}\")"),
                    );
                    replacement
                })
                .collect::<Vec<_>>()
        };
        let wire_ids = stable_ids(draft.new_wires);
        if draft.step["kind"] == "reroute" {
            cleaned_wires.extend(wire_ids.iter().cloned());
        }
        draft.step["wire_uuids_added"] = json!(wire_ids);
        draft.step["junction_uuids_added"] = json!(stable_ids(draft.new_junctions));
        current = draft.source;
        steps.push(draft.step);
    }
}
