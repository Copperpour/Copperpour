//! Advisory checks for the drawn route, not the electrically merged net.

use super::{
    drawn_symbol_pin_inventory, get_path, opt_positive_f64,
    sch_connectivity::{
        bus_entry_wire_ends, wire_run_net_graph, PinNetNames, UnitNotation, WireIndex,
        COINCIDENT_TOLERANCE,
    },
    ToolDef,
};
use crate::{
    mcp::{error::ToolErrorKind, protocol::CallToolResult},
    tool,
};
use copperpour_sexp::{
    schematic::{
        extract_buses, extract_junctions, extract_labels, extract_no_connects, extract_sheet_pins,
        extract_wires, find_lib_symbol, parse_at, pin_endpoint, read_schematic, Label, LabelKind,
        Wire,
    },
    SexpError, SexpNode,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const LONG_MM: f64 = 100.0;
const DETOUR_MM: f64 = 10.0;
const NM_PER_MM: f64 = 1_000_000.0;
type Point = (i64, i64);

pub(super) fn tool_def() -> ToolDef {
    tool!(
        "check_schematic_wire_runs",
        "Report long or indirect branch-free orthogonal wire runs on a saved sheet. Includes \
         lengths in mm, bends, endpoint status, and net names where known. Makes no edits.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch file" },
                "long_run_mm": { "type": "number", "default": LONG_MM, "description": "Minimum total length in mm for a long-run finding; must be positive" },
                "detour_mm": { "type": "number", "default": DETOUR_MM, "description": "Minimum extra length in mm over the direct orthogonal distance; must be positive" }
            },
            "required": ["schematic"]
        }),
        |args, _ctx| async move { handle_check_schematic_wire_runs(args) }
    )
}

fn mm((x, y): Point) -> (f64, f64) {
    (x as f64 / NM_PER_MM, y as f64 / NM_PER_MM)
}

fn point(x: f64, y: f64) -> Point {
    (
        (x * NM_PER_MM).round() as i64,
        (y * NM_PER_MM).round() as i64,
    )
}

fn coordinate_is_representable(n: f64) -> bool {
    // Leave room for the neighboring spatial buckets at the 0.5 nm bus tolerance.
    n.abs() < i64::MAX as f64 / (4.0 * NM_PER_MM)
}

fn wire_is_representable(wire: &Wire) -> bool {
    [wire.x1, wire.y1, wire.x2, wire.y2]
        .into_iter()
        .all(coordinate_is_representable)
}

fn distance(a: Point, b: Point) -> u128 {
    u128::from(a.0.abs_diff(b.0)) + u128::from(a.1.abs_diff(b.1))
}

struct Edge {
    a: Point,
    b: Point,
    wire: usize,
}

pub(crate) struct WireRun {
    segments: Vec<Edge>,
    start: &'static str,
    end: &'static str,
    pub(crate) length_mm: f64,
    direct_orthogonal_mm: f64,
    extra_length_mm: f64,
    bend_count: usize,
}

pub(crate) struct WireRuns {
    pub(crate) runs: Vec<WireRun>,
    wires: Vec<Wire>,
    labels: Vec<Label>,
    skipped_wire_count: usize,
    skipped_closed_segment_count: usize,
    pin_geometry_resolved: bool,
}

impl Edge {
    fn other(&self, p: Point) -> Point {
        if p == self.a {
            self.b
        } else {
            self.a
        }
    }
}

/// Split only where KiCad attaches labels or junctions to a wire's interior.
/// A bare crossing or endpoint landing mid-wire is not an attachment.
fn split_points(wires: &[Wire], taps: impl Iterator<Item = (f64, f64)>) -> Vec<Vec<Point>> {
    let index = WireIndex::build(wires, COINCIDENT_TOLERANCE);
    let ordinals: HashMap<*const Wire, usize> = wires
        .iter()
        .enumerate()
        .map(|(i, wire)| (wire as *const _, i))
        .collect();
    let mut cuts = vec![Vec::new(); wires.len()];
    for (x, y) in taps {
        let tap = point(x, y);
        for wire in index.interior_hits(x, y, point) {
            cuts[ordinals[&(wire as *const _)]].push(tap);
        }
    }
    cuts
}

fn handle_check_schematic_wire_runs(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let threshold = |key, default| opt_positive_f64(args, key).map(|v| v.unwrap_or(default));
    let (long_mm, detour_mm) = match (
        threshold("long_run_mm", LONG_MM),
        threshold("detour_mm", DETOUR_MM),
    ) {
        (Ok(long), Ok(detour)) => (long, detour),
        (Err(error), _) | (_, Err(error)) => return Ok(error),
    };
    let malformed = |reason: String| {
        let message = format!("Cannot check {}: {reason}", path.display());
        CallToolResult::error_kind(
            ToolErrorKind::MalformedFile {
                path: path.display().to_string(),
                reason,
            },
            message,
        )
    };
    let tree = match read_schematic(&path) {
        Ok((_, tree)) => tree,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&path),
                format!("Schematic not found: {}", path.display()),
            ));
        }
        // `read_to_string` reports a file that is not UTF-8 as invalid data.
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Ok(malformed(error.to_string()));
        }
        Err(error @ SexpError::Parse { .. }) => return Ok(malformed(error.to_string())),
        Err(error) => return Err(error.into()),
    };
    if tree.head() != Some("kicad_sch") {
        return Ok(malformed("expected a KiCad schematic sheet".to_owned()));
    }
    let WireRuns {
        runs,
        wires,
        labels,
        skipped_wire_count,
        skipped_closed_segment_count,
        pin_geometry_resolved,
    } = match measure_wire_runs(&tree) {
        Ok(report) => report,
        Err(reason) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::MalformedFile {
                    path: path.display().to_string(),
                    reason: reason.to_owned(),
                },
                format!("Cannot check {}: {reason}", path.display()),
            ));
        }
    };
    let run_count = runs.len();
    let mut findings = Vec::new();
    let mut names = None;
    for run in runs {
        let mut reasons = Vec::new();
        if run.length_mm >= long_mm {
            reasons.push("long_run");
        }
        if run.extra_length_mm >= detour_mm {
            reasons.push("orthogonal_detour");
        }
        if reasons.is_empty() {
            continue;
        }
        let (net_graph, pin_names) = names.get_or_insert_with(|| {
            let mut graph = wire_run_net_graph(&tree, &wires, &labels, point);
            let pin_names = pin_geometry_resolved
                .then(|| PinNetNames::build(&tree, &mut graph, UnitNotation::for_schematic(&path)));
            (graph, pin_names)
        });
        let first = &run.segments[0];
        let last = run.segments.last().unwrap();
        let source_wire = &wires[first.wire];
        let root = net_graph.root_at(source_wire.x1, source_wire.y1);
        let net = net_graph
            .name_of_root(root)
            .or_else(|| pin_names.as_ref()?.pin_name_of_root(root));
        let endpoint = |p: Point, status| {
            let (x, y) = mm(p);
            json!({ "x": x, "y": y, "status": status })
        };
        let segments: Vec<_> = run
            .segments
            .iter()
            .map(|edge| {
                let ((x1, y1), (x2, y2)) = (mm(edge.a), mm(edge.b));
                json!({"wire_uuid": wires[edge.wire].uuid, "x1": x1, "y1": y1, "x2": x2, "y2": y2})
            })
            .collect();
        findings.push(json!({
            "start": endpoint(first.a, run.start),
            "end": endpoint(last.b, run.end),
            "net": net,
            "length_mm": run.length_mm,
            "direct_orthogonal_mm": run.direct_orthogonal_mm,
            "extra_length_mm": run.extra_length_mm,
            "bend_count": run.bend_count,
            "segments": segments,
            "reasons": reasons
        }));
    }
    Ok(CallToolResult::json(&json!({
        "schematic": path,
        "source": "saved_file",
        "thresholds": { "long_run_mm": long_mm, "detour_mm": detour_mm },
        "run_count": run_count,
        "finding_count": findings.len(),
        "findings": findings,
        "skipped_wire_count": skipped_wire_count,
        "skipped_closed_segment_count": skipped_closed_segment_count,
        "pin_geometry_resolved": pin_geometry_resolved
    })))
}

/// Measure all runs on one saved sheet, before applying advisory thresholds.
pub(crate) fn measure_wire_runs(tree: &SexpNode) -> Result<WireRuns, &'static str> {
    let anchors = [
        "label",
        "global_label",
        "hierarchical_label",
        "junction",
        "no_connect",
    ]
    .into_iter()
    .flat_map(|kind| tree.find_all(kind))
    .chain(
        tree.find_all("sheet")
            .into_iter()
            .flat_map(|s| s.find_all("pin")),
    );
    if anchors.into_iter().any(|anchor| {
        !parse_at(anchor).is_some_and(|(x, y, _)| {
            coordinate_is_representable(x) && coordinate_is_representable(y)
        })
    }) {
        return Err("one or more wire attachments have unreadable positions");
    }
    let mut wires = extract_wires(tree);
    wires.retain(wire_is_representable);
    let buses: Vec<_> = extract_buses(tree)
        .into_iter()
        .filter(wire_is_representable)
        .collect();
    let mut labels = extract_labels(tree);
    let junctions = extract_junctions(tree);
    let cuts = split_points(
        &wires,
        junctions
            .iter()
            .copied()
            .chain(labels.iter().map(|l| (l.x, l.y))),
    );
    let mut attached: BTreeSet<Point> = junctions
        .into_iter()
        .chain(labels.iter().map(|label| (label.x, label.y)))
        .chain(extract_sheet_pins(tree))
        .chain(
            bus_entry_wire_ends(tree, &buses, 0.5 / NM_PER_MM)
                .into_iter()
                .filter(|(x, y)| {
                    coordinate_is_representable(*x) && coordinate_is_representable(*y)
                }),
        )
        .map(|(x, y)| point(x, y))
        .collect();
    let no_connects: BTreeSet<Point> = extract_no_connects(tree)
        .into_iter()
        .map(|(x, y)| point(x, y))
        .collect();
    let pin_inventory = drawn_symbol_pin_inventory(tree);
    let mut pins_resolved = pin_inventory.iter().all(Option::is_some);
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|n| n.find_all("symbol"))
        .unwrap_or_default();
    for (instance, pins) in pin_inventory.into_iter().flatten() {
        let names_net = !instance.value.is_empty()
            && find_lib_symbol(&lib_symbols, &instance)
                .is_some_and(|lib| lib.find("power").is_some());
        for (pin, transform) in pins {
            let (x, y) = pin_endpoint(&pin, transform);
            if !coordinate_is_representable(x) || !coordinate_is_representable(y) {
                pins_resolved = false;
                continue;
            }
            attached.insert(point(x, y));
            if names_net && pin.electrical_type == "power_in" {
                labels.push(Label {
                    kind: LabelKind::PowerSymbol,
                    net: instance.value.clone(),
                    x,
                    y,
                    rotation: instance.rotation,
                    uuid: instance.uuid.clone(),
                });
            }
        }
    }
    let mut skipped_wire_count = tree.find_all("wire").len().saturating_sub(wires.len());
    let mut unsupported_ends = BTreeSet::new();
    let mut diagonal_degree = BTreeMap::<Point, usize>::new();
    let mut edges = Vec::new();
    let mut adjacent: BTreeMap<Point, Vec<usize>> = BTreeMap::new();
    for (wire, (w, mut points)) in wires.iter().zip(cuts).enumerate() {
        let a = point(w.x1, w.y1);
        let b = point(w.x2, w.y2);
        if a == b || (a.0 != b.0 && a.1 != b.1) {
            unsupported_ends.extend([a, b]);
            if a != b {
                for endpoint in [a, b] {
                    *diagonal_degree.entry(endpoint).or_default() += 1;
                }
            }
            skipped_wire_count += 1;
            continue;
        }
        points.extend([a, b]);
        points.sort_unstable();
        points.dedup();
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            for point in [a, b] {
                adjacent.entry(point).or_default().push(edges.len());
            }
            edges.push(Edge { a, b, wire });
        }
    }
    let stops = |p: Point| {
        adjacent[&p].len() != 2
            || unsupported_ends.contains(&p)
            || no_connects.contains(&p)
            || attached.contains(&p)
    };
    let endpoint = |p: Point| {
        let degree = adjacent[&p].len() + diagonal_degree.get(&p).copied().unwrap_or(0);
        if degree > 2 {
            "branch"
        } else if no_connects.contains(&p) {
            "marked_no_connect"
        } else if attached.contains(&p) || degree == 2 {
            "attached"
        } else if pins_resolved && !unsupported_ends.contains(&p) {
            "open"
        } else {
            "unknown"
        }
    };
    let mut used = vec![false; edges.len()];
    let mut runs = Vec::new();
    let mut closed_edge_count = 0;
    for (&start, incident) in &adjacent {
        if !stops(start) {
            continue;
        }
        for &first in incident {
            if used[first] {
                continue;
            }
            let (mut p, mut edge_id) = (start, first);
            let mut segments = Vec::new();
            let mut length = 0;
            let mut bends = 0;
            let mut last_vertical = None;
            loop {
                used[edge_id] = true;
                let edge = &edges[edge_id];
                let q = edge.other(p);
                let vertical = p.0 == q.0;
                if last_vertical.is_some_and(|v| v != vertical) {
                    bends += 1;
                }
                last_vertical = Some(vertical);
                length += distance(p, q);
                segments.push(Edge {
                    a: p,
                    b: q,
                    wire: edge.wire,
                });
                p = q;
                if stops(p) {
                    break;
                }
                let Some(&next) = adjacent[&p].iter().find(|&&i| !used[i]) else {
                    break;
                };
                edge_id = next;
            }
            if start == p {
                closed_edge_count += segments.len();
                continue;
            }
            let direct = distance(start, p);
            runs.push(WireRun {
                segments,
                start: endpoint(start),
                end: endpoint(p),
                length_mm: length as f64 / NM_PER_MM,
                direct_orthogonal_mm: direct as f64 / NM_PER_MM,
                extra_length_mm: (length - direct) as f64 / NM_PER_MM,
                bend_count: bends,
            });
        }
    }
    closed_edge_count += used.into_iter().filter(|used| !used).count();
    Ok(WireRuns {
        runs,
        wires,
        labels,
        skipped_wire_count,
        skipped_closed_segment_count: closed_edge_count,
        pin_geometry_resolved: pins_resolved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use copperpour_sexp::parse_sexp;
    use std::{fs, path::Path};

    const FIXTURE: &[u8] =
        include_bytes!("../../tests/fixtures/schematic_wire_runs_kicad10.kicad_sch");
    const MIXED_POWER: &[u8] =
        include_bytes!("../../tests/fixtures/schematic_wire_runs_mixed-power.kicad_sch");

    async fn served(
        handler: &crate::mcp::handler::McpHandler,
        name: &str,
        arguments: Value,
    ) -> (Value, Value) {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        (result, body)
    }

    async fn fixture_call(path: &Path, arguments: Value, gateway: bool) -> (Value, Value) {
        call_on_fixture(path, FIXTURE, arguments, gateway).await
    }

    async fn call_on_fixture(
        path: &Path,
        bytes: &[u8],
        mut arguments: Value,
        gateway: bool,
    ) -> (Value, Value) {
        fs::write(path, bytes).unwrap();
        arguments["schematic"] = json!(path);
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            eager_toolsets: !gateway,
            ..Default::default()
        })
        .await
        .unwrap();
        let reply = if gateway {
            assert!(!handler.call_tool_writes_allowed());
            assert_wire_run_tool_not_listed(&handler).await;
            let result = served(
                &handler,
                "call_tool",
                json!({
                    "name": "check_schematic_wire_runs", "arguments": arguments
                }),
            )
            .await;
            assert_wire_run_tool_not_listed(&handler).await;
            result
        } else {
            served(&handler, "check_schematic_wire_runs", arguments).await
        };
        assert_eq!(fs::read(path).unwrap(), bytes);
        reply
    }

    async fn assert_wire_run_tool_not_listed(handler: &crate::mcp::handler::McpHandler) {
        let response = handler
            .handle_message(json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))
            .await
            .unwrap();
        let result = response.result.unwrap();
        assert!(result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "check_schematic_wire_runs"));
    }

    #[tokio::test]
    async fn wire_runs_report_meander_and_long_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.kicad_sch");
        let (result, body) = fixture_call(&path, json!({}), false).await;
        let (gateway_result, gateway_body) = fixture_call(&path, json!({}), true).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_ne!(gateway_result["isError"], true, "{gateway_result}");
        assert_eq!(body, gateway_body);
        assert_eq!(body["source"], "saved_file");
        assert_eq!(
            body["thresholds"],
            json!({"long_run_mm":100.0,"detour_mm":10.0})
        );
        assert_eq!(body["pin_geometry_resolved"], true);
        assert_eq!(body["run_count"], 3);
        assert_eq!(body["finding_count"], 2);
        assert_eq!(body["skipped_wire_count"], 0);
        let findings = body["findings"].as_array().unwrap();
        let meander = findings
            .iter()
            .find(|f| f["net"] == "Net-(R1-Pad2)")
            .unwrap();
        assert_eq!(meander["length_mm"], 119.38);
        assert_eq!(meander["direct_orthogonal_mm"], 68.58);
        assert_eq!(meander["extra_length_mm"], 50.8);
        assert_eq!(meander["bend_count"], 4);
        assert_eq!(
            meander["segments"],
            json!([
                {"wire_uuid":"10a1cb4d-5282-4b58-814f-e6f9336c2a0a","x1":29.21,"y1":50.8,"x2":38.1,"y2":50.8},
                {"wire_uuid":"bf3e4edd-6c21-4a15-823b-5227fc0a8a95","x1":38.1,"y1":50.8,"x2":38.1,"y2":76.2},
                {"wire_uuid":"edaf526f-d4eb-4fc5-85ae-142104854802","x1":38.1,"y1":76.2,"x2":88.9,"y2":76.2},
                {"wire_uuid":"a85a95a2-c9f8-4c0c-ac45-281864cfbd0a","x1":88.9,"y1":76.2,"x2":88.9,"y2":50.8},
                {"wire_uuid":"09104ba8-a23c-49b2-a5fa-1ab70492d062","x1":88.9,"y1":50.8,"x2":97.79,"y2":50.8}
            ])
        );
        assert_eq!(meander["reasons"], json!(["long_run", "orthogonal_detour"]));
        assert_eq!(
            meander["start"],
            json!({"x":29.21,"y":50.8,"status":"attached"})
        );
        assert_eq!(
            meander["end"],
            json!({"x":97.79,"y":50.8,"status":"attached"})
        );
        let straight = findings.iter().find(|f| f["net"] == "LONG").unwrap();
        assert_eq!(straight["length_mm"], 228.6);
        assert_eq!(straight["extra_length_mm"], 0.0);
        assert_eq!(straight["reasons"], json!(["long_run"]));
        assert_eq!(straight["start"]["status"], "open");
        assert_eq!(straight["end"]["status"], "attached");
    }

    #[tokio::test]
    async fn wire_runs_threshold_overrides_are_inclusive_and_independent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.kicad_sch");
        let (result, body) =
            fixture_call(&path, json!({"long_run_mm": 200, "detour_mm": 51}), false).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(body["run_count"], 3);
        assert_eq!(body["finding_count"], 1);
        assert_eq!(body["findings"][0]["net"], "LONG");

        let (result, body) =
            fixture_call(&path, json!({"long_run_mm": 200, "detour_mm": 50.8}), false).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(body["finding_count"], 2);
        let meander = body["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["net"] == "Net-(R1-Pad2)")
            .unwrap();
        assert_eq!(meander["reasons"], json!(["orthogonal_detour"]));

        let (result, body) = fixture_call(&path, json!({"long_run_mm": 12.7}), false).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(body["finding_count"], 3);
        let stub = body["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["start"]["y"] == 127.0)
            .unwrap();
        assert_eq!(stub["length_mm"], 12.7);
        assert_eq!(stub["reasons"], json!(["long_run"]));
        assert_eq!(stub["start"]["status"], "open");
        assert_eq!(stub["end"]["status"], "open");
        assert!(stub["net"].is_null());
    }

    #[tokio::test]
    async fn wire_runs_report_collinear_backtracking_without_bends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backtrack.kicad_sch");
        let bytes = include_bytes!("../../tests/fixtures/schematic_wire_runs_backtrack.kicad_sch");
        let (result, body) = call_on_fixture(&path, bytes, json!({}), false).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(body["finding_count"], 1);
        let run = &body["findings"][0];
        assert_eq!(run["reasons"], json!(["orthogonal_detour"]));
        assert_eq!(run["length_mm"], 15.24);
        assert_eq!(run["direct_orthogonal_mm"], 5.08);
        assert_eq!(run["extra_length_mm"], 10.16);
        assert_eq!(run["bend_count"], 0);
        assert_eq!(run["start"], json!({"x":50.8,"y":50.8,"status":"attached"}));
        assert_eq!(run["end"], json!({"x":55.88,"y":50.8,"status":"open"}));
    }

    async fn assert_invalid_threshold(field: &str, value: Value) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.kicad_sch");
        let (result, body) = fixture_call(&path, json!({ (field): value }), false).await;
        assert_eq!(result["isError"], true, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], field);
    }

    #[tokio::test]
    async fn wire_runs_refuse_zero_detour_threshold_without_writing() {
        assert_invalid_threshold("detour_mm", json!(0)).await;
    }

    #[tokio::test]
    async fn wire_runs_refuse_negative_long_threshold_without_writing() {
        assert_invalid_threshold("long_run_mm", json!(-1)).await;
    }

    /// The tool's served answer for a file it cannot read as a schematic, and
    /// the directory holding it, unchanged.
    async fn served_refusal(
        name: &str,
        bytes: Option<&[u8]>,
    ) -> (Value, Value, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        if let Some(bytes) = bytes {
            fs::write(&path, bytes).unwrap();
        }
        let listing = |dir: &Path| {
            let mut names: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listing(dir.path());
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let (result, body) = served(
            &handler,
            "check_schematic_wire_runs",
            json!({"schematic": path}),
        )
        .await;
        assert_eq!(listing(dir.path()), before, "the directory changed");
        match bytes {
            Some(bytes) => assert_eq!(fs::read(&path).unwrap(), bytes),
            None => assert!(!path.exists()),
        }
        assert_eq!(result["isError"], true, "{body}");
        (result, body, path)
    }

    #[tokio::test]
    async fn wire_runs_report_a_missing_schematic_as_file_not_found() {
        let (_, body, path) = served_refusal("missing.kicad_sch", None).await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert_eq!(body["error"]["path"], path.display().to_string(), "{body}");
    }

    #[tokio::test]
    async fn wire_runs_report_a_non_schematic_as_malformed_file() {
        for (name, bytes) in [
            ("board.kicad_sch", &b"(kicad_pcb (version 20241229))"[..]),
            (
                "unbalanced.kicad_sch",
                &b"(kicad_sch (version 20250114)"[..],
            ),
            ("binary.kicad_sch", &b"(kicad_sch \xff)"[..]),
        ] {
            let (_, body, path) = served_refusal(name, Some(bytes)).await;
            assert_eq!(body["error"]["kind"], "malformed_file", "{name}: {body}");
            assert_eq!(body["error"]["path"], path.display().to_string(), "{body}");
            assert!(
                body["error"]["reason"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty()),
                "{name}: {body}"
            );
        }
    }

    fn measure(items: &str) -> WireRuns {
        measure_wire_runs(&parse_sexp(&format!("(kicad_sch {items})")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn wire_runs_refuse_malformed_attachments_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed.kicad_sch");
        let bytes = b"(kicad_sch (wire (pts (xy 0 20) (xy 130 20))) (label BAD (at NaN 20 0)))";
        fs::write(&path, bytes).unwrap();
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let (result, body) = served(
            &handler,
            "check_schematic_wire_runs",
            json!({"schematic": path}),
        )
        .await;
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(result["isError"], true, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file");
    }

    const STYLED_PIN: &str = r#"
        (lib_symbols
          (symbol "Test:Styled"
            (symbol "Styled_1_1" (pin passive line (at 0 0 0)))
            (symbol "Styled_1_2" (pin passive line (at 5 0 0)))))
        (symbol (lib_id "Test:Styled") (at 20 20 0) (unit 1) (property "Reference" "X1"))
    "#;

    #[test]
    fn wire_runs_use_only_the_drawn_pin_style() {
        let report = measure(&format!(
            "{STYLED_PIN} (wire (pts (xy 10 20) (xy 20 20)))
             (wire (pts (xy 20 20) (xy 25 20))) (wire (pts (xy 25 20) (xy 30 20)))"
        ));
        assert!(report.pin_geometry_resolved);
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].end, "attached");
        assert_eq!(report.runs[1].length_mm, 10.0);
    }

    #[test]
    fn wire_runs_keep_known_attachments_when_another_symbol_is_unresolved() {
        let report = measure(&format!(
            r#"{STYLED_PIN} (wire (pts (xy 10 20) (xy 20 20)))
               (symbol (lib_id "Test:Missing") (at 10 20 0))"#
        ));
        assert!(!report.pin_geometry_resolved);
        assert_eq!(report.runs[0].start, "unknown");
        assert_eq!(report.runs[0].end, "attached");
    }

    fn inspect(items: &str) -> Value {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.kicad_sch");
        fs::write(&path, format!("(kicad_sch {items})")).unwrap();
        let result = handle_check_schematic_wire_runs(&json!({
            "schematic": path,
            "long_run_mm": 1
        }))
        .unwrap();
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected JSON text");
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn wire_runs_leave_single_pin_nets_unnamed() {
        let body = inspect(&format!("{STYLED_PIN} (wire (pts (xy 10 20) (xy 20 20)))"));
        let run = &body["findings"][0];
        assert_eq!(run["end"]["status"], "attached");
        assert_eq!(run["net"], Value::Null);
    }

    #[test]
    fn wire_runs_name_a_power_pin_stub_without_joining_the_bare_crossing() {
        let body = inspect(
            r##"
            (lib_symbols (symbol "power:GND" (power) (pin power_in line (at 0 0 0))))
            (symbol (lib_id "power:GND") (at 15 20 0)
              (property "Reference" "#PWR01") (property "Value" "GND"))
            (wire (pts (xy 10 20) (xy 30 20))) (wire (pts (xy 15 20) (xy 15 25)))
        "##,
        );
        assert_eq!(body["run_count"], 2);
        assert_eq!(body["findings"][0]["length_mm"], 20.0);
        assert_eq!(body["findings"][0]["net"], Value::Null);
        assert_eq!(body["findings"][1]["start"]["status"], "attached");
        assert_eq!(body["findings"][1]["net"], "GND");
    }

    #[tokio::test]
    async fn wire_runs_name_only_the_power_input_of_a_power_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed-power.kicad_sch");
        let (result, body) =
            call_on_fixture(&path, MIXED_POWER, json!({"long_run_mm": 10}), false).await;
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(body["run_count"], 2);
        assert_eq!(body["findings"][0]["net"], "RAIL");
        assert_eq!(body["findings"][0]["end"]["status"], "attached");
        assert_eq!(body["findings"][1]["net"], "SIGNAL");
        assert_eq!(body["findings"][1]["start"]["status"], "attached");
    }

    #[tokio::test]
    async fn the_fixture_retains_its_legacy_connectivity_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed-power.kicad_sch");
        fs::write(&path, MIXED_POWER).unwrap();
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig::default())
            .await
            .unwrap();
        let (loaded, body) =
            served(&handler, "load_toolset", json!({"name": "sch_analysis"})).await;
        assert_ne!(loaded["isError"], true, "{body}");

        // Captured from the stdio tools on main (305cc780) using these same bytes.
        for (reference, net, near_x, pin_x, wire) in [
            ("R1", "RAIL", 71.1201, 60.96, [60.96, 71.12]),
            ("R2", "SIGNAL", 91.4401, 91.44, [81.28, 91.44]),
        ] {
            // The legacy trace rounds this query to the wire end. The run
            // graph's saved-coordinate equality would leave it unnamed.
            let (result, trace) = served(
                &handler,
                "trace_from_point",
                json!({"schematic": path, "x": near_x, "y": 50.8}),
            )
            .await;
            assert_ne!(result["isError"], true, "{trace}");
            assert_eq!(trace["net"], net, "{trace}");
            assert_eq!(
                trace["wires_here"],
                json!([{"x1": wire[0], "x2": wire[1], "y1": 50.8, "y2": 50.8}])
            );

            let (result, pin) = served(
                &handler,
                "get_pin_connections",
                json!({"schematic": path, "reference": reference, "pin_number": "1"}),
            )
            .await;
            assert_ne!(result["isError"], true, "{pin}");
            assert_eq!(
                pin,
                json!({"reference": reference, "pin": "1", "pin_x": pin_x, "pin_y": 50.8, "net": net})
            );

            let (result, connectivity) = served(
                &handler,
                "get_net_connectivity",
                json!({"schematic": path, "net": net}),
            )
            .await;
            assert_ne!(result["isError"], true, "{connectivity}");
            let wires = connectivity["wires"].as_array().unwrap();
            assert_eq!(wires.len(), 1, "{connectivity}");
            assert_eq!(json!([wires[0]["x1"], wires[0]["x2"]]), json!(wire));
        }
        assert_eq!(fs::read(path).unwrap(), MIXED_POWER);
    }

    #[test]
    fn wire_runs_do_not_attach_off_wire_labels_at_ends_or_midspan() {
        let body = inspect(
            r#"
            (wire (pts (xy 10 20) (xy 30 20)))
            (label NEAR_END (at 10 20.005 0))
            (label NEAR_MIDDLE (at 15 20.005 0))
        "#,
        );
        assert_eq!(body["run_count"], 1);
        assert_eq!(body["findings"][0]["start"]["status"], "open");
        assert_eq!(body["findings"][0]["net"], Value::Null);
    }

    #[test]
    fn wire_runs_keep_distinct_net_roots_at_saved_precision() {
        let body = inspect(
            "(wire (pts (xy 10 20) (xy 20 20))) (label FIRST (at 10 20 0))
             (wire (pts (xy 20.0001 20) (xy 30 20))) (label SECOND (at 30 20 0))",
        );
        assert_eq!(body["run_count"], 2);
        assert_eq!(body["findings"][0]["net"], "FIRST");
        assert_eq!(body["findings"][0]["end"]["status"], "open");
        assert_eq!(body["findings"][1]["net"], "SECOND");
        assert_eq!(body["findings"][1]["start"]["status"], "open");
    }

    #[test]
    fn wire_runs_report_the_same_root_name_across_an_interior_label() {
        let body = inspect(
            r#"
            (wire (pts (xy 10 20) (xy 30 20)) (uuid "original"))
            (label LOCAL (at 15 20 0))
            (global_label WINNER (at 30 20 0))
        "#,
        );
        assert_eq!(body["run_count"], 2);
        for index in 0..2 {
            assert_eq!(body["findings"][index]["net"], "WINNER");
            assert_eq!(
                body["findings"][index]["segments"][0]["wire_uuid"],
                "original"
            );
        }
    }

    #[test]
    fn wire_runs_split_at_interior_label() {
        let report = measure("(wire (pts (xy 10 20) (xy 30 20))) (label SIG (at 15 20 0))");
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].length_mm, 5.0);
        assert_eq!(report.runs[0].end, "attached");
        assert_eq!(report.runs[1].length_mm, 15.0);
        assert_eq!(report.runs[1].start, "attached");
    }

    #[test]
    fn wire_runs_split_at_interior_junction() {
        let report = measure("(wire (pts (xy 10 20) (xy 30 20))) (junction (at 15 20))");
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].length_mm, 5.0);
        assert_eq!(report.runs[1].length_mm, 15.0);
    }

    #[test]
    fn wire_runs_do_not_split_at_bare_crossing() {
        let report =
            measure("(wire (pts (xy 10 20) (xy 30 20))) (wire (pts (xy 15 10) (xy 15 25)))");
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].length_mm, 20.0);
        assert_eq!(report.runs[1].length_mm, 15.0);
    }

    #[test]
    fn wire_runs_stop_at_branches_without_requiring_junction_dots() {
        let report = measure("(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 20 20))) (wire (pts (xy 15 20) (xy 15 25)))");
        assert_eq!(report.runs.len(), 3);
        assert_eq!(report.runs[0].end, "branch");
        assert!(report.runs[1..].iter().all(|r| r.start == "branch"));
    }

    #[test]
    fn wire_runs_skip_closed_loop_segments() {
        let report = measure(RECTANGLE);
        assert!(report.runs.is_empty());
        assert_eq!(report.skipped_closed_segment_count, 4);
        assert_eq!(report.skipped_wire_count, 0);
    }

    const RECTANGLE: &str = "(wire (pts (xy 0 0) (xy 10 0))) (wire (pts (xy 10 0) (xy 10 10)))
                             (wire (pts (xy 10 10) (xy 0 10))) (wire (pts (xy 0 10) (xy 0 0)))";

    #[test]
    fn wire_runs_skip_a_run_that_returns_to_its_only_stop() {
        let body = inspect(&format!("{RECTANGLE} (label LOOP (at 0 0 0))"));
        assert_eq!(body["run_count"], 0);
        assert_eq!(body["finding_count"], 0);
        assert_eq!(body["skipped_closed_segment_count"], 4);
    }

    #[test]
    fn wire_runs_report_distinct_ends_on_a_loop_split_by_labels() {
        let body = inspect(&format!(
            "{RECTANGLE} (label LOOP (at 0 0 0)) (label LOOP (at 10 0 0))"
        ));
        assert_eq!(body["skipped_closed_segment_count"], 0);
        assert_eq!(body["run_count"], 2);
        assert_eq!(body["finding_count"], 2);
        let findings = body["findings"].as_array().unwrap();
        for run in findings {
            assert_eq!(run["start"], json!({"x":0.0,"y":0.0,"status":"attached"}));
            assert_eq!(run["end"], json!({"x":10.0,"y":0.0,"status":"attached"}));
            assert_eq!(run["net"], "LOOP");
        }
        assert!(findings.iter().any(|run| run["length_mm"] == 10.0));
        assert!(findings.iter().any(|run| run["length_mm"] == 30.0));
    }

    #[test]
    fn wire_runs_stop_at_unscored_wires() {
        for (skipped, status) in [
            ("(wire (pts (xy 15 20) (xy 20 25)))", "branch"),
            ("(wire (pts (xy 15 20) (xy 15 20)))", "attached"),
        ] {
            let report = measure(&format!(
                "(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 25 20))) {skipped}"
            ));
            assert_eq!(report.runs.len(), 2, "{skipped}");
            assert_eq!(report.skipped_wire_count, 1, "{skipped}");
            assert_eq!(report.runs[0].end, status, "{skipped}");
            assert_eq!(report.runs[1].start, status, "{skipped}");
        }
    }

    #[test]
    fn wire_runs_stop_at_no_connect_markers() {
        let report = measure("(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 25 20))) (no_connect (at 15 20))");
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].end, "marked_no_connect");
        assert_eq!(report.runs[1].start, "marked_no_connect");
    }

    #[test]
    fn wire_runs_stop_at_sheet_pins() {
        let report = measure(
            r#"(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 25 20))) (sheet (pin "OUT" output (at 15 20 0)))"#,
        );
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].end, "attached");
        assert_eq!(report.runs[1].start, "attached");
    }

    #[test]
    fn wire_runs_stop_at_verified_bus_entry_wire_ends() {
        let report = measure("(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 25 20))) (bus_entry (at 15 20) (size 5 5)) (bus (pts (xy 20 25) (xy 30 25)))");
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].end, "attached");
        assert_eq!(report.runs[1].start, "attached");
    }

    #[test]
    fn wire_runs_do_not_stop_at_a_detached_bus_entry() {
        let report = measure("(wire (pts (xy 10 20) (xy 15 20))) (wire (pts (xy 15 20) (xy 25 20))) (bus_entry (at 15 20) (size 5 5))");
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].length_mm, 15.0);
    }

    #[test]
    fn wire_runs_ignore_bus_entries_outside_the_spatial_index_range() {
        let report = measure(
            "(wire (pts (xy 10 20) (xy 25 20)))
             (bus (pts (xy 10 30) (xy 25 30)))
             (bus_entry (at 1e30 -1e30) (size 5 5))",
        );
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].start, "open");
        assert_eq!(report.runs[0].end, "open");
    }

    #[test]
    fn wire_runs_l_and_z_have_no_extra_length() {
        let l = "(wire (pts (xy 0 0) (xy 0 2))) (wire (pts (xy 0 2) (xy 5 2)))";
        for (tail, bends) in [("", 1), ("(wire (pts (xy 5 2) (xy 5 4)))", 2)] {
            let report = measure(&format!("{l} {tail}"));
            let run = &report.runs[0];
            assert_eq!(run.bend_count, bends);
            assert_eq!(run.extra_length_mm, 0.0);
        }
    }
}
