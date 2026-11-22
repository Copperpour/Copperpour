use super::project_file_writer_tests::{body_of, served_call};
use super::ServerConfig;
use crate::mcp::handler::McpHandler;
use copperpour_sexp::schematic::{extract_wires, read_schematic};
use serde_json::{json, Value};
use std::path::PathBuf;

const FIXTURE: &str = "route_elbows_kicad10.kicad_sch";
type Point = [f64; 2];
const BODY_PATH: [Point; 3] = [[50.8, 54.61], [50.8, 63.5], [76.2, 63.5]];
const TEXT_PATH: [Point; 3] = [[101.6, 54.61], [101.6, 63.5], [127.0, 63.5]];
const CLEAR_PATH: [Point; 3] = [[50.8, 105.41], [76.2, 105.41], [76.2, 114.3]];
const STRAIGHT_PATH: [Point; 2] = [[101.6, 105.41], [101.6, 115.57]];
const FALLBACK_PATH: [Point; 3] = [[50.8, 143.51], [76.2, 143.51], [76.2, 152.4]];
const BLOCKED_REASONS: [&str; 2] = [
    "the horizontal-first route would join R18 pin 1 at (114.3, 143.51)",
    "the vertical-first route would join R19 pin 1 at (120.65, 152.4)",
];

struct Sheet {
    _dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
}

impl Sheet {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FIXTURE);
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(FIXTURE),
            &path,
        )
        .unwrap();
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            path,
            handler,
        }
    }

    async fn call(&self, tool: &str, mut args: Value) -> (bool, Value) {
        args["schematic"] = json!(self.path);
        let result = served_call(&self.handler, tool, args).await;
        (result.is_error, body_of(&result))
    }

    async fn connects(&self, tool: &str, args: Value, expected: &[Point]) {
        let (is_error, body) = self.call(tool, args).await;
        assert!(!is_error, "{body}");
        assert_eq!(body["path"], json!(expected), "{body}");
        self.assert_wires(&[expected]);
    }

    fn assert_wires(&self, paths: &[&[Point]]) {
        let (_, tree) = read_schematic(&self.path).unwrap();
        let wires: Vec<_> = extract_wires(&tree)
            .into_iter()
            .map(|w| [[w.x1, w.y1], [w.x2, w.y2]])
            .collect();
        let legs: Vec<_> = paths
            .iter()
            .flat_map(|path| path.windows(2).map(|leg| [leg[0], leg[1]]))
            .collect();
        assert_eq!(wires, legs);
    }
}

fn pair(ref1: &str, ref2: &str) -> Value {
    json!({ "ref1": ref1, "pin1": "2", "ref2": ref2, "pin2": "1" })
}

fn points([x1, y1]: Point, [x2, y2]: Point) -> Value {
    json!({ "x1": x1, "y1": y1, "x2": x2, "y2": y2 })
}

#[tokio::test]
async fn connect_pins_routes_around_r3_without_a_wire_overlap() {
    let sheet = Sheet::new().await;
    sheet
        .connects("connect_pins", pair("R1", "R2"), &BODY_PATH)
        .await;
    let (is_error, body) = sheet.call("check_schematic_overlaps", json!({})).await;
    assert!(!is_error, "{body}");
    assert!(
        body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "wire_overlap"),
        "{body}"
    );
}

#[tokio::test]
async fn connect_pins_routes_around_label_text_without_touching_its_anchor() {
    Sheet::new()
        .await
        .connects("connect_pins", pair("R4", "R5"), &TEXT_PATH)
        .await;
}

#[tokio::test]
async fn add_schematic_connection_prioritizes_body_clearance_over_text() {
    // Horizontal-first crosses only R6's body; vertical-first crosses its Reference.
    let from = [110.0, 60.5];
    let to = [117.5, 57.6];
    Sheet::new()
        .await
        .connects(
            "add_schematic_connection",
            points(from, to),
            &[from, [110.0, 57.6], to],
        )
        .await;
}

#[tokio::test]
async fn add_schematic_connection_routes_around_a_visible_field() {
    // The horizontal leg crosses only R6's Reference, above its body.
    let from = [108.0, 57.6];
    let to = [120.0, 55.0];
    Sheet::new()
        .await
        .connects(
            "add_schematic_connection",
            points(from, to),
            &[from, [108.0, 55.0], to],
        )
        .await;
}

#[tokio::test]
async fn batch_connect_pins_routes_the_same_pairs_and_skips_the_blocked_pair() {
    let sheet = Sheet::new().await;
    let (is_error, body) = sheet
        .call(
            "batch_connect_pins",
            json!({ "connections": [
                pair("R1", "R2"), pair("R4", "R5"), pair("R7", "R8"),
                pair("R9", "R10"), pair("R12", "R13"), pair("R16", "R17")
            ] }),
        )
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["connected_count"], 5, "{body}");
    assert_eq!(
        body["errors"],
        json!([format!("R16.2 -> R17.1: {}", BLOCKED_REASONS.join("; "))])
    );
    sheet.assert_wires(&[
        &BODY_PATH,
        &TEXT_PATH,
        &CLEAR_PATH,
        &STRAIGHT_PATH,
        &FALLBACK_PATH,
    ]);
}

#[tokio::test]
async fn clear_elbows_tie_in_favor_of_horizontal_first() {
    Sheet::new()
        .await
        .connects("connect_pins", pair("R7", "R8"), &CLEAR_PATH)
        .await;
}

#[tokio::test]
async fn a_straight_route_stays_straight_through_a_body() {
    Sheet::new()
        .await
        .connects("connect_pins", pair("R9", "R10"), &STRAIGHT_PATH)
        .await;
}

#[tokio::test]
async fn a_clearer_vertical_elbow_cannot_join_another_net() {
    Sheet::new()
        .await
        .connects("connect_pins", pair("R12", "R13"), &FALLBACK_PATH)
        .await;
}

#[tokio::test]
async fn two_unsafe_elbows_keep_reason_order_even_when_vertical_is_clearer() {
    let sheet = Sheet::new().await;
    let before = std::fs::read(&sheet.path).unwrap();
    let (is_error, body) = sheet.call("connect_pins", pair("R12", "R17")).await;
    assert_eq!(std::fs::read(&sheet.path).unwrap(), before, "{body}");
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    let reasons = body["error"]["reasons"].as_array().unwrap();
    assert_eq!(reasons.len(), 2, "{body}");
    assert!(
        reasons[0]
            .as_str()
            .unwrap()
            .starts_with("the horizontal-first route would join "),
        "{body}"
    );
    assert!(
        reasons[1]
            .as_str()
            .unwrap()
            .starts_with("the vertical-first route would join "),
        "{body}"
    );
}
