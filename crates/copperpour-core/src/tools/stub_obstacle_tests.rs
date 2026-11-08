//! #117: a `connect_to_net` stub joins nothing beyond the net it names.
//!
//! The fixture and KiCad's netlist of each result are in
//! `tests/fixtures/stub_obstacles_kicad10.README.md`.

use super::ServerConfig;
use crate::mcp::handler::McpHandler;
use copperpour_sexp::schematic::{extract_junctions, extract_wires, read_schematic};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/stub_obstacles_kicad10.kicad_sch");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stub_obstacles_kicad10.kicad_sch");
    std::fs::copy(src, &path).unwrap();
    (dir, path)
}

async fn call(path: &Path, name: &str, mut arguments: Value) -> (bool, Value) {
    let handler = McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..ServerConfig::default()
    })
    .await
    .expect("handler builds");
    arguments["schematic"] = json!(path.display().to_string());
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 117, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    let result = response.result.expect("successful JSON-RPC response");
    let body = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    (result["isError"] == json!(true), body)
}

/// The fixture has no wires, so every wire on the sheet is one the call drew.
fn wires(path: &Path) -> Vec<[f64; 4]> {
    let (_, tree) = read_schematic(path).unwrap();
    let mut wires: Vec<[f64; 4]> = extract_wires(&tree)
        .into_iter()
        .map(|w| [w.x1, w.y1, w.x2, w.y2])
        .collect();
    wires.sort_by(|a, b| a.partial_cmp(b).unwrap());
    wires
}

fn junctions(path: &Path) -> Vec<(f64, f64)> {
    extract_junctions(&read_schematic(path).unwrap().1)
}

fn reasons(body: &Value) -> Vec<&str> {
    body["error"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect()
}

fn pin(reference: &str, pin_number: &str) -> Value {
    json!({ "reference": reference, "pin_number": pin_number })
}

/// What stands at the end of each of R5.2's three stubs.
const BOXED: [&str; 3] = [
    "the down stub would join R6 pin 1 and label OTHER at (101.6, 57.15) on net OTHER",
    "the right stub would join R7 pin 1 at (104.14, 54.61)",
    "the left stub would join R8 pin 1 at (99.06, 54.61)",
];
const R1_DOWN: &str = "the down stub would join R2 pin 1 at (50.8, 57.15)";

async fn assert_refused(path: &Path, args: Value, expected: &[&str]) {
    let before = std::fs::read(path).unwrap();
    let (is_error, body) = call(path, "connect_to_net", args).await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    assert_eq!(body["error"]["operation"], "connect_to_net", "{body}");
    assert_eq!(reasons(&body), expected);
    assert_eq!(
        std::fs::read(path).unwrap(),
        before,
        "a refusal writes nothing"
    );
}

#[tokio::test]
async fn connect_to_net_turns_away_from_a_pin_at_the_stub_end() {
    let (_dir, path) = fixture();
    let args = json!({ "reference": "R1", "pin_number": "2", "net": "N2" });
    let (is_error, body) = call(&path, "connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["direction"], "right", "{body}");
    assert_eq!(
        body["label"],
        json!({ "x": 53.34, "y": 54.61, "rotation": 0.0 })
    );
    assert_eq!(wires(&path), [[50.8, 54.61, 53.34, 54.61]]);
    assert_eq!(junctions(&path), []);
}

#[tokio::test]
async fn connect_to_net_by_coordinates_turns_away_too() {
    let (_dir, path) = fixture();
    let args = json!({ "pin_x": 50.8, "pin_y": 54.61, "net": "N2" });
    let (is_error, body) = call(&path, "connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["direction"], "right", "{body}");
    assert_eq!(wires(&path), [[50.8, 54.61, 53.34, 54.61]]);
}

#[tokio::test]
async fn connect_to_net_turns_away_from_a_pin_the_stub_runs_over() {
    let (_dir, path) = fixture();
    let args = json!({ "reference": "R3", "pin_number": "2", "net": "N3", "stub_length": 7.62 });
    let (is_error, body) = call(&path, "connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["direction"], "right", "{body}");
    assert_eq!(wires(&path), [[76.2, 54.61, 83.82, 54.61]]);
    assert_eq!(junctions(&path), [], "R4.1 gets no junction");
}

#[tokio::test]
async fn connect_to_net_refuses_a_named_direction_that_joins_a_pin() {
    let (_dir, path) = fixture();
    let args = json!({ "reference": "R1", "pin_number": "2", "net": "N2", "direction": "down" });
    assert_refused(&path, args, &[R1_DOWN]).await;
}

#[tokio::test]
async fn connect_to_net_refuses_when_every_direction_joins_another_net() {
    let (_dir, path) = fixture();
    // No `up`: an auto stub never runs back over its own symbol.
    let args = json!({ "reference": "R5", "pin_number": "2", "net": "N5" });
    assert_refused(&path, args, &BOXED).await;
}

#[tokio::test]
async fn connect_to_net_may_end_on_a_pin_already_on_the_net() {
    let (_dir, path) = fixture();
    // R10.1 carries label N, so joining it is what the call asks for.
    let args = json!({ "reference": "R9", "pin_number": "2", "net": "N" });
    let (is_error, body) = call(&path, "connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["direction"], "down", "{body}");
    assert_eq!(wires(&path), [[127.0, 54.61, 127.0, 57.15]]);
}

#[tokio::test]
async fn connect_to_net_refuses_when_pins_cannot_be_resolved() {
    let (_dir, path) = fixture();
    // Without the embedded Device:R, no pin on the sheet can be located.
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text.find("(lib_symbols").unwrap();
    let end = copperpour_sexp::writer::find_balanced_block(&text, start)
        .unwrap()
        .1;
    std::fs::write(
        &path,
        format!("{}(lib_symbols){}", &text[..start], &text[end..]),
    )
    .unwrap();
    // The promised wording, restated rather than read from the code under test.
    let args = json!({ "pin_x": 127.0, "pin_y": 54.61, "net": "N9" });
    assert_refused(
        &path,
        args,
        &["cannot check what the stub would join: one or more placed symbols have unresolved library pin geometry"],
    )
    .await;
}

#[tokio::test]
async fn batch_connect_to_net_skips_only_the_blocked_stubs() {
    let (_dir, path) = fixture();
    let args = json!({
        "net_name": "N2", "stub_length": 2.54, "direction": "down",
        "pins": [pin("R1", "2"), pin("R3", "2"), pin("R5", "2")]
    });
    let (is_error, body) = call(&path, "batch_connect_to_net", args).await;
    assert!(!is_error, "{body}");
    let added: Vec<&str> = body["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["reference"].as_str().unwrap())
        .collect();
    assert_eq!(added, ["R3"], "{body}");
    assert_eq!(
        body["errors"],
        json!([format!("R1.2: {R1_DOWN}"), format!("R5.2: {}", BOXED[0])])
    );
    assert_eq!(wires(&path), [[76.2, 54.61, 76.2, 57.15]]);
}

#[tokio::test]
async fn batch_connect_to_net_turns_an_auto_stub_away() {
    let (_dir, path) = fixture();
    let args =
        json!({ "net_name": "N5", "stub_length": 2.54, "pins": [pin("R1", "2"), pin("R5", "2")] });
    let (is_error, body) = call(&path, "batch_connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["added"][0]["direction"], "right", "{body}");
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors, &[json!(format!("R5.2: {}", BOXED.join("; ")))]);
    assert_eq!(wires(&path), [[50.8, 54.61, 53.34, 54.61]]);
}

#[tokio::test]
async fn batch_connect_to_net_may_end_on_another_pin_it_names() {
    let (_dir, path) = fixture();
    let args = json!({
        "net_name": "N2", "stub_length": 2.54, "direction": "down",
        "pins": [pin("R1", "2"), pin("R2", "1")]
    });
    let (is_error, body) = call(&path, "batch_connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");
    assert_eq!(
        wires(&path),
        [[50.8, 54.61, 50.8, 57.15], [50.8, 57.15, 50.8, 59.69]]
    );
}

#[tokio::test]
async fn a_stub_turned_up_keeps_its_label_reading_away_from_the_symbol() {
    let (_dir, path) = fixture();
    // R11.1 faces left; its left stub would end on R12.1.
    let args = json!({ "reference": "R11", "pin_number": "1", "net": "N11" });
    let (is_error, body) = call(&path, "connect_to_net", args).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["direction"], "up", "{body}");
    assert_eq!(
        body["label"],
        json!({ "x": 148.59, "y": 48.26, "rotation": 180.0 })
    );
    assert_eq!(wires(&path), [[148.59, 50.8, 148.59, 48.26]]);
}

#[tokio::test]
async fn batch_connect_to_net_with_every_stub_blocked_is_an_error_and_writes_nothing() {
    let (_dir, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    let args = json!({
        "net_name": "N2", "stub_length": 2.54, "direction": "down",
        "pins": [pin("R1", "2"), pin("R5", "2")]
    });
    let (is_error, body) = call(&path, "batch_connect_to_net", args).await;
    assert!(is_error, "{body}");
    assert_eq!(body["added"], json!([]), "{body}");
    assert_eq!(body["errors"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
