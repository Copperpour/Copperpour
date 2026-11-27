//! Readability evidence for a drawn run measured by the wire-run check.

use super::{
    drawn_symbol_pin_inventory, label_edit::native_bus_entry_wire_ends, mm, point, WireRun,
    WireRuns,
};
use crate::tools::sch_connectivity::{label_roots, net_graph_for, NetGraph, NetRoot};
use copperpour_sexp::{
    schematic::{extract_buses, extract_sheet_pins, pin_endpoint, Label, LabelKind, Wire},
    SexpNode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

type Point = (f64, f64);

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn is_valid(self) -> bool {
        self.min.0 < self.max.0 && self.min.1 < self.max.1
    }

    fn contains(self, (x, y): Point) -> bool {
        (self.min.0..=self.max.0).contains(&x) && (self.min.1..=self.max.1).contains(&y)
    }

    fn intersects(self, a: Point, b: Point) -> bool {
        a.0.min(b.0) <= self.max.0
            && self.min.0 <= a.0.max(b.0)
            && a.1.min(b.1) <= self.max.1
            && self.min.1 <= a.1.max(b.1)
    }

    /// For an orthogonal run, count only a passage through the block's
    /// interior, not a tangent or a run terminating in that block.
    fn crosses(self, points: &[Point]) -> bool {
        let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
            return false;
        };
        !self.contains(first)
            && !self.contains(last)
            && points.windows(2).any(|p| {
                p[0].0.min(p[1].0) < self.max.0
                    && self.min.0 < p[0].0.max(p[1].0)
                    && p[0].1.min(p[1].1) < self.max.1
                    && self.min.1 < p[0].1.max(p[1].1)
            })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    long_run_mm: f64,
    region: Option<Bounds>,
    wire_uuids: Option<Vec<String>>,
    #[serde(default)]
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    name: String,
    bounds: Bounds,
}

impl Request {
    pub(super) fn schema() -> Value {
        let point =
            json!({ "type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2 });
        let bounds = json!({
            "type": "object",
            "properties": { "min": point, "max": point },
            "required": ["min", "max"],
            "description": "Lower and upper corners [x, y] in mm of a nonempty rectangle"
        });
        json!({
            "type": "object",
            "description": "Read-only labels-versus-wires advice. Select one region or one exact run.",
            "properties": {
                "long_run_mm": { "type": "number", "exclusiveMinimum": 0,
                    "description": "Minimum drawn length in mm for label advice; no default." },
                "region": bounds,
                "wire_uuids": {
                    "type": "array", "minItems": 1, "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1 },
                    "description": "The complete set of saved wire UUIDs for one run."
                },
                "blocks": { "type": "array", "items": {
                    "type": "object",
                    "properties": { "name": { "type": "string", "minLength": 1 }, "bounds": bounds },
                    "required": ["name", "bounds"]
                }}
            },
            "required": ["long_run_mm"],
            "oneOf": [{ "required": ["region"] }, { "required": ["wire_uuids"] }]
        })
    }

    pub(super) fn parse(value: &Value) -> Result<Self, &'static str> {
        let request: Self = serde_json::from_value(value.clone()).map_err(|_| {
            "requires long_run_mm and either region or wire_uuids; blocks must be named rectangles"
        })?;
        if !(request.long_run_mm.is_finite() && request.long_run_mm > 0.0) {
            return Err("long_run_mm must be positive");
        }
        match (&request.region, &request.wire_uuids) {
            (Some(bounds), None) if bounds.is_valid() => {}
            (Some(_), None) => return Err("region min must be below max on both axes"),
            (None, Some(uuids))
                if !uuids.is_empty()
                    && uuids.iter().all(|uuid| !uuid.trim().is_empty())
                    && uuids.iter().collect::<BTreeSet<_>>().len() == uuids.len() => {}
            (None, Some(_)) => return Err("wire_uuids must contain distinct nonempty wire UUIDs"),
            _ => return Err("provide exactly one of region or wire_uuids"),
        }
        if request
            .blocks
            .iter()
            .any(|block| block.name.trim().is_empty() || !block.bounds.is_valid())
        {
            return Err("blocks need a name and min below max on both axes");
        }
        Ok(request)
    }

    fn select<'a>(&self, measured: &'a WireRuns) -> Result<Vec<&'a WireRun>, &'static str> {
        if let Some(region) = self.region {
            return Ok(measured
                .runs
                .iter()
                .filter(|run| {
                    run.segments
                        .iter()
                        .any(|edge| region.intersects(mm(edge.a), mm(edge.b)))
                })
                .collect());
        }
        let selected: BTreeSet<_> = self
            .wire_uuids
            .as_ref()
            .expect("validated selector")
            .iter()
            .map(String::as_str)
            .collect();
        let matches: Vec<_> = measured
            .runs
            .iter()
            .filter(|run| wire_uuids(run, &measured.wires).is_some_and(|uuids| uuids == selected))
            .collect();
        match matches.len() {
            1 => Ok(matches),
            0 => Err("wire_uuids do not identify one measured run"),
            _ => Err("wire_uuids identify more than one run; select a region instead"),
        }
    }
}

pub(super) fn wire_uuids<'a>(run: &WireRun, wires: &'a [Wire]) -> Option<BTreeSet<&'a str>> {
    run.segments
        .iter()
        .map(|edge| wires[edge.wire].uuid.as_deref())
        .collect()
}

/// The intersection with an orthogonal run segment. Endpoints are excluded
/// on the run side; on the other side, remember whether the wire extends to
/// both sides of the run. Two collinear wires meeting here can still cross it.
fn crossing_sides(a: Point, b: Point, wire: &Wire) -> Option<(Point, u8)> {
    let vertical = a.0 == b.0;
    let axes = |(x, y)| if vertical { (y, x) } else { (x, y) };
    let (lo, line) = axes(a);
    let (hi, _) = axes(b);
    let (u, v) = axes((wire.x1, wire.y1));
    let (s, t) = axes((wire.x2, wire.y2));
    if v == t || line < v.min(t) || line > v.max(t) {
        return None;
    }
    let along = u + (s - u) * ((line - v) / (t - v));
    if !(lo.min(hi) < along && along < lo.max(hi)) {
        return None;
    }
    let sides = u8::from(v.min(t) < line) | (u8::from(v.max(t) > line) << 1);
    Some((axes((along, line)), sides))
}

fn straight_spans(points: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    let mut from = 0;
    (1..points.len()).filter_map(move |i| {
        let to = points[i];
        let start = points[from];
        if let Some(&next) = points.get(i + 1) {
            let between = |a: f64, b: f64, c: f64| a.min(c) < b && b < a.max(c);
            if (start.0 == to.0 && to.0 == next.0 && between(start.1, to.1, next.1))
                || (start.1 == to.1 && to.1 == next.1 && between(start.0, to.0, next.0))
            {
                return None;
            }
        }
        from = i;
        Some((start, to))
    })
}

pub(super) fn wire_crossings(
    points: &[Point],
    ignore_root: Option<NetRoot>,
    wires: &[Wire],
    wire_roots: &[NetRoot],
) -> Vec<(Point, NetRoot)> {
    let mut crossings = BTreeMap::new();
    for (a, b) in straight_spans(points) {
        let mut hits = BTreeMap::new();
        for (wire, &other_root) in wires.iter().zip(wire_roots) {
            // A dotted junction already belongs to the run's net in NetGraph.
            if ignore_root == Some(other_root) {
                continue;
            }
            let Some(((x, y), sides)) = crossing_sides(a, b, wire) else {
                continue;
            };
            let hit = hits.entry((point(x, y), other_root)).or_insert(((x, y), 0));
            hit.1 |= sides;
        }
        crossings.extend(
            hits.into_iter()
                .filter(|(_, (_, sides))| *sides == 0b11)
                .map(|(key, (at, _))| (key, (at, key.1))),
        );
    }
    crossings.into_values().collect()
}

#[derive(Default)]
struct NetEndpoints<'a> {
    pins: Vec<Pin>,
    labels: Vec<&'a Label>,
    has_sheet_or_bus_endpoint: bool,
}

struct Pin {
    at: Point,
    details: Value,
}

impl NetEndpoints<'_> {
    fn pin_details(&self, endpoint: Point) -> Vec<Value> {
        self.pins
            .iter()
            .filter(|pin| same_point(pin.at, endpoint))
            .map(|pin| &pin.details)
            .cloned()
            .collect()
    }

    fn label_details(&self, endpoint: Point) -> Vec<Value> {
        self.labels
            .iter()
            .filter(|label| same_point((label.x, label.y), endpoint))
            .map(|label| label_details(label))
            .collect()
    }

    fn label_choice(&self, reused: &[Option<&Label>; 2]) -> Value {
        if self.has_sheet_or_bus_endpoint {
            return json!({ "action": "unsupported_scope_or_aliases" });
        }
        let Some(first) = self.labels.first() else {
            return json!({
                "action": "choose_name",
                "changes_netlist_name": true,
                "labels_to_find_added": 2
            });
        };
        if !super::label_edit::ordinary_name(&first.net)
            || self
                .labels
                .iter()
                .any(|label| label.kind != LabelKind::NetLabel || label.net != first.net)
        {
            return json!({ "action": "unsupported_scope_or_aliases" });
        }
        json!({
            "action": "reuse_local_label",
            "text": first.net,
            "labels_to_find_added": reused.iter().filter(|anchor| anchor.is_none()).count(),
        })
    }
}

fn same_point((x, y): Point, (a, b): Point) -> bool {
    point(x, y) == point(a, b)
}

fn label_details(label: &Label) -> Value {
    let kind = match label.kind {
        LabelKind::NetLabel => "local",
        LabelKind::GlobalLabel => "global",
        LabelKind::HierarchicalLabel => "hierarchical",
        LabelKind::PowerSymbol => "power",
    };
    json!({
        "text": label.net,
        "kind": kind,
        "uuid": label.uuid,
        "x": label.x,
        "y": label.y,
    })
}

fn net_endpoints<'a>(
    tree: &SexpNode,
    graph: &mut NetGraph,
    wire_roots: &[NetRoot],
    labels: &'a [Label],
) -> BTreeMap<NetRoot, NetEndpoints<'a>> {
    let mut endpoints = BTreeMap::<NetRoot, NetEndpoints>::new();
    let roots: BTreeSet<_> = wire_roots.iter().copied().collect();
    for (instance, pins) in drawn_symbol_pin_inventory(tree).into_iter().flatten() {
        for (pin, transform) in pins {
            let (x, y) = pin_endpoint(&pin, transform);
            let root = graph.root_at(x, y);
            if roots.contains(&root) {
                endpoints.entry(root).or_default().pins.push(Pin {
                    at: (x, y),
                    details: json!({
                        "reference": instance.reference,
                        "pin": pin.number,
                        "symbol_uuid": instance.uuid,
                        "unit": instance.unit,
                        "x": x,
                        "y": y
                    }),
                });
            }
        }
    }
    for (root, label) in label_roots(graph, labels) {
        if roots.contains(&root) {
            endpoints.entry(root).or_default().labels.push(label);
        }
    }
    let buses = extract_buses(tree);
    for (x, y) in extract_sheet_pins(tree)
        .into_iter()
        .chain(native_bus_entry_wire_ends(tree, &buses))
    {
        let root = graph.root_at(x, y);
        if roots.contains(&root) {
            endpoints.entry(root).or_default().has_sheet_or_bus_endpoint = true;
        }
    }
    endpoints
}

pub(super) fn report(
    request: &Request,
    tree: &SexpNode,
    measured: &WireRuns,
    detour_mm: f64,
) -> Result<Vec<Value>, &'static str> {
    let runs = request.select(measured)?;
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let wires = &measured.wires;
    let mut graph = net_graph_for(tree, wires, &measured.labels);
    let wire_roots: Vec<_> = wires.iter().map(|w| graph.root_at(w.x1, w.y1)).collect();
    let endpoints = net_endpoints(tree, &mut graph, &wire_roots, &measured.labels);
    let hidden_power = super::hidden_power_roots(tree, &mut graph);
    let junctions = copperpour_sexp::schematic::extract_junctions(tree);
    let has_variables = super::label_edit::variable_labels(&measured.labels);
    let no_endpoints = NetEndpoints::default();

    Ok(runs
        .iter()
        .map(|run| {
            let first = &run.segments[0];
            let root = wire_roots[first.wire];
            let mut points = vec![mm(first.a)];
            points.extend(run.segments.iter().map(|edge| mm(edge.b)));
            let start = points[0];
            let end = *points.last().unwrap();
            let endpoints = endpoints.get(&root).unwrap_or(&no_endpoints);
            let endpoint = |point: Point, status| {
                json!({
                    "x": point.0, "y": point.1, "status": status,
                    "pins": endpoints.pin_details(point),
                    "label_anchors": endpoints.label_details(point),
                })
            };
            let crossings: Vec<_> = wire_crossings(&points, Some(root), wires, &wire_roots)
                .into_iter()
                .map(|((x, y), other_root)| json!({ "x": x, "y": y, "net": graph.name_of_root(other_root) }))
                .collect();
            let blocks: Vec<_> = request
                .blocks
                .iter()
                .filter(|block| block.bounds.crosses(&points))
                .map(|block| &block.name)
                .collect();
            let other_pins: Vec<_> = endpoints
                .pins
                .iter()
                .filter(|pin| !same_point(pin.at, start) && !same_point(pin.at, end))
                .map(|pin| &pin.details)
                .collect();
            let other_labels: Vec<_> = endpoints
                .labels
                .iter()
                .filter(|label| !same_point((label.x, label.y), start) && !same_point((label.x, label.y), end))
                .map(|label| label_details(label))
                .collect();
            let whole_wires = run.uses_whole_wires(wires);
            let reused = if endpoints.labels.is_empty() || !whole_wires {
                [None, None]
            } else {
                super::label_edit::reusable_anchors(tree, measured, run, &endpoints.labels, None)
            };
            let label_choice = if hidden_power.contains_key(&root) || has_variables {
                json!({"action":"unsupported_scope_or_aliases"})
            } else {
                endpoints.label_choice(&reused)
            };
            let (recommendation, reason) = if !measured.pin_geometry_resolved {
                ("keep_wire", "unresolved_pins")
            } else if run.start == "branch" || run.end == "branch" || endpoints.pins.len() > 2 {
                ("keep_wire", "keep_visible_trunk")
            } else if label_choice["action"] == "unsupported_scope_or_aliases" {
                ("keep_wire", "unsupported_scope_or_aliases")
            } else if !whole_wires {
                ("keep_wire", "interior_tap")
            } else if [start, end].into_iter().any(|end| {
                super::label_edit::cut_end_issue(
                    end,
                    endpoints.pins.iter().any(|pin| same_point(pin.at, end)),
                    &junctions,
                    wires,
                    |wire| run.segments.iter().any(|edge| std::ptr::eq(&wires[edge.wire], wire)),
                    &measured.labels,
                ).is_some()
            }) {
                ("keep_wire", "dangling_cut_end")
            } else if run.start != "attached" || run.end != "attached"
                || endpoints.pins.len() < 2
            {
                ("keep_wire", "unproven_endpoints")
            } else if run.length_mm < request.long_run_mm {
                ("keep_wire", "below_long_run_mm")
            } else if run.bend_count > 0 && run.extra_length_mm >= detour_mm {
                ("reroute_first", "orthogonal_detour")
            } else if crossings.is_empty() && blocks.is_empty() {
                ("keep_wire", "no_foreign_crossings_or_blocks")
            } else if crossings.is_empty() {
                ("consider_labels", "avoid_unrelated_block")
            } else {
                ("consider_labels", "remove_crossings")
            };
            let anchor_estimate = (recommendation == "consider_labels").then(|| {
                let anchors: [Point; 2] = std::array::from_fn(|i| {
                    reused[i].map(|label| (label.x, label.y)).unwrap_or_else(|| {
                        let (end, toward) = if i == 0 {
                            (start, points[1])
                        } else {
                            (end, points[points.len() - 2])
                        };
                        super::label_edit::default_stub_anchor(end, toward)
                    })
                });
                json!({
                    "anchors": anchors,
                    "distance_mm": (anchors[0].0 - anchors[1].0).hypot(anchors[0].1 - anchors[1].1),
                    "clearance_checked": false
                })
            });
            json!({
                "wire_uuids": wire_uuids(run, wires),
                "start": endpoint(start, run.start),
                "end": endpoint(end, run.end),
                "length_mm": run.length_mm,
                "direct_orthogonal_mm": run.direct_orthogonal_mm,
                "extra_length_mm": run.extra_length_mm,
                "bend_count": run.bend_count,
                "foreign_crossings": crossings,
                "crossed_blocks": blocks,
                "other_pins": other_pins,
                "other_label_anchors": other_labels,
                "run_endpoint_distance_mm": (start.0 - end.0).hypot(start.1 - end.1),
                "label_anchor_estimate": anchor_estimate,
                "label_choice": label_choice,
                "recommendation": recommendation,
                "reason": reason,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::sch_wire_runs::measure_wire_runs;
    use copperpour_sexp::parse_sexp;

    const CROSSING: &str =
        include_str!("../../../tests/fixtures/labels_vs_wires_kicad10/crossing/crossing.kicad_sch");
    const CROSSING_WIRE: &str = "0bb4199c-0170-4fa5-beb8-87ea04205c3a";

    fn wire((x1, y1): Point, (x2, y2): Point) -> Wire {
        Wire {
            x1,
            y1,
            x2,
            y2,
            uuid: None,
        }
    }

    #[test]
    fn a_straight_crossing_survives_wire_boundaries_on_either_net() {
        let run = [(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)];
        let wires = [wire((5.0, -5.0), (5.0, 0.0)), wire((5.0, 0.0), (5.0, 5.0))];
        assert_eq!(
            wire_crossings(&run, Some((0, 0)), &wires, &[(1, 1), (1, 1)]),
            [((5.0, 0.0), (1, 1))]
        );
    }

    #[test]
    fn a_foreign_wire_end_is_not_a_crossing() {
        let run = [(0.0, 0.0), (10.0, 0.0)];
        let wires = [wire((5.0, -5.0), (5.0, 0.0))];
        assert!(wire_crossings(&run, Some((0, 0)), &wires, &[(1, 1)]).is_empty());
    }

    #[test]
    fn a_crossing_on_the_same_net_is_not_foreign() {
        let run = [(0.0, 0.0), (10.0, 0.0)];
        let wires = [wire((5.0, -5.0), (5.0, 5.0))];
        assert!(wire_crossings(&run, Some((0, 0)), &wires, &[(0, 0)]).is_empty());
        assert_eq!(wire_crossings(&run, None, &wires, &[(0, 0)]).len(), 1);
    }

    #[test]
    fn a_block_passage_is_not_a_tangent_or_a_link_to_that_block() {
        let block = Bounds {
            min: (3.0, 3.0),
            max: (7.0, 7.0),
        };
        assert!(block.crosses(&[(0.0, 5.0), (10.0, 5.0)]));
        assert!(block.intersects((0.0, 3.0), (10.0, 3.0)));
        assert!(!block.crosses(&[(0.0, 3.0), (10.0, 3.0)]));
        assert!(!block.crosses(&[(0.0, 5.0), (5.0, 5.0)]));
        assert!(!block.crosses(&[(5.0, 5.0), (10.0, 5.0)]));
    }

    #[test]
    fn crossing_fixture_reports_what_a_reader_gains_and_has_to_look_up() {
        let tree = parse_sexp(CROSSING).unwrap();
        let request = Request::parse(&json!({
            "long_run_mm": 100.0,
            "wire_uuids": [CROSSING_WIRE],
            "blocks": [{
                "name": "divider",
                "bounds": { "min": [108.0, 92.0], "max": [132.0, 100.0] }
            }]
        }))
        .unwrap();
        let reports = report(&request, &tree, &measure_wire_runs(&tree).unwrap(), 10.0).unwrap();
        assert_eq!(reports.len(), 1);
        let run = &reports[0];
        assert_eq!(run["wire_uuids"], json!([CROSSING_WIRE]));
        assert_eq!(run["length_mm"], 143.51);
        assert_eq!(run["direct_orthogonal_mm"], 143.51);
        assert_eq!(run["extra_length_mm"], 0.0);
        assert_eq!(run["bend_count"], 0);
        assert_eq!(run["start"]["pins"][0]["reference"], "J1");
        assert_eq!(run["start"]["pins"][0]["pin"], "2");
        assert_eq!(run["start"]["label_anchors"][0]["text"], "SIG_A");
        assert_eq!(run["end"]["pins"][0]["reference"], "R3");
        assert_eq!(run["end"]["pins"][0]["pin"], "1");
        assert_eq!(
            run["foreign_crossings"],
            json!([
                { "x": 114.3, "y": 96.52, "net": "+5V" },
                { "x": 127.0, "y": 96.52, "net": null }
            ])
        );
        assert_eq!(run["crossed_blocks"], json!(["divider"]));
        assert_eq!(run["recommendation"], "consider_labels");
        assert_eq!(run["other_pins"], json!([]));
        assert!((run["run_endpoint_distance_mm"].as_f64().unwrap() - 143.51).abs() < 1e-9);
        assert_eq!(
            run["label_anchor_estimate"]["anchors"],
            json!([[55.88, 96.52], [196.85, 96.52]])
        );
        assert_eq!(
            run["label_choice"],
            json!({"action":"reuse_local_label","text":"SIG_A","labels_to_find_added":1})
        );
    }

    #[test]
    fn the_analog_feedback_run_stays_wired_even_at_a_low_threshold() {
        let tree = parse_sexp(include_str!(
            "../../../tests/fixtures/labels_vs_wires_kicad10/analog/analog.kicad_sch"
        ))
        .unwrap();
        let request = Request::parse(&json!({
            "long_run_mm": 1.0,
            "wire_uuids": ["2c916bc8-ab76-48b2-8781-3529b4c4a9e0", "893a179b-551c-4296-93d4-3e0cc6c4ab00"]
        })).unwrap();
        let measured = measure_wire_runs(&tree).unwrap();
        let reports = report(&request, &tree, &measured, 10.0).unwrap();
        let run = &reports[0];
        assert_eq!(run["length_mm"], 13.97);
        assert_eq!(run["bend_count"], 1);
        assert_eq!(run["recommendation"], "keep_wire");
        assert_eq!(run["reason"], "keep_visible_trunk");
        let region = Request::parse(&json!({
            "long_run_mm": 1.0,
            "region":{"min":[90.0,70.0],"max":[180.0,120.0]}
        }))
        .unwrap();
        let reports = report(&region, &tree, &measured, 10.0).unwrap();
        assert!(reports.iter().any(|report| report == run));
        assert!(reports
            .iter()
            .all(|report| report["recommendation"] != "consider_labels"));
    }

    #[test]
    fn advice_parser_refuses_invalid_selectors_and_blocks() {
        let bounds = json!({"min":[0,0],"max":[1,1]});
        for (fields, reason) in [
            (
                json!({"region":{"min":[1,0],"max":[0,1]}}),
                "region min must be below max on both axes",
            ),
            (
                json!({"region":bounds,"blocks":[{"name":" ","bounds":bounds}]}),
                "blocks need a name and min below max on both axes",
            ),
        ] {
            let mut args = fields;
            args["long_run_mm"] = json!(100);
            assert_eq!(Request::parse(&args).err(), Some(reason), "{args}");
        }
    }

    #[test]
    fn advice_applies_its_own_inclusive_length_limit() {
        let tree = parse_sexp(CROSSING).unwrap();
        let measured = measure_wire_runs(&tree).unwrap();
        let check = |length| {
            let request = Request::parse(&json!({
                "long_run_mm": length,
                "wire_uuids": [CROSSING_WIRE],
            }))
            .unwrap();
            report(&request, &tree, &measured, 10.0).unwrap().remove(0)
        };
        assert_eq!(check(143.51)["recommendation"], "consider_labels");
        assert_eq!(check(143.52)["reason"], "below_long_run_mm");
    }

    #[test]
    fn a_block_passage_is_evidence_even_if_no_other_net_crosses() {
        let tree = parse_sexp(include_str!(
            "../../../tests/fixtures/labels_vs_wires_kicad10/names/names.kicad_sch"
        ))
        .unwrap();
        let measured = measure_wire_runs(&tree).unwrap();
        let mut arguments = json!({
            "long_run_mm": 20.0,
            "wire_uuids": ["566e47a9-13f9-4bfe-8770-9f907db66b32"],
        });
        let check = |arguments: &Value| {
            let request = Request::parse(arguments).unwrap();
            report(&request, &tree, &measured, 10.0).unwrap().remove(0)
        };
        assert_eq!(
            check(&arguments)["reason"],
            "no_foreign_crossings_or_blocks"
        );
        arguments["blocks"] = json!([{
            "name": "unrelated",
            "bounds": {"min":[90.0,48.0],"max":[100.0,53.0]}
        }]);
        let report = check(&arguments);
        assert_eq!(report["recommendation"], "consider_labels");
        assert_eq!(report["reason"], "avoid_unrelated_block");
        assert_eq!(report["foreign_crossings"], json!([]));
        assert_eq!(report["label_choice"]["action"], "choose_name");
        assert_eq!(report["label_choice"]["changes_netlist_name"], true);
    }

    #[test]
    fn a_detour_through_a_block_prompts_rerouting_first() {
        let tree = parse_sexp(include_str!(
            "../../../tests/fixtures/schematic_wire_runs_kicad10.kicad_sch"
        ))
        .unwrap();
        let measured = measure_wire_runs(&tree).unwrap();
        let request = Request::parse(&json!({
            "long_run_mm":100.0,
            "region":{"min":[50.0,76.0],"max":[60.0,77.0]},
            "blocks":[{"name":"unrelated","bounds":{"min":[50.0,75.0],"max":[60.0,77.0]}}]
        }))
        .unwrap();
        let reports = report(&request, &tree, &measured, 50.8).unwrap();
        let run = reports
            .iter()
            .find(|report| report["length_mm"] == 119.38)
            .unwrap();
        assert_eq!(run["crossed_blocks"], json!(["unrelated"]));
        assert_eq!(run["recommendation"], "reroute_first");
    }
}
