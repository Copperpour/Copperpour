use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;
use copperpour_sexp::schematic::{extract_wires, read_schematic};
use copperpour_sexp::SexpNode;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "sch_layout_score_guard_tests.rs"]
mod guards;
#[path = "sch_layout_library_tests.rs"]
mod library;

const CROWDED: &str = "region_cleanup_kicad10/crowded/crowded.kicad_sch";
const SPARSE: &str = "region_cleanup_kicad10/sparse/sparse.kicad_sch";
const DIRECT_LANES: &str = "direct_lanes_kicad10/direct_lanes.kicad_sch";
const TOOL: &str = "compare_schematic_layout";

#[path = "sch_layout_properties_tests.rs"]
mod properties;

#[tokio::test]
async fn only_preview_comparison_excludes_mandatory_lead_length() {
    let handler = handler().await;
    let sheet = Sheet::input("block_placement_kicad10/opamp_stage/opamp_stage.kicad_sch");
    let tree = sheet.tree();
    let stub = extract_wires(&tree)
        .into_iter()
        .find(|w| ((w.x1 - w.x2).hypot(w.y1 - w.y2) - 2.54).abs() < 1e-6)
        .expect("fixture has a 2.54 mm wire");
    let omitted = BTreeSet::from([stub.uuid.unwrap()]);
    let all = BTreeSet::new();
    let baseline = compare(&handler, &sheet, &sheet, &[]).await;
    let shared = super::compare_layout(&tree, Some(&tree), &[], [&all; 2]).json();
    assert_eq!(shared, baseline);
    let preview = super::compare_layout(&tree, Some(&tree), &[], [&all, &omitted]).json();
    let mut expected = baseline;
    expected["score"]["wire_length_mm"] = json!(495.30);
    expected["delta"]["wire_length_mm"] = json!(-2.54);
    expected["verdict"] = json!("improved");
    assert_eq!(preview, expected);
}

struct Sheet {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl Sheet {
    fn input(relative: &str) -> Self {
        Self::copy(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(relative),
        )
    }

    fn copy(source: &Path) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(source.file_name().unwrap());
        fs::copy(source, &path).unwrap();
        let project = source.with_extension("kicad_pro");
        if project.exists() {
            fs::copy(project, path.with_extension("kicad_pro")).unwrap();
        }
        Self { dir, path }
    }

    fn fork(&self) -> Self {
        Self::copy(&self.path)
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).unwrap()
    }

    fn tree(&self) -> SexpNode {
        read_schematic(&self.path).unwrap().1
    }

    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<_> = fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name().into(), fs::read(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..ServerConfig::default()
    })
    .await
    .unwrap()
}

async fn rpc(handler: &McpHandler, name: &str, arguments: Value) -> (bool, Value) {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 775, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .await
        .unwrap()
        .result
        .unwrap();
    let text = response["content"][0]["text"].as_str().unwrap();
    (
        response["isError"] == true,
        serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
    )
}

async fn call(handler: &McpHandler, sheet: &Sheet, name: &str, mut args: Value) -> (bool, Value) {
    args["schematic"] = json!(sheet.path);
    rpc(handler, name, args).await
}

async fn ok(handler: &McpHandler, sheet: &Sheet, name: &str, args: Value) -> Value {
    let (error, body) = call(handler, sheet, name, args).await;
    assert!(!error, "{name}: {body}");
    body
}

async fn compare(handler: &McpHandler, was: &Sheet, now: &Sheet, removed: &[String]) -> Value {
    let before = [was.files(), now.files()];
    let args = json!({ "baseline": was.path, "removed_power_symbol_uuids": removed });
    let first = ok(handler, now, TOOL, args.clone()).await;
    assert_eq!(ok(handler, now, TOOL, args).await, first);
    assert_eq!([was.files(), now.files()], before);
    first
}

async fn wire(handler: &McpHandler, sheet: &Sheet, wires: &[[f64; 4]]) {
    let wires: Vec<_> = wires
        .iter()
        .map(|&[x1, y1, x2, y2]| json!({"x1": x1, "y1": y1, "x2": x2, "y2": y2}))
        .collect();
    ok(handler, sheet, "batch_add_wire", json!({"wires": wires})).await;
}

fn reasons(result: &Value) -> String {
    result["reasons"].to_string()
}

fn assert_verdict(result: &Value, expected: &str) {
    assert_eq!(result["verdict"], expected, "{result}");
}

#[tokio::test]
async fn single_saved_score_and_gateway_report_the_same_read_only_result() {
    let handler = handler().await;
    let sheet = Sheet::input(CROWDED);
    let before = sheet.files();
    let result = ok(&handler, &sheet, TOOL, json!({})).await;
    let args = json!({"name": TOOL, "arguments": {"schematic": sheet.path, "baseline": null}});
    let (error, via_gateway) = rpc(&handler, "call_tool", args).await;
    assert!(!error, "{via_gateway}");
    assert_eq!(result, via_gateway);
    assert_eq!(sheet.files(), before);
    for key in ["baseline_score", "delta", "verdict"] {
        assert_eq!(result.get(key), Some(&Value::Null));
    }
    let expected = json!({
        "component_overlap": 0, "label_overlap": 0, "wire_overlap": 0,
        "dangling_wire_end": 0, "floating_label": 0, "unconnected_pin": 0,
        "text_overlap_text": 2, "text_overlap_body": 1, "wire_field_overlap": 0,
        "wire_pin_text_overlap": 0, "pin_contact": 7,
        "short_pin_wire": 0, "diagonal_wires": 0, "collinear_overlaps": 0,
        "wire_bends": 5, "split_runs": 0, "redundant_power_symbols": 0,
        "wire_length_mm": 130.81, "wire_segments": 9, "power_symbols": 10,
        "bounds_unresolved": [], "pin_text_unresolved": []
    });
    assert_eq!(result["score"], expected);
    assert_eq!(result["not_compared"], json!([]));
}

#[tokio::test]
async fn the_near_miss_fixture_retains_its_legacy_connectivity_answers() {
    let handler = handler().await;
    let connected = Sheet::input(CROWDED);
    let near_miss = connected.fork();
    let source = fs::read_to_string(&near_miss.path).unwrap();
    assert_eq!(source.matches("(xy 63.5 105.41)").count(), 1);
    fs::write(
        &near_miss.path,
        source.replacen("(xy 63.5 105.41)", "(xy 63.5004 105.41)", 1),
    )
    .unwrap();

    // The existing comparator treats the two endpoints as coincident.
    // Cleanup must use finer precision without changing that answer.
    let report = compare(&handler, &near_miss, &connected, &[]).await;
    assert_eq!(report["reasons"], json!([]), "{report}");
    assert_eq!(report["delta"]["diagonal_wires"], -1, "{report}");

    let before = near_miss.files();
    let report = ok(&handler, &near_miss, "find_pin_contacts", json!({})).await;
    assert_eq!(report["contact_count"], 7, "{report}");
    assert_eq!(report["short_wires"], json!([]), "{report}");
    assert_eq!(near_miss.files(), before);
}

async fn direct_lanes_with_clear_fields(handler: &McpHandler, crossed: &Sheet) -> Sheet {
    let clear = crossed.fork();
    ok(
        handler,
        &clear,
        "edit_schematic_component",
        json!({
            "reference": "C1",
            "field_placements": {
                "Reference": {"x": 71.12, "y": 69.85, "rotation": 90},
                "Value": {"x": 71.12, "y": 72.39, "rotation": 90}
            }
        }),
    )
    .await;
    clear
}

#[tokio::test]
async fn clearing_wire_field_crossings_improves_the_layout_score() {
    let handler = handler().await;
    let crossed = Sheet::input(DIRECT_LANES);
    let clear = direct_lanes_with_clear_fields(&handler, &crossed).await;
    let result = compare(&handler, &crossed, &clear, &[]).await;
    assert_verdict(&result, "improved");
    assert_eq!(
        result["baseline_score"]["wire_field_overlap"], 3,
        "{result}"
    );
    assert_eq!(result["score"]["wire_field_overlap"], 0, "{result}");
    assert_eq!(result["delta"]["wire_field_overlap"], -3, "{result}");
    for (term, delta) in result["delta"].as_object().unwrap() {
        if term != "wire_field_overlap" {
            assert_eq!(delta.as_f64(), Some(0.0), "{term}: {result}");
        }
    }
}

#[tokio::test]
async fn introducing_wire_field_crossings_makes_the_layout_not_improved() {
    let handler = handler().await;
    let crossed = Sheet::input(DIRECT_LANES);
    let clear = direct_lanes_with_clear_fields(&handler, &crossed).await;
    let result = compare(&handler, &clear, &crossed, &[]).await;
    assert_verdict(&result, "not_improved");
    assert_eq!(
        result["baseline_score"]["wire_field_overlap"], 0,
        "{result}"
    );
    assert_eq!(result["score"]["wire_field_overlap"], 3, "{result}");
    assert_eq!(result["delta"]["wire_field_overlap"], 3, "{result}");
}

#[tokio::test]
async fn score_counts_both_kinds_of_pin_contact_findings() {
    let handler = handler().await;
    let sheet = Sheet::input("pin_contact_kicad10/pin_contact.kicad_sch");
    let report = ok(&handler, &sheet, "find_pin_contacts", json!({})).await;
    let score = ok(&handler, &sheet, TOOL, json!({})).await;
    assert!(report["contact_count"].as_u64().unwrap() > 0);
    assert_eq!(report["short_wire_count"], 1);
    assert_eq!(score["score"]["pin_contact"], report["contact_count"]);
    assert_eq!(score["score"]["short_pin_wire"], report["short_wire_count"]);
    assert_eq!(score["not_compared"], json!([]));
}

const JOG: [[f64; 4]; 6] = [
    [63.5, 105.41, 63.5, 96.52],
    [63.5, 96.52, 88.9, 96.52],
    [88.9, 96.52, 88.9, 63.5],
    [88.9, 63.5, 106.68, 63.5],
    [106.68, 63.5, 106.68, 76.2],
    [106.68, 76.2, 119.38, 76.2],
];

async fn delete_jog(handler: &McpHandler, sheet: &Sheet) {
    let uuids: Vec<_> = extract_wires(&sheet.tree())
        .into_iter()
        .filter(|w| {
            let close =
                |a: [f64; 4], b: [f64; 4]| a.into_iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
            JOG.iter()
                .any(|&j| close(j, [w.x1, w.y1, w.x2, w.y2]) || close(j, [w.x2, w.y2, w.x1, w.y1]))
        })
        .map(|w| w.uuid.unwrap())
        .collect();
    let body = ok(
        handler,
        sheet,
        "batch_delete_schematic_wire",
        json!({"uuids": uuids}),
    )
    .await;
    assert_eq!(body["deleted"], 6);
}

async fn move_c1(handler: &McpHandler, sheet: &Sheet, dx: f64, dy: f64) {
    ok(
        handler,
        sheet,
        "move_region",
        json!({"x1": 58.0, "y1": 100.0, "x2": 69.0, "y2": 118.0, "dx": dx, "dy": dy, "include": "all"}),
    )
    .await;
}

fn pins(ref1: &str, pin1: &str, ref2: &str, pin2: &str) -> Value {
    json!({"ref1": ref1, "pin1": pin1, "ref2": ref2, "pin2": pin2})
}

async fn connect_c1(handler: &McpHandler, sheet: &Sheet) -> (bool, Value) {
    call(handler, sheet, "connect_pins", pins("C1", "1", "U1", "3")).await
}

#[tokio::test]
async fn cleanup_and_reroute_improve_but_replay_does_not() {
    let handler = handler().await;
    let crowded = Sheet::input(CROWDED);
    let reroute = crowded.fork();
    delete_jog(&handler, &reroute).await;
    assert!(!connect_c1(&handler, &reroute).await.0);
    let clean = crowded.fork();
    delete_jog(&handler, &clean).await;
    move_c1(&handler, &clean, 50.8, -29.21).await;
    assert!(!connect_c1(&handler, &clean).await.0);

    for (was, now, bends, segments, length) in [
        (&crowded, &reroute, 1, 5, 105.41),
        (&reroute, &clean, 0, 4, 25.4),
        (&crowded, &clean, 0, 4, 25.4),
    ] {
        let result = compare(&handler, was, now, &[]).await;
        assert_verdict(&result, "improved");
        assert_eq!(result["score"]["wire_bends"], bends);
        assert_eq!(result["score"]["wire_segments"], segments);
        assert_eq!(result["score"]["wire_length_mm"], length);
        assert_eq!(result["delta"]["text_overlap_text"], 0);
        assert_eq!(result["delta"]["pin_contact"], 0);
    }

    let replay = clean.fork();
    assert!(!connect_c1(&handler, &replay).await.0);
    let result = compare(&handler, &clean, &replay, &[]).await;
    assert_verdict(&result, "not_improved");
    assert_eq!(result["delta"]["collinear_overlaps"], 1);
    assert_eq!(result["score"]["wire_length_mm"], 30.48);
}

#[tokio::test]
async fn unchanged_score_does_not_certify_no_write() {
    let handler = handler().await;
    let sparse = Sheet::input(SPARSE);
    let copy = sparse.fork();
    assert_verdict(&compare(&handler, &sparse, &copy, &[]).await, "unchanged");
    assert_eq!(sparse.bytes(), copy.bytes());

    let clean = Sheet::input(CROWDED);
    delete_jog(&handler, &clean).await;
    move_c1(&handler, &clean, 50.8, -29.21).await;
    assert!(!connect_c1(&handler, &clean).await.0);
    let second = clean.fork();
    move_c1(&handler, &second, 0.0, 0.0).await;
    assert_verdict(&compare(&handler, &clean, &second, &[]).await, "unchanged");
    assert_ne!(clean.bytes(), second.bytes());
}

#[tokio::test]
async fn a_symbol_only_move_loses_a_pin_and_is_blocked() {
    let handler = handler().await;
    let crowded = Sheet::input(CROWDED);
    let bulk = crowded.fork();
    ok(
        &handler,
        &bulk,
        "bulk_move_schematic_components",
        json!({"references": ["C1"], "dx": 50.8, "dy": -29.21}),
    )
    .await;
    let result = compare(&handler, &crowded, &bulk, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        reasons(&result).contains("would lose [C1 pin 2]"),
        "{result}"
    );
}

#[tokio::test]
async fn a_stranded_pin_blocks_a_shorter_layout() {
    let handler = handler().await;
    let constrained = Sheet::input("region_cleanup_kicad10/constrained/constrained.kicad_sch");
    let partial = constrained.fork();
    delete_jog(&handler, &partial).await;
    let before = partial.bytes();
    let (error, response) = connect_c1(&handler, &partial).await;
    assert!(error, "{response}");
    assert_eq!(response["error"]["kind"], "plan_blocked");
    assert_eq!(partial.bytes(), before);
    let result = compare(&handler, &constrained, &partial, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        reasons(&result).contains("would lose [C1 pin 1]"),
        "{result}"
    );
    assert_eq!(result["delta"]["wire_length_mm"], -110.49);
}

#[tokio::test]
async fn clearer_elbows_improve_and_new_connections_are_not_layout_only() {
    let handler = handler().await;
    let input = Sheet::input("route_elbows_kicad10.kicad_sch");
    let routed = input.fork();
    let clearer = input.fork();
    // Preserve the old router's elbows even when connect_pins chooses better.
    wire(
        &handler,
        &routed,
        &[
            [50.8, 54.61, 76.2, 54.61],
            [76.2, 54.61, 76.2, 63.5],
            [101.6, 54.61, 127.0, 54.61],
            [127.0, 54.61, 127.0, 63.5],
        ],
    )
    .await;
    wire(
        &handler,
        &clearer,
        &[
            [50.8, 54.61, 50.8, 63.5],
            [50.8, 63.5, 76.2, 63.5],
            [101.6, 54.61, 101.6, 63.5],
            [101.6, 63.5, 127.0, 63.5],
        ],
    )
    .await;
    for args in [
        pins("R7", "2", "R8", "1"),
        pins("R9", "2", "R10", "1"),
        pins("R12", "2", "R13", "1"),
    ] {
        ok(&handler, &routed, "connect_pins", args.clone()).await;
        ok(&handler, &clearer, "connect_pins", args).await;
    }
    let result = compare(&handler, &routed, &clearer, &[]).await;
    assert_verdict(&result, "improved");
    assert_eq!(result["baseline_score"]["wire_overlap"], 3);
    assert_eq!(result["score"]["wire_overlap"], 2);
    assert_eq!(result["delta"]["wire_length_mm"], 0.0);
    let result = compare(&handler, &input, &routed, &[]).await;
    assert_verdict(&result, "blocked");
    assert_changed_pins(
        &result,
        &[
            "R1 pin 2",
            "R2 pin 1",
            "R4 pin 2",
            "R5 pin 1",
            "R7 pin 2",
            "R8 pin 1",
            "R9 pin 2",
            "R10 pin 1",
            "R12 pin 2",
            "R13 pin 1",
        ],
    );
}

fn assert_changed_pins(result: &Value, expected: &[&str]) {
    use std::collections::BTreeSet;
    let found: BTreeSet<_> = result["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|reason| {
            reason
                .as_str()
                .unwrap()
                .split(['[', ']'])
                .skip(1)
                .step_by(2)
        })
        .flat_map(|list| list.split(", "))
        .collect();
    assert_eq!(found, expected.iter().copied().collect(), "{result}");
}

#[tokio::test]
async fn a_longer_label_stub_trades_wire_for_readability() {
    let handler = handler().await;
    let input = Sheet::input("authoring_label_clearance_kicad10/amp_stage.kicad_sch");
    let short = input.fork();
    let long = input.fork();
    for (pin, net) in [("1", "SIG_IN"), ("2", "SIG_OUT")] {
        let mut args =
            json!({"reference": "J1", "pin_number": pin, "net": net, "stub_length": 2.54});
        ok(&handler, &short, "connect_to_net", args.clone()).await;
        args["stub_length"] = json!(5.08);
        ok(&handler, &long, "connect_to_net", args).await;
    }
    let result = compare(&handler, &short, &long, &[]).await;
    assert_verdict(&result, "improved");
    assert_eq!(result["baseline_score"]["text_overlap_text"], 2);
    assert_eq!(result["score"]["text_overlap_text"], 1);
    assert_eq!(result["delta"]["wire_length_mm"], 5.08);
    assert_verdict(&compare(&handler, &long, &short, &[]).await, "not_improved");
    let result = compare(&handler, &input, &short, &[]).await;
    assert_verdict(&result, "blocked");
    assert_changed_pins(
        &result,
        &["J1 pin 1", "J1 pin 2", "U1 pin 1", "U1 pin 3", "R1 pin 2"],
    );
}

#[tokio::test]
async fn library_direction_rails_trade_wire_for_readability() {
    let handler = handler().await;
    let today = Sheet::input("power_orientation_kicad10/power_orientation.kicad_sch");
    let mut references = Vec::new();
    for (net, reference, pin) in [
        ("+3V3", "J1", "1"),
        ("GND", "J1", "2"),
        ("-5V", "J3", "1"),
        ("+3V3", "J6", "1"),
        ("GND", "J6", "3"),
        ("GND", "J6", "6"),
    ] {
        let body = ok(
            &handler,
            &today,
            "add_power_symbol",
            json!({"power_net": net, "reference": reference, "pin_number": pin, "stub_length": 0}),
        )
        .await;
        references.push(body["reference"].as_str().unwrap().to_owned());
    }
    let library = today.fork();
    let mut turned = Vec::new();
    for (i, [x1, y1, x2, y2]) in [
        (0, [68.58, 63.5, 71.12, 63.5]),
        (1, [68.58, 66.04, 71.12, 66.04]),
        (2, [68.58, 109.22, 71.12, 109.22]),
        (3, [116.84, 111.76, 114.3, 111.76]),
        (5, [116.84, 124.46, 114.3, 124.46]),
    ] {
        let reference = &references[i];
        ok(
            &handler,
            &library,
            "move_schematic_component",
            json!({"reference": reference, "x": x2, "y": y2}),
        )
        .await;
        ok(
            &handler,
            &library,
            "rotate_schematic_component",
            json!({"reference": reference, "rotation": 0}),
        )
        .await;
        wire(&handler, &library, &[[x1, y1, x2, y2]]).await;
        turned.push(reference);
    }
    ok(
        &handler,
        &library,
        "reset_schematic_field_positions",
        json!({"references": turned}),
    )
    .await;
    let result = compare(&handler, &today, &library, &[]).await;
    assert_verdict(&result, "improved");
    assert_eq!(result["baseline_score"]["text_overlap_text"], 3);
    assert_eq!(result["score"]["text_overlap_text"], 1);
    assert_eq!(result["delta"].get("wire_segments"), None);
    assert_eq!(result["score"]["wire_segments"], 8);
    assert_eq!(result["score"]["wire_length_mm"], 20.32);
}

fn symbol_uuid(sheet: &Sheet, reference: &str) -> String {
    let tree = sheet.tree();
    copperpour_sexp::schematic::extract_symbol_instances(&tree)
        .into_iter()
        .find(|inst| inst.reference == reference)
        .unwrap()
        .uuid
        .unwrap()
}

#[tokio::test]
async fn grouping_ldo_supplies_reduces_bends_even_with_more_segments() {
    let handler = handler().await;
    let baseline = Sheet::input("block_placement_kicad10/ldo_enable/ldo_enable.kicad_sch");
    let grouped = baseline.fork();
    let removed: Vec<_> = ["#PWR08", "#PWR010", "#PWR011", "#PWR013"]
        .map(|reference| symbol_uuid(&baseline, reference))
        .into_iter()
        .collect();
    let wire_ids: Vec<_> = extract_wires(&grouped.tree())
        .into_iter()
        .map(|w| w.uuid.unwrap())
        .collect();
    ok(
        &handler,
        &grouped,
        "batch_delete_schematic_wire",
        json!({"uuids": wire_ids}),
    )
    .await;
    ok(
        &handler,
        &grouped,
        "batch_delete",
        json!({"uuids": removed}),
    )
    .await;
    for (reference, x, y) in [
        ("J1", 66.04, 86.36),
        ("J2", 157.48, 86.36),
        ("R1", 111.76, 92.71),
        ("C1", 104.14, 90.17),
        ("C2", 139.7, 92.71),
        ("#PWR01", 91.44, 83.82),
        ("#PWR02", 91.44, 91.44),
        ("#PWR03", 78.74, 86.36),
        ("#PWR04", 78.74, 88.9),
        ("#PWR05", 106.68, 83.82),
        ("#PWR06", 137.16, 83.82),
        ("#PWR09", 152.4, 91.44),
        ("#PWR012", 104.14, 96.52),
        ("#PWR014", 139.7, 99.06),
    ] {
        ok(
            &handler,
            &grouped,
            "move_schematic_component",
            json!({"reference": reference, "x": x, "y": y}),
        )
        .await;
    }
    wire(
        &handler,
        &grouped,
        &[
            [91.44, 86.36, 91.44, 83.82],
            [71.12, 86.36, 78.74, 86.36],
            [78.74, 88.9, 91.44, 88.9],
            [127.0, 96.52, 127.0, 99.06],
            [78.74, 86.36, 91.44, 86.36],
            [91.44, 88.9, 91.44, 91.44],
            [71.12, 88.9, 78.74, 88.9],
            [119.38, 86.36, 111.76, 86.36],
            [111.76, 86.36, 111.76, 88.9],
            [111.76, 86.36, 106.68, 86.36],
            [106.68, 86.36, 106.68, 83.82],
            [106.68, 86.36, 104.14, 86.36],
            [134.62, 86.36, 137.16, 86.36],
            [137.16, 86.36, 137.16, 83.82],
            [137.16, 86.36, 139.7, 86.36],
            [139.7, 86.36, 139.7, 88.9],
            [139.7, 86.36, 152.4, 86.36],
            [104.14, 93.98, 104.14, 96.52],
            [139.7, 96.52, 139.7, 99.06],
            [152.4, 88.9, 152.4, 91.44],
            [111.76, 96.52, 119.38, 96.52],
            [119.38, 96.52, 119.38, 88.9],
        ],
    )
    .await;
    let result = compare(&handler, &baseline, &grouped, &removed).await;
    assert_verdict(&result, "improved");
    for (field, was, now) in [
        ("wire_bends", json!(5), json!(3)),
        ("wire_segments", json!(20), json!(22)),
        ("wire_length_mm", json!(170.18), json!(114.3)),
    ] {
        assert_eq!(result["baseline_score"][field], was, "{result}");
        assert_eq!(result["score"][field], now, "{result}");
    }
    let result = compare(&handler, &baseline, &grouped, &removed[1..]).await;
    assert_verdict(&result, "blocked");
    assert!(reasons(&result).contains(&format!("symbol {} was removed", removed[0])));
}

#[tokio::test]
async fn only_supply_symbols_on_the_same_drawn_node_are_redundant() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    for (power_net, x) in [
        ("+5V", 20.32),
        ("+5V", 25.4),
        ("+5V", 45.72),
        ("PWR_FLAG", 30.48),
        ("PWR_FLAG", 35.56),
    ] {
        ok(
            &handler,
            &before,
            "add_power_symbol",
            json!({"power_net": power_net, "x": x, "y": 20.32}),
        )
        .await;
    }
    wire(
        &handler,
        &before,
        &[
            [20.32, 20.32, 25.4, 20.32],
            [25.4, 20.32, 30.48, 20.32],
            [25.4, 20.32, 25.4, 25.4],
            [30.48, 20.32, 35.56, 20.32],
            [45.72, 20.32, 50.8, 20.32],
        ],
    )
    .await;
    ok(
        &handler,
        &before,
        "add_schematic_net_label",
        json!({"net": "+5V", "x": 50.8, "y": 20.32}),
    )
    .await;
    ok(
        &handler,
        &before,
        "add_schematic_net_label",
        json!({"net": "+5V", "x": 25.4, "y": 25.4}),
    )
    .await;
    let mut rails = copperpour_sexp::schematic::extract_power_symbol_labels(&before.tree());
    let removed = rails
        .drain(..)
        .find(|label| label.net == "+5V" && (label.x - 25.4).abs() < 1e-6)
        .unwrap()
        .uuid
        .unwrap();
    let after = before.fork();
    ok(
        &handler,
        &after,
        "batch_delete",
        json!({"uuids": [removed]}),
    )
    .await;
    let result = compare(&handler, &before, &after, &[removed]).await;
    assert_verdict(&result, "improved");
    assert_eq!(result["baseline_score"]["redundant_power_symbols"], 1);
    assert_eq!(result["score"]["redundant_power_symbols"], 0);
    assert_eq!(result["delta"]["split_runs"], 0);
}

#[tokio::test]
async fn listing_the_last_supply_does_not_allow_losing_a_net_name() {
    let handler = handler().await;
    let before = Sheet::input("power_orientation_kicad10/power_orientation.kicad_sch");
    let original = symbol_uuid(&before, "#PWR03");
    ok(
        &handler,
        &before,
        "batch_delete",
        json!({"uuids": [original]}),
    )
    .await;
    let supply = ok(
        &handler,
        &before,
        "add_power_symbol",
        json!({"power_net": "-5V", "x": 25.4, "y": 20.32}),
    )
    .await;
    let uuid = symbol_uuid(&before, supply["reference"].as_str().unwrap());
    let after = before.fork();
    ok(&handler, &after, "batch_delete", json!({"uuids": [uuid]})).await;
    let result = compare(&handler, &before, &after, &[uuid]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!(["net -5V no longer has its baseline name and scope on the sheet"])
    );
}

#[tokio::test]
async fn changed_embedded_pin_numbers_block_a_same_identity_comparison() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let after = before.fork();
    let source = String::from_utf8(before.bytes()).unwrap();
    assert!(source.contains("(number \"1\"") && source.contains("(number \"2\""));
    let changed = source
        .replacen("(number \"1\"", "(number \"swap\"", 1)
        .replacen("(number \"2\"", "(number \"1\"", 1)
        .replacen("(number \"swap\"", "(number \"2\"", 1);
    fs::write(&after.path, changed).unwrap();
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!(["the move changes which pins the sheet places"])
    );
}

#[tokio::test]
async fn replacing_a_local_label_with_a_global_label_is_not_layout_only() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let after = before.fork();
    let args = json!({"net": "SCOPE", "x": 25.4, "y": 20.32});
    ok(&handler, &before, "add_schematic_net_label", args.clone()).await;
    let mut args = args;
    args["label_type"] = json!("global_label");
    ok(&handler, &after, "add_schematic_net_label", args).await;
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(reasons(&result).contains("net SCOPE no longer has its baseline name and scope"));
}

#[tokio::test]
async fn a_colocated_local_label_does_not_hide_loss_of_global_scope() {
    let handler = handler().await;
    let after = Sheet::input(SPARSE);
    wire(&handler, &after, &[[25.4, 20.32, 30.48, 20.32]]).await;
    ok(
        &handler,
        &after,
        "add_schematic_net_label",
        json!({"net": "SCOPE", "x": 25.4, "y": 20.32}),
    )
    .await;
    let before = after.fork();
    ok(
        &handler,
        &before,
        "add_schematic_net_label",
        json!({"net": "SCOPE", "x": 25.4, "y": 20.32, "label_type": "global_label"}),
    )
    .await;
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(reasons(&result).contains("net SCOPE no longer has its baseline name and scope"));
}

#[tokio::test]
async fn an_unchanged_primary_net_name_does_not_hide_alias_changes() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    wire(&handler, &before, &[[25.4, 20.32, 30.48, 20.32]]).await;
    ok(
        &handler,
        &before,
        "add_schematic_net_label",
        json!({"net": "PRIMARY", "x": 25.4, "y": 20.32, "label_type": "global_label"}),
    )
    .await;
    for label_type in ["global_label", "hierarchical_label"] {
        let after = before.fork();
        ok(
            &handler,
            &after,
            "add_schematic_net_label",
            json!({"net": "SECONDARY", "x": 30.48, "y": 20.32, "label_type": label_type}),
        )
        .await;
        for (was, now) in [(&before, &after), (&after, &before)] {
            let result = compare(&handler, was, now, &[]).await;
            assert_verdict(&result, "blocked");
            assert!(reasons(&result).contains("SECONDARY"), "{result}");
            assert!(!reasons(&result).contains("PRIMARY"), "{result}");
        }
    }
}

#[tokio::test]
async fn changing_library_power_scope_blocks_an_otherwise_identical_sheet() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let after = before.fork();
    let source = String::from_utf8(before.bytes()).unwrap();
    let changed = source.replacen("(power global)", "(power local)", 1);
    assert_ne!(changed, source);
    fs::write(&after.path, changed).unwrap();
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        reasons(&result).contains("would change scope (+5V)"),
        "{result}"
    );
}

#[tokio::test]
async fn a_hidden_power_pin_joins_a_separate_rail_by_name() {
    let handler = handler().await;
    let after = Sheet::input("hidden_power_pin/single_pin_nets.kicad_sch");
    // Both snapshots retain the visible rail name. Only the wire to the
    // hidden pin is redundant: its own global driver already joins the net.
    let (x, y) = (200.66, 44.45);
    ok(
        &handler,
        &after,
        "move_schematic_component",
        json!({"reference": "R9", "x": x - 15.24, "y": y}),
    )
    .await;
    let label_x = x - 11.43;
    ok(
        &handler,
        &after,
        "add_schematic_net_label",
        json!({"net": "HIDDEN", "x": label_x, "y": y, "label_type": "global_label"}),
    )
    .await;
    let before = after.fork();
    wire(&handler, &before, &[[label_x, y, x, y]]).await;
    let result = compare(&handler, &before, &after, &[]).await;
    assert_eq!(result["reasons"], json!([]), "{result}");
}

#[tokio::test]
async fn removing_a_symbol_from_the_pcb_is_not_layout_only() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let after = before.fork();
    let source = String::from_utf8(before.bytes()).unwrap();
    let start = source.find("(lib_id \"Device:R\")").unwrap();
    let changed = source[..start].to_owned()
        + &source[start..].replacen("(on_board yes)", "(on_board no)", 1);
    assert_ne!(changed, source);
    fs::write(&after.path, changed).unwrap();
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} changed its PCB membership",
            symbol_uuid(&before, "R1")
        )])
    );
}

#[tokio::test]
async fn listing_an_ordinary_symbol_does_not_allow_removing_it() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let uuid = symbol_uuid(&before, "R1");
    let after = before.fork();
    ok(&handler, &after, "batch_delete", json!({"uuids": [uuid]})).await;
    let result = compare(&handler, &before, &after, std::slice::from_ref(&uuid)).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"].as_array().unwrap().first(),
        Some(&json!(format!("{uuid} is not a removed power symbol"))),
        "{result}"
    );
}

#[tokio::test]
async fn simple_wire_joints_exclude_pins_and_labels() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    wire(
        &handler,
        &before,
        &[
            [12.7, 12.7, 17.78, 12.7],
            [17.78, 12.7, 22.86, 12.7],
            [12.7, 20.32, 17.78, 20.32],
            [17.78, 20.32, 22.86, 25.4],
            [12.7, 30.48, 17.78, 30.48],
            [17.78, 30.48, 17.78, 35.56],
        ],
    )
    .await;
    let result = ok(&handler, &before, TOOL, json!({})).await;
    assert_eq!(result["score"]["wire_bends"], 2);
    assert_eq!(result["score"]["split_runs"], 1);
    assert_eq!(result["score"]["diagonal_wires"], 1);
    let after = before.fork();
    ok(
        &handler,
        &after,
        "add_power_symbol",
        json!({"power_net": "+5V", "x": 17.78, "y": 12.7}),
    )
    .await;
    ok(
        &handler,
        &after,
        "add_schematic_net_label",
        json!({"net": "+5V", "x": 17.78, "y": 20.32}),
    )
    .await;
    let result = ok(&handler, &after, TOOL, json!({})).await;
    assert_eq!(result["score"]["wire_bends"], 1);
    assert_eq!(result["score"]["split_runs"], 0);
}

#[tokio::test]
async fn unavailable_geometry_is_null_on_either_side_of_a_comparison() {
    let handler = handler().await;
    let intact = Sheet::input(SPARSE);
    let missing = intact.fork();
    let original = String::from_utf8(intact.bytes()).unwrap();
    let edited = original.replacen("(symbol \"Device:R\"", "(symbol \"Device:Unknown\"", 1);
    assert_ne!(original, edited);
    fs::write(&missing.path, edited).unwrap();
    for (was, now, side) in [
        (&intact, &missing, "score"),
        (&missing, &intact, "baseline_score"),
    ] {
        let result = compare(&handler, was, now, &[]).await;
        assert_verdict(&result, "blocked");
        assert!(reasons(&result).contains(crate::tools::UNRESOLVED_PIN_GEOMETRY));
        let unresolved = result[side]["bounds_unresolved"].as_array().unwrap();
        assert!(unresolved.contains(&json!("R1")) && unresolved.contains(&json!("R2")));
        for key in [
            "component_overlap",
            "unconnected_pin",
            "wire_bends",
            "pin_contact",
            "short_pin_wire",
        ] {
            assert_eq!(result[side][key], Value::Null);
            assert_eq!(result["delta"].get(key), Some(&Value::Null));
            assert!(result["not_compared"]
                .as_array()
                .unwrap()
                .contains(&json!(key)));
        }
        assert_eq!(result["delta"]["wire_length_mm"], 0.0);
    }
}

#[tokio::test]
async fn a_power_pin_mid_wire_connects_only_with_the_junction() {
    let handler = handler().await;
    let before = Sheet::input(SPARSE);
    let (x, y) = (50.8, 30.48);
    ok(
        &handler,
        &before,
        "move_schematic_component",
        json!({"reference": "R1", "x": x, "y": y + 3.81}),
    )
    .await;
    ok(
        &handler,
        &before,
        "move_schematic_component",
        json!({"reference": "R2", "x": x + 12.7, "y": y + 3.81}),
    )
    .await;
    ok(
        &handler,
        &before,
        "add_power_symbol",
        json!({"power_net": "+5V", "reference": "R1", "pin_number": "1"}),
    )
    .await;
    wire(&handler, &before, &[[x - 12.7, y, x + 12.7, y]]).await;
    ok(&handler, &before, "add_junction", json!({"x": x, "y": y})).await;
    let after = before.fork();
    let junction = before
        .tree()
        .find_all("junction")
        .into_iter()
        .find(|node| {
            copperpour_sexp::schematic::parse_at(node)
                .is_some_and(|(a, b, _)| (a - x).abs() < 1e-6 && (b - y).abs() < 1e-6)
        })
        .unwrap()
        .find_str("uuid")
        .unwrap()
        .to_owned();
    ok(
        &handler,
        &after,
        "batch_delete",
        json!({"uuids": [junction]}),
    )
    .await;
    let result = compare(&handler, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(reasons(&result).contains("R2 pin 1"), "{result}");
}

#[test]
fn no_tier_a_or_b_term_may_get_worse_even_if_its_tier_sum_improves() {
    for (better, worse) in [
        ("wire_overlap", "component_overlap"),
        ("text_overlap_text", "text_overlap_body"),
    ] {
        let mut delta: super::Terms = super::terms().map(|key| (key, Some(0))).collect();
        delta.insert(better, Some(-2));
        delta.insert(worse, Some(1));
        assert_eq!(super::verdict(&delta, &[]), "not_improved");
    }
}

#[test]
fn a_lower_tier_breaks_a_tied_sum_but_not_a_worse_sum() {
    let mut delta: super::Terms = super::terms().map(|key| (key, Some(0))).collect();
    delta.insert("wire_bends", Some(-1));
    delta.insert("split_runs", Some(1));
    delta.insert("wire_length_mm", Some(-1));
    assert_eq!(super::verdict(&delta, &[]), "improved");
    delta.insert("wire_length_mm", Some(0));
    assert_eq!(super::verdict(&delta, &[]), "not_improved");
    delta.insert("split_runs", Some(2));
    delta.insert("wire_length_mm", Some(-2));
    assert_eq!(super::verdict(&delta, &[]), "not_improved");
}

#[test]
fn collinear_overlap_is_invariant_under_wire_and_endpoint_order() {
    use super::{collinear_overlap, Wire};
    let wire = |[x1, y1, x2, y2]: [f64; 4]| Wire {
        x1,
        y1,
        x2,
        y2,
        uuid: None,
    };
    let long = wire([0.0, 0.0, 10.0, 0.0]);
    for (other, expected) in [
        ([0.0, 0.0, 10.0, 0.0], true),
        ([5.0, 0.0, 15.0, 0.0], true),
        ([10.0, 0.0, 15.0, 0.0], false),
        ([5.0, 0.0, 5.5, 0.00009], false),
    ] {
        let [x1, y1, x2, y2] = other;
        for short in [wire(other), wire([x2, y2, x1, y1])] {
            assert_eq!(collinear_overlap(&long, &short), expected);
            assert_eq!(collinear_overlap(&short, &long), expected);
        }
    }
}
