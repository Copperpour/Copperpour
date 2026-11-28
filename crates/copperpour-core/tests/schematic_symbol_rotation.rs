//! Symbol angle writes through the served MCP boundary.

use copperpour_core::mcp::handler::McpHandler;
use copperpour_core::tools::ServerConfig;
use copperpour_sexp::schematic::extract_symbol_instances;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SHEET: &str = include_str!("fixtures/pin_contact_kicad10/pin_contact.kicad_sch");
const POWER_ROWS: &str = include_str!("fixtures/power_rows_kicad10.kicad_sch");
const QUARTER_TURN_REASON: &str = "must be a multiple of 90 degrees";

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

fn sheet(source: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    fs::write(&path, source).unwrap();
    (dir, path)
}

async fn serve(handler: &McpHandler, path: &Path, tool: &str, mut args: Value) -> (bool, Value) {
    args["schematic"] = json!(path);
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 836, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .await
        .unwrap()
        .result
        .unwrap();
    (
        response["isError"] == true,
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap(),
    )
}

fn placement(reference: &str, rotation: f64, mirror: &str) -> Value {
    json!({
        "lib_id": "Device:R", "reference": reference, "x": 124.46, "y": 140.97,
        "rotation": rotation, "mirror": mirror,
    })
}

fn snapshot(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

fn assert_invalid(error: bool, body: &Value, field: &str) {
    assert!(error, "{body}");
    assert_eq!(
        body["error"],
        json!({"kind": "invalid_argument", "field": field, "reason": QUARTER_TURN_REASON}),
        "{body}"
    );
}

fn stored(path: &Path, reference: &str) -> Vec<(u32, f64, bool, bool)> {
    let tree = copperpour_sexp::parse_sexp(&fs::read_to_string(path).unwrap()).unwrap();
    extract_symbol_instances(&tree)
        .into_iter()
        .filter(|s| s.reference == reference)
        .map(|s| (s.unit, s.rotation, s.mirror_x, s.mirror_y))
        .collect()
}

#[tokio::test]
async fn symbol_rotation_single_and_batch_placement_refuse_non_quarter_angles() {
    let handler = handler().await;
    for angle in [45.0, 90.5] {
        for mirror in ["none", "x", "y"] {
            let (_dir, path) = sheet(SHEET, "pin_contact.kicad_sch");
            let candidate = placement("R30", angle, mirror);
            let before = snapshot(&path);
            let (error, body) = serve(
                &handler,
                &path,
                "add_schematic_component",
                candidate.clone(),
            )
            .await;
            assert_invalid(error, &body, "rotation");
            assert!(snapshot(&path) == before, "single: {angle}, {mirror}");
            let (error, body) = serve(
                &handler,
                &path,
                "batch_place_components",
                json!({"components": [candidate]}),
            )
            .await;
            assert!(error, "{body}");
            assert_eq!(body["placed"], json!([]), "{body}");
            let failures = body["failures"].as_array().unwrap();
            assert_eq!(failures.len(), 1, "{body}");
            assert_eq!(failures[0]["index"], 0, "{body}");
            assert_eq!(failures[0]["reference"], "R30", "{body}");
            assert_eq!(failures[0]["kind"], "invalid_argument", "{body}");
            let reason: Value =
                serde_json::from_str(failures[0]["message"].as_str().unwrap()).unwrap();
            assert_invalid(true, &reason, "rotation");
            assert!(snapshot(&path) == before, "batch: {angle}, {mirror}");
        }
    }
}

#[tokio::test]
async fn symbol_rotation_batch_refusal_keeps_valid_sibling_and_does_not_reserve_its_reference() {
    let handler = handler().await;
    let (_dir, path) = sheet(SHEET, "pin_contact.kicad_sch");
    let (error, body) = serve(
        &handler,
        &path,
        "batch_place_components",
        json!({"components": [placement("R30", 90.5, "x"), placement("R30", 450.0, "y")]}),
    )
    .await;
    assert!(!error, "{body}");
    assert_eq!(body["placed_count"], 1, "{body}");
    assert_eq!(body["placed"][0]["reference"], "R30", "{body}");
    assert_eq!(
        body["placed"][0]["rotation"].as_f64(),
        Some(270.0),
        "{body}"
    );
    assert_eq!(body["placed"][0]["mirror_x"], true, "{body}");
    assert_eq!(body["failures"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["failures"][0]["index"], 0, "{body}");
    assert_eq!(body["failures"][0]["kind"], "invalid_argument", "{body}");
    assert_eq!(stored(&path, "R30"), [(1, 270.0, true, false)]);
}

#[tokio::test]
async fn symbol_rotation_refuses_invalid_request_and_rotates_canonical_orientations() {
    let handler = handler().await;
    for (mirror, want) in [
        ("none", (1, 90.0, false, false)),
        ("x", (1, 90.0, true, false)),
        ("y", (1, 270.0, true, false)),
    ] {
        let (_dir, path) = sheet(SHEET, "pin_contact.kicad_sch");
        let (error, body) = serve(
            &handler,
            &path,
            "add_schematic_component",
            placement("R30", 0.0, mirror),
        )
        .await;
        assert!(!error, "{body}");
        let before = snapshot(&path);
        for angle in [45.0, 90.5] {
            let (error, body) = serve(
                &handler,
                &path,
                "rotate_schematic_component",
                json!({"reference": "R30", "rotation": angle}),
            )
            .await;
            assert_invalid(error, &body, "rotation");
            assert!(snapshot(&path) == before, "{mirror}: {angle}");
        }
        let (error, body) = serve(
            &handler,
            &path,
            "rotate_schematic_component",
            json!({"reference": "R30", "rotation": 450}),
        )
        .await;
        assert!(!error, "{body}");
        assert_eq!(body["rotation"].as_f64(), Some(want.1), "{body}");
        assert_eq!(stored(&path, "R30"), [want]);
    }
}

#[tokio::test]
async fn symbol_rotation_power_point_refuses_invalid_angles() {
    check_power_rotation(json!({"power_net": "GND", "x": 127.0, "y": 152.4})).await;
}

#[tokio::test]
async fn symbol_rotation_power_pin_refuses_invalid_angles() {
    check_power_rotation(
        json!({"power_net": "GND", "reference": "R20", "pin_number": "2", "stub_length": 0}),
    )
    .await;
    check_power_rotation(json!({"power_net": "GND", "reference": "R20", "pin_number": "2"})).await;
}

async fn check_power_rotation(selector: Value) {
    let handler = handler().await;
    let (_dir, path) = sheet(SHEET, "pin_contact.kicad_sch");
    let before = snapshot(&path);
    for angle in [45.0, 90.5] {
        let mut args = selector.clone();
        args["rotation"] = json!(angle);
        let (error, body) = serve(&handler, &path, "add_power_symbol", args).await;
        assert_invalid(error, &body, "rotation");
        assert!(snapshot(&path) == before, "{selector}: {angle}");
    }
    let mut args = selector;
    args["rotation"] = json!(-90);
    let (error, body) = serve(&handler, &path, "add_power_symbol", args).await;
    assert!(!error, "{body}");
    assert_eq!(body["reference"], "#PWR06", "{body}");
    assert_eq!(body["rotation"].as_f64(), Some(270.0), "{body}");
    assert_eq!(stored(&path, "#PWR06"), [(1, 270.0, false, false)]);
}

#[tokio::test]
async fn symbol_rotation_batch_power_refuses_used_override_but_ignores_one_on_a_bar() {
    let handler = handler().await;
    let (_dir, path) = sheet(POWER_ROWS, "power_rows_kicad10.kicad_sch");
    let before = snapshot(&path);
    let c2 = json!({"reference": "C2", "pin_number": "2"});
    for angle in [45.0, 90.5] {
        let c1 = json!({"reference": "C1", "pin_number": "2", "rotation": angle});
        let (error, body) = serve(
            &handler,
            &path,
            "batch_add_power_symbol",
            json!({"power_net": "GND", "max_gap": 0, "pins": [c2, c1]}),
        )
        .await;
        assert_invalid(error, &body, "pins[1].rotation");
        assert!(snapshot(&path) == before, "no stubs written for {angle}");
    }
    let (error, body) = serve(
        &handler, &path, "batch_add_power_symbol",
        json!({"power_net": "GND", "pins": [{"reference": "C1", "pin_number": "2", "rotation": 90.5}, c2]}),
    ).await;
    assert!(!error, "{body}");
    assert_eq!(body["placed_count"], 1, "{body}");
    let group = &body["groups"][0];
    assert_eq!(group["joined"], true, "{body}");
    assert_eq!(
        group["ignored_rotations"],
        json!([{"reference": "C1", "pin_number": "2"}]),
        "{body}"
    );
    assert_eq!(group["symbol"]["rotation"].as_f64(), Some(0.0), "{body}");
    assert_eq!(
        stored(&path, group["symbol"]["reference"].as_str().unwrap()),
        [(1, 0.0, false, false)]
    );
}

#[tokio::test]
async fn symbol_rotation_legacy_angle_blocks_transforms() {
    // The noncanonical parser input from #810. The KiCad resave removes
    // precisely the angle this test needs to read and refuse.
    let legacy = SHEET.replacen("(at 59.69 50.8 90)", "(at 59.69 50.8 90.5)", 1);
    let handler = handler().await;
    let (_dir, path) = sheet(&legacy, "pin_contact.kicad_sch");
    assert_eq!(stored(&path, "R1"), [(1, 90.5, false, false)]);
    let before = snapshot(&path);
    for (tool, extra) in [
        ("mirror_schematic_component", json!({"mirror": "x"})),
        ("mirror_schematic_component", json!({"mirror": "y"})),
        ("rotate_schematic_component", json!({"rotation": 90})),
        (
            "rotate_schematic_component",
            json!({"rotation": 90, "autoplace_fields": true}),
        ),
    ] {
        let mut args = extra;
        args["reference"] = json!("R1");
        let (error, body) = serve(&handler, &path, tool, args).await;
        assert!(error, "{tool}: {body}");
        assert_eq!(
            body["error"],
            json!({"kind": "plan_blocked", "operation": tool, "reasons": [
                "R1 unit 1 has a saved rotation of 90.5 degrees. Open the sheet in KiCad, correct or resave its orientation, and retry."
            ]}),
        );
        assert!(snapshot(&path) == before, "{tool} changed the sheet");
    }
}
