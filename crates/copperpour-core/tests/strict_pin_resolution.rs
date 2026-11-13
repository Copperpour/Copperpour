//! #415: resolve named pins consistently through the served tool catalogue.
use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};
use std::path::Path;

const SHEET: &str = include_str!("fixtures/power_on_pin_kicad10.kicad_sch");

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

fn sheet(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("power_on_pin_kicad10.kicad_sch");
    std::fs::write(&path, SHEET).unwrap();
    path
}

async fn call(h: &McpHandler, tool: &str, path: &Path, mut args: Value) -> Value {
    args["schematic"] = json!(path);
    let response = h
        .handle_message(json!({"jsonrpc":"2.0", "id":415,
        "method":"tools/call", "params":{"name":tool,"arguments":args}}))
        .await
        .unwrap();
    let result = response.result.expect("JSON-RPC success");
    let mut body: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    body["is_error"] = json!(result["isError"] == true);
    body
}

#[tokio::test]
async fn analysis_refuses_ambiguous_and_stale_pins_through_both_names() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = sheet(dir.path());
    for tool in ["get_pin_connections", "get_pin_net_name"] {
        for (reference, pin, kind) in [
            ("R4", "1", "ambiguous_target"),
            ("R99", "1", "stale_target"),
            ("R1", "99", "stale_target"),
        ] {
            let body = call(
                &h,
                tool,
                &path,
                json!({"reference":reference,"pin_number":pin}),
            )
            .await;
            assert_eq!(body["is_error"], true, "{tool}: {body}");
            assert_eq!(body["error"]["kind"], kind, "{tool}: {body}");
            assert_eq!(body["error"]["target"], format!("{reference} pin {pin}"));
            if reference == "R4" {
                let candidates = body["error"]["candidates"].as_array().unwrap();
                assert_eq!(candidates.len(), 2);
                for (candidate, point) in candidates
                    .iter()
                    .zip(["(180.34, 116.84)", "(200.66, 116.84)"])
                {
                    let candidate = candidate.as_str().unwrap();
                    assert!(
                        candidate.starts_with("unit 1 ") && candidate.ends_with(point),
                        "{body}"
                    );
                }
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
        }
    }
}

#[tokio::test]
async fn bus_skips_ambiguous_and_stale_pins_without_writing() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = sheet(dir.path());
    for (reference, pin, reason) in [
        ("R4", "1", "names more than one placed pin"),
        ("R99", "1", "component R99 is not present"),
        ("R1", "99", "has no pin 99"),
    ] {
        let body = call(&h, "connect_pins_to_bus", &path, json!({"bus_y":60.0,"connections":[{"reference":reference,"pin_number":pin,"net":"SIG"}]})).await;
        assert_eq!(body["connected_count"], 0, "{body}");
        assert_eq!(body["errors"].as_array().unwrap().len(), 1);
        assert!(
            body["errors"][0].as_str().unwrap().contains(reason),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
    }
}

#[tokio::test]
async fn bus_keeps_valid_items_and_analysis_observes_the_written_net() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = sheet(dir.path());
    // Literal positions from KiCad ERC, documented in the fixture README.
    for (reference, pin, x, y) in [
        ("R1", "2", 100.33, 83.82),
        ("U1", "8", 137.16, 113.03),
        ("U2", "3", 248.92, 120.65),
    ] {
        let body = call(
            &h,
            "connect_pins_to_bus",
            &path,
            json!({"bus_y":60.0,"connections":[
            {"reference":"R4","pin_number":"1","net":"WRONG"},
            {"reference":reference,"pin_number":pin,"net":"SIG"}]}),
        )
        .await;
        assert_eq!(body["connected_count"], 1, "{body}");
        assert_eq!(body["errors"].as_array().unwrap().len(), 1);
        assert_eq!(body["connected"][0]["reference"], reference);
        assert_eq!(body["connected"][0]["pin_at"], json!({"x":x,"y":y}));
        for tool in ["get_pin_connections", "get_pin_net_name"] {
            let observed = call(
                &h,
                tool,
                &path,
                json!({"reference":reference,"pin_number":pin}),
            )
            .await;
            assert_eq!(observed["is_error"], false, "{tool}: {observed}");
            assert_eq!(observed["pin_x"], x);
            assert_eq!(observed["pin_y"], y);
            assert_eq!(observed["net"], "SIG");
        }
        let tree = copperpour_sexp::parse_sexp(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let labels = copperpour_sexp::schematic::extract_all_net_labels(&tree);
        assert!(labels.iter().any(|label| label.net == "SIG"
            && (label.x - x).abs() < 0.001
            && (label.y - y).abs() < 0.001));
        assert!(!labels.iter().any(|label| label.net == "WRONG"));
    }
}

#[tokio::test]
async fn bus_and_analysis_resolve_a_sheet_local_library_name() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("power_on_off_grid_pin.kicad_sch");
    std::fs::write(
        &path,
        include_str!("fixtures/power_on_off_grid_pin.kicad_sch"),
    )
    .unwrap();
    // KiCad's stock hv_converter demo: D2 uses lib_name DIODE_5.
    let body = call(
        &h,
        "connect_pins_to_bus",
        &path,
        json!({"bus_y":80.0,"connections":[
        {"reference":"D2","pin_number":"2","net":"DERIVED"}]}),
    )
    .await;
    assert_eq!(body["connected_count"], 1, "{body}");
    assert_eq!(body["errors"], json!([]));
    assert_eq!(
        body["connected"][0]["pin_at"],
        json!({"x":131.572,"y":96.52})
    );
    for tool in ["get_pin_connections", "get_pin_net_name"] {
        let observed = call(&h, tool, &path, json!({"reference":"D2","pin_number":"2"})).await;
        assert_eq!(observed["is_error"], false, "{observed}");
        assert_eq!(observed["pin_x"], 131.572);
        assert_eq!(observed["pin_y"], 96.52);
        // Existing demo labels may outrank DERIVED; resolution must still
        // return the effective definition's physical point.
    }
}
