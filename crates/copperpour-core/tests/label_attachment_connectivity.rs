use std::path::{Path, PathBuf};

use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};

const EXACT: &str = include_str!("fixtures/label_attachment_kicad10/exact/opamp_stage.kicad_sch");
const OFFSET: &str = include_str!("fixtures/label_attachment_kicad10/offset/opamp_stage.kicad_sch");
const ENDPOINT_OFFSET: &str =
    include_str!("fixtures/label_attachment_kicad10/endpoint-offset/opamp_stage.kicad_sch");
const CROSSING: &str =
    include_str!("fixtures/label_attachment_kicad10/crossing/opamp_stage.kicad_sch");
const JUNCTION: &str =
    include_str!("fixtures/label_attachment_kicad10/junction/opamp_stage.kicad_sch");
const JUNCTION_ONE_UNIT: &str =
    include_str!("fixtures/label_attachment_kicad10/junction-1iu/opamp_stage.kicad_sch");
const NAME_JOIN: &str =
    include_str!("fixtures/label_attachment_kicad10/name-join/opamp_stage.kicad_sch");
const ONE_UNIT: &str =
    include_str!("fixtures/label_attachment_kicad10/one-unit/opamp_stage.kicad_sch");
const TWO_UNITS: &str =
    include_str!("fixtures/label_attachment_kicad10/two-units/opamp_stage.kicad_sch");
const PROJECT: &str =
    include_str!("fixtures/block_placement_kicad10/opamp_stage/opamp_stage.kicad_pro");
const POWER_ROUNDED: &str =
    include_str!("fixtures/label_attachment_kicad10/power-rounded-pin/opamp_stage.kicad_sch");
const POWER_DETACHED: &str =
    include_str!("fixtures/label_attachment_kicad10/power-wrong-pin-point/opamp_stage.kicad_sch");
const SHALLOW_WIRE: &str =
    include_str!("fixtures/label_attachment_kicad10/shallow-wire-route/opamp_stage.kicad_sch");

struct Sheet<'a> {
    _dir: tempfile::TempDir,
    path: PathBuf,
    source: &'a str,
}

impl<'a> Sheet<'a> {
    fn new(source: &'a str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opamp_stage.kicad_sch");
        std::fs::write(&path, source).unwrap();
        std::fs::write(path.with_extension("kicad_pro"), PROJECT).unwrap();
        Self {
            _dir: dir,
            path,
            source,
        }
    }

    fn assert_unchanged(&self) {
        assert_eq!(std::fs::read_to_string(&self.path).unwrap(), self.source);
        assert_eq!(
            std::fs::read_to_string(self.path.with_extension("kicad_pro")).unwrap(),
            PROJECT
        );
        assert_eq!(
            std::fs::read_dir(self.path.parent().unwrap())
                .unwrap()
                .count(),
            2
        );
    }
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn served(handler: &McpHandler, sheet: &Sheet<'_>, name: &str, mut args: Value) -> Value {
    args["schematic"] = json!(sheet.path);
    handler
        .handle_message(json!({"jsonrpc": "2.0", "id": 901, "method": "tools/call",
            "params": {"name": name, "arguments": args}}))
        .await
        .unwrap()
        .result
        .unwrap()
}

async fn call(handler: &McpHandler, sheet: &Sheet<'_>, name: &str, args: Value) -> Value {
    let result = served(handler, sheet, name, args).await;
    assert_ne!(result["isError"], true, "{name}: {result}");
    sheet.assert_unchanged();
    serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

async fn assert_membership(source: &str, attached: bool) {
    let sheet = Sheet::new(source);
    let handler = handler().await;
    let net = if attached {
        json!("TOLERANT_OUT")
    } else {
        Value::Null
    };
    let trace = call(
        &handler,
        &sheet,
        "trace_from_point",
        json!({"x": 190.5, "y": 88.9}),
    )
    .await;
    assert_eq!(trace["net"], net, "{trace}");
    let pins = trace["pins_here"].as_array().unwrap();
    assert_eq!(pins.len(), 1, "{trace}");
    assert_eq!(pins[0]["reference"], "J2", "{trace}");
    assert_eq!(pins[0]["pin"], "1", "{trace}");

    let connected = call(
        &handler,
        &sheet,
        "get_connected_items",
        json!({"reference": "J2"}),
    )
    .await;
    let named = if attached {
        json!(["TOLERANT_OUT"])
    } else {
        json!([])
    };
    assert_eq!(connected["nets"], named, "{connected}");
    let connected_labels: Vec<_> = connected["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| &label["net"])
        .collect();
    assert_eq!(json!(connected_labels), named, "{connected}");
    assert_eq!(connected["connected_wires"], 1, "{connected}");

    let args = json!({"net": "TOLERANT_OUT"});
    let connections = call(&handler, &sheet, "get_net_connections", args.clone()).await;
    let pins: Vec<_> = connections["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pin| (&pin["reference"], &pin["pin"]))
        .collect();
    let expected_pins = if attached {
        json!([["J2", "1"]])
    } else {
        json!([])
    };
    assert_eq!(json!(pins), expected_pins, "{connections}");
    assert_eq!(connections["label_count"], 1, "{connections}");
    assert_eq!(
        connections["connected_points"],
        if attached { 3 } else { 1 },
        "{connections}"
    );
    let components = call(&handler, &sheet, "get_net_components", args.clone()).await;
    let components: Vec<_> = components["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                &item["reference"],
                &item["pins"][0]["pin"],
                item["pins"].as_array().unwrap().len(),
            )
        })
        .collect();
    let expected = if attached {
        json!([["J2", "1", 1]])
    } else {
        json!([])
    };
    assert_eq!(json!(components), expected);
    let geometry = call(&handler, &sheet, "get_net_connectivity", args).await;
    let wire_ids: Vec<_> = geometry["wires"]
        .as_array()
        .unwrap()
        .iter()
        .map(|wire| &wire["uuid"])
        .collect();
    let expected = if attached {
        json!(["8ea9bcfd-cb19-41b2-9f24-5cc27b747964"])
    } else {
        json!([])
    };
    assert_eq!(json!(wire_ids), expected, "{geometry}");
}

#[tokio::test]
async fn the_offset_label_no_longer_names_j2s_wire() {
    assert_membership(OFFSET, false).await;
}

#[tokio::test]
async fn an_exact_label_names_j2s_wire() {
    assert_membership(EXACT, true).await;
}

#[tokio::test]
async fn a_label_at_a_distinct_saved_endpoint_coordinate_stays_separate() {
    assert_membership(ENDPOINT_OFFSET, false).await;
}

#[tokio::test]
async fn a_label_rounding_to_the_same_coordinate_names_j2s_wire() {
    assert_membership(&rounded_input(), true).await;
}

#[tokio::test]
async fn the_pin_origin_and_offset_round_before_the_net_lookup() {
    let source = rounded_pin_input(EXACT);
    assert_membership(&source, true).await;
    let report = call(
        &handler().await,
        &Sheet::new(&source),
        "get_pin_connections",
        json!({"reference": "J2", "pin_number": "1"}),
    )
    .await;
    assert_eq!(report["net"], "TOLERANT_OUT", "{report}");
    assert_eq!(report["pin_x"], 190.50008, "{report}");
}

fn rounded_pin_input(source: &str) -> String {
    source
        .replacen("(at 195.58 88.9 0)", "(at 195.58004 88.9 0)", 1)
        .replacen("(at -5.08 0 0)", "(at -5.07996 0 0)", 1)
}

#[tokio::test]
async fn connector_signal_evidence_uses_the_saved_pin_tip() {
    let source = rounded_pin_input(EXACT);
    let report = call(
        &handler().await,
        &Sheet::new(&source),
        "check_connector_pin_signals",
        json!({"references": ["J2"]}),
    )
    .await;
    let pin = &report["connectors"][0]["pins"][0];
    assert_eq!(pin["pin"], "1", "{report}");
    assert_eq!(pin["status"], "identified", "{report}");
    assert_eq!(pin["evidence"], json!(["label"]), "{report}");
    assert_eq!(pin["context"]["nearest_label"]["x"], 165.1, "{report}");
}

fn rounded_input() -> String {
    // KiCad resaves this numeric perturbation to the exact fixture byte for byte.
    EXACT.replacen("(at 165.1 88.9 90)", "(at 165.1 88.90004 90)", 1)
}

#[tokio::test]
async fn an_orthogonal_label_has_a_one_unit_allowance() {
    assert_membership(ONE_UNIT, true).await;
    assert_membership(TWO_UNITS, false).await;
}

async fn net_pin_numbers(source: &str) -> Value {
    let report = call(
        &handler().await,
        &Sheet::new(source),
        "get_net_connections",
        json!({"net": "TOLERANT_OUT"}),
    )
    .await;
    let mut pins: Vec<_> = report["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pin| {
            assert_eq!(pin["reference"], "J2", "{report}");
            pin["pin"].as_str().unwrap()
        })
        .collect();
    pins.sort_unstable();
    json!(pins)
}

#[tokio::test]
async fn a_bare_crossing_does_not_join_j2s_pins() {
    assert_eq!(net_pin_numbers(CROSSING).await, json!(["1"]));
}

#[tokio::test]
async fn an_explicit_junction_joins_j2s_pins() {
    assert_eq!(net_pin_numbers(JUNCTION).await, json!(["1", "2"]));
}

#[tokio::test]
async fn a_junction_one_saved_unit_off_the_trunk_does_not_join_it() {
    assert_eq!(net_pin_numbers(JUNCTION_ONE_UNIT).await, json!(["1"]));
    let trace = call(
        &handler().await,
        &Sheet::new(JUNCTION_ONE_UNIT),
        "trace_from_point",
        json!({"x": 152.4, "y": 88.9001}),
    )
    .await;
    assert_eq!(trace["junctions_here"], json!([{"x": 152.4, "y": 88.9001}]));
}

#[tokio::test]
async fn equal_labels_join_the_two_separate_drawn_islands() {
    assert_eq!(net_pin_numbers(NAME_JOIN).await, json!(["1", "2"]));
    let report = call(
        &handler().await,
        &Sheet::new(NAME_JOIN),
        "check_connector_pin_signals",
        json!({"references": ["J2"]}),
    )
    .await;
    let pins = report["connectors"][0]["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 2, "{report}");
    // The selected mark is on each pin's own drawn island, not on the other
    // island of the merged net.
    assert_eq!(pins[0]["context"]["nearest_label"]["x"], 165.1, "{report}");
    assert_eq!(pins[1]["context"]["nearest_label"]["x"], 177.8, "{report}");
}

#[tokio::test]
async fn pin_queries_summary_and_audit_use_the_corrected_membership() {
    let handler = handler().await;
    for (source, exact) in [(EXACT, true), (OFFSET, false)] {
        let sheet = Sheet::new(source);
        let expected = if exact {
            json!("TOLERANT_OUT")
        } else {
            Value::Null
        };
        let pin = call(
            &handler,
            &sheet,
            "get_pin_connections",
            json!({"reference": "J2", "pin_number": "1"}),
        )
        .await;
        assert_eq!(pin["net"], expected, "{pin}");
        let component = call(
            &handler,
            &sheet,
            "get_component_nets",
            json!({"reference": "J2"}),
        )
        .await;
        assert_eq!(component["pins"][0]["pin"], "1", "{component}");
        assert_eq!(component["pins"][0]["net"], expected, "{component}");
        let summary = call(&handler, &sheet, "export_netlist_summary", json!({})).await;
        assert_eq!(
            summary["components"].as_array().unwrap().len(),
            1,
            "{summary}"
        );
        let pins = summary["components"][0]["pins"].as_array().unwrap();
        assert_eq!(pins[0]["number"], "1", "{summary}");
        assert_eq!(
            pins[0]["net"],
            if exact { "TOLERANT_OUT" } else { "~" },
            "{summary}"
        );
        let single = call(&handler, &sheet, "find_single_pin_nets", json!({})).await;
        assert_eq!(single["single_pin_net_count"], 1, "{single}");
        assert_eq!(single["nets"][0]["net"], "TOLERANT_OUT", "{single}");
        assert_eq!(
            single["nets"][0]["pin_count"],
            usize::from(exact),
            "{single}"
        );
    }
}

#[tokio::test]
async fn only_an_attached_label_is_connector_signal_evidence() {
    let handler = handler().await;
    for (source, status, evidence) in [
        (EXACT, "identified", json!(["label"])),
        (OFFSET, "unidentified", json!([])),
    ] {
        let report = call(
            &handler,
            &Sheet::new(source),
            "check_connector_pin_signals",
            json!({"references": ["J2"]}),
        )
        .await;
        assert_eq!(
            report["connectors"].as_array().unwrap().len(),
            1,
            "{report}"
        );
        let connector = &report["connectors"][0];
        assert_eq!(connector["reference"], "J2", "{report}");
        assert_eq!(connector["pins"][0]["pin"], "1", "{report}");
        assert_eq!(connector["pins"][0]["status"], status, "{report}");
        assert_eq!(connector["pins"][0]["evidence"], evidence, "{report}");
        assert_eq!(connector["pins"][1]["pin"], "2", "{report}");
        assert_eq!(connector["pins"][1]["status"], "unconnected", "{report}");
    }
}

#[tokio::test]
async fn layout_comparison_blocks_detaching_the_label_from_j2() {
    let handler = handler().await;
    let exact = Sheet::new(EXACT);
    let compare = async |source, baseline: &Path| {
        call(
            &handler,
            &Sheet::new(source),
            "compare_schematic_layout",
            json!({"baseline": baseline}),
        )
        .await
    };
    let detached = compare(OFFSET, &exact.path).await;
    assert_eq!(detached["verdict"], "blocked", "{detached}");
    assert_eq!(
        detached["reasons"],
        json!(["the net of [J2 pin 1] would be renamed from TOLERANT_OUT to (unnamed)"]),
        "{detached}"
    );
    let unchanged = compare(EXACT, &exact.path).await;
    assert_eq!(unchanged["verdict"], "unchanged", "{unchanged}");
    exact.assert_unchanged();
}

#[tokio::test]
async fn a_route_cannot_swallow_a_detached_label_already_near_its_source() {
    let body = route_refusal(OFFSET, [165.1, 88.9], [165.1, 96.52]).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("would join label TOLERANT_OUT"),
        "{body}"
    );
}

async fn route_refusal(source: &str, from: [f64; 2], to: [f64; 2]) -> Value {
    let handler = handler().await;
    let sheet = Sheet::new(source);
    let result = served(
        &handler,
        &sheet,
        "add_schematic_connection",
        json!({"x1": from[0], "y1": from[1], "x2": to[0], "y2": to[1]}),
    )
    .await;
    assert_eq!(result["isError"], true, "{result}");
    sheet.assert_unchanged();
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    body
}

#[tokio::test]
async fn a_route_checks_the_serialized_source_point() {
    let body = route_refusal(OFFSET, [165.099_949_9, 88.9], [165.099_949_9, 96.52]).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("would join label TOLERANT_OUT"),
        "{body}"
    );
}

#[tokio::test]
async fn a_route_endpoint_near_a_wire_does_not_authorize_that_net() {
    let body = route_refusal(EXACT, [134.619, 88.9], [190.501, 88.9]).await;
    assert_eq!(body["error"]["reasons"], json!([
        "the straight route would join J2 pin 1 at (190.5, 88.9) on net TOLERANT_OUT, label TOLERANT_OUT at (165.1, 88.9) on net TOLERANT_OUT"
    ]), "{body}");
}

#[tokio::test]
async fn the_serialized_destination_can_authorize_its_net() {
    let handler = handler().await;
    let sheet = Sheet::new(EXACT);
    let result = served(
        &handler,
        &sheet,
        "add_schematic_connection",
        json!({"x1": 190.5, "y1": 90.17, "x2": 190.5, "y2": 88.899_949_9}),
    )
    .await;
    assert_ne!(result["isError"], true, "{result}");
    let source = std::fs::read_to_string(&sheet.path).unwrap();
    let tree = copperpour_sexp::parse_sexp(&source).unwrap();
    let wire = copperpour_sexp::schematic::extract_wires(&tree)
        .into_iter()
        .find(|w| (w.x1, w.y1) == (190.5, 90.17))
        .expect("the written route");
    assert_eq!((wire.x2, wire.y2), (190.5, 88.89995));
}

#[tokio::test]
async fn nearly_aligned_routes_that_would_save_as_diagonal_are_refused() {
    for end in [[120.0377, 20.0007], [20.0007, 120.0377]] {
        let body = route_refusal(EXACT, [20.0, 20.0], end).await;
        assert_eq!(
            body["error"]["reasons"],
            json!(["cannot check what a diagonal straight route would join; align its endpoints in saved coordinates"]),
            "{body}"
        );
    }
}

#[tokio::test]
async fn an_orthogonal_route_cannot_join_a_net_through_a_shallow_diagonal() {
    let body = route_refusal(SHALLOW_WIRE, [162.56, 80.01], [162.56, 91.44]).await;
    assert_eq!(
        body["error"]["reasons"],
        json!(["the straight route cannot be checked on a sheet with a near-axis diagonal wire"]),
        "{body}"
    );
}

#[tokio::test]
async fn nearly_aligned_routes_that_save_as_orthogonal_still_write() {
    let handler = handler().await;
    for end in [[120.0377, 20.00004], [20.00004, 120.0377]] {
        let sheet = Sheet::new(EXACT);
        let result = served(
            &handler,
            &sheet,
            "add_schematic_connection",
            json!({"x1": 20.0, "y1": 20.0, "x2": end[0], "y2": end[1]}),
        )
        .await;
        assert_ne!(result["isError"], true, "{result}");
        let source = std::fs::read_to_string(&sheet.path).unwrap();
        let tree = copperpour_sexp::parse_sexp(&source).unwrap();
        let wire = copperpour_sexp::schematic::extract_wires(&tree)
            .into_iter()
            .find(|w| (w.x1, w.y1) == (20.0, 20.0))
            .expect("the written route");
        assert_eq!([wire.x2, wire.y2], end);
    }
}

#[tokio::test]
async fn fractional_instance_angle_is_truncated_for_electrical_membership() {
    let source = EXACT.replacen("(at 195.58 88.9 0)", "(at 195.58 88.9 0.5)", 1);
    assert_eq!(
        call(
            &handler().await,
            &Sheet::new(&source),
            "get_pin_connections",
            json!({"reference": "J2", "pin_number": "1"}),
        )
        .await["net"],
        "TOLERANT_OUT"
    );
}

#[tokio::test]
async fn single_power_placement_checks_the_saved_pin_not_the_raw_sum() {
    let handler = handler().await;
    let source = rounded_pin_input(POWER_ROUNDED);
    let args = json!({"power_net": "GND", "reference": "J2", "pin_number": "1", "stub_length": 0});
    let report = call(
        &handler,
        &Sheet::new(&source),
        "add_power_symbol",
        args.clone(),
    )
    .await;
    assert_eq!(report["already_connected"], true, "{report}");
    assert_eq!(report["existing_reference"], "#PWR01", "{report}");
    assert_eq!(report["x"], 190.5, "{report}");

    // Match the unrounded sum with a disconnected symbol. The new symbol
    // must actually land on the pin, not skip it or repeat the wrong site.
    let source = rounded_pin_input(POWER_DETACHED);
    let sheet = Sheet::new(&source);
    let result = served(&handler, &sheet, "add_power_symbol", args).await;
    assert_ne!(result["isError"], true, "{result}");
    let written = std::fs::read_to_string(&sheet.path).unwrap();
    let tree = copperpour_sexp::parse_sexp(&written).unwrap();
    let symbols = copperpour_sexp::schematic::extract_symbol_instances(&tree);
    let existing = symbols.iter().find(|s| s.reference == "#PWR01").unwrap();
    let placed = symbols.iter().find(|s| s.reference == "#PWR02").unwrap();
    assert_eq!((existing.x, existing.y), (190.5001, 88.9));
    assert_eq!((placed.x, placed.y), (190.5, 88.9));
}

#[tokio::test]
async fn batch_power_placement_checks_the_saved_pin_not_the_raw_sum() {
    let handler = handler().await;
    let source = rounded_pin_input(POWER_ROUNDED);
    let args = json!({"power_net": "GND", "pins": [{"reference": "J2", "pin_number": "1"}]});
    let report = call(
        &handler,
        &Sheet::new(&source),
        "batch_add_power_symbol",
        args.clone(),
    )
    .await;
    assert_eq!(report["placed_count"], 0, "{report}");
    assert_eq!(report["already_connected"][0]["x"], 190.5, "{report}");
    assert_eq!(
        report["already_connected"][0]["existing_reference"], "#PWR01",
        "{report}"
    );

    // The near symbol is an obstacle, not an already connected pin.
    let source = rounded_pin_input(POWER_DETACHED);
    let sheet = Sheet::new(&source);
    let result = served(&handler, &sheet, "batch_add_power_symbol", args).await;
    assert_eq!(result["isError"], true, "{result}");
    let report: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(report["already_connected"], json!([]), "{report}");
    assert_eq!(report["error"]["kind"], "plan_blocked", "{report}");
    sheet.assert_unchanged();
}

#[tokio::test]
async fn wire_runs_use_electrical_names_without_cutting_at_off_wire_labels() {
    let handler = handler().await;
    for (source, name, count) in [
        (EXACT, json!("TOLERANT_OUT"), 2),
        (ONE_UNIT, json!("TOLERANT_OUT"), 1),
        (TWO_UNITS, Value::Null, 1),
    ] {
        let report = call(
            &handler,
            &Sheet::new(source),
            "check_schematic_wire_runs",
            json!({"long_run_mm": 1}),
        )
        .await;
        let findings = report["findings"].as_array().unwrap();
        assert_eq!(findings.len(), count, "{report}");
        for run in findings {
            assert_eq!(run["net"], name, "{run}");
        }
    }
}
