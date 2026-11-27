use super::project_file_writer_tests::{body_of, served_call};
use super::ServerConfig;
use crate::mcp::handler::McpHandler;
use copperpour_sexp::schematic::{extract_wires, read_schematic};
use serde_json::{json, Value};
use std::path::PathBuf;

const FIXTURE: &str = "route_elbows_kicad10.kicad_sch";
const LEGACY_ELBOW_NAME: &str = "legacy_route_elbows.kicad_sch";
const LEGACY_ELBOW_SEED: &str =
    include_str!("../../tests/fixtures/pin_contact_kicad10/legacy_route_elbows.kicad_sch");
const NEARBY_TAP: &str =
    include_str!("../../tests/fixtures/pin_contact_kicad10/nearby_wire_tap.kicad_sch");
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
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(FIXTURE);
        Self::with_source(FIXTURE, &std::fs::read(source).unwrap()).await
    }

    async fn with_source(name: &str, source: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, source).unwrap();
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

    async fn with_destination_label() -> Self {
        let sheet = Self::new().await;
        let (is_error, body) = sheet
            .call(
                "add_schematic_net_label",
                json!({ "net": "VCC(3V3)", "x": 127, "y": 63.5, "rotation": 180 }),
            )
            .await;
        assert!(!is_error, "{body}");
        sheet
    }

    async fn assert_no_wire_label_overlap(&self) {
        let (is_error, body) = self.call("check_schematic_overlaps", json!({})).await;
        assert!(!is_error, "{body}");
        let findings = body["overlaps"].as_array().unwrap();
        assert!(
            findings
                .iter()
                .all(|item| item["type"] != "wire_label_overlap"),
            "{body}"
        );
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
async fn connect_pins_routes_around_label_text_without_a_wire_label_overlap() {
    let sheet = Sheet::new().await;
    sheet
        .connects("connect_pins", pair("R4", "R5"), &TEXT_PATH)
        .await;
    sheet.assert_no_wire_label_overlap().await;
}

#[tokio::test]
async fn connect_pins_can_route_along_the_destination_label_baseline() {
    let sheet = Sheet::with_destination_label().await;
    let (is_error, route) = sheet.call("connect_pins", pair("R4", "R5")).await;
    assert!(!is_error, "{route}");
    sheet.assert_no_wire_label_overlap().await;
    assert_eq!(route["path"], json!(TEXT_PATH), "{route}");
    sheet.assert_wires(&[&TEXT_PATH]);
}

#[tokio::test]
async fn add_schematic_connection_can_route_along_the_destination_label_baseline() {
    let from = [117.5, 62.5];
    let to = TEXT_PATH[2];
    Sheet::with_destination_label()
        .await
        .connects(
            "add_schematic_connection",
            points(from, to),
            &[from, [from[0], to[1]], to],
        )
        .await;

    // The other elbow crosses the same label, not a second obstacle.
    let sheet = Sheet::with_destination_label().await;
    let corner = [to[0], from[1]];
    for (start, end) in [(from, corner), (corner, to)] {
        let (is_error, body) = sheet.call("add_wire", points(start, end)).await;
        assert!(!is_error, "{body}");
    }
    let (is_error, body) = sheet.call("check_schematic_overlaps", json!({})).await;
    assert!(!is_error, "{body}");
    let findings = body["overlaps"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|item| item["type"] == "wire_label_overlap" && item["b"]["net"] == "VCC(3V3)"),
        "{body}"
    );
}

#[tokio::test]
async fn batch_connect_pins_can_route_along_the_destination_label_baseline() {
    let sheet = Sheet::with_destination_label().await;
    let (is_error, body) = sheet
        .call(
            "batch_connect_pins",
            json!({ "connections": [pair("R4", "R5")] }),
        )
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["connected_count"], 1, "{body}");
    sheet.assert_wires(&[&TEXT_PATH]);
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

#[tokio::test]
async fn connect_pins_does_not_join_legacy_mirrored_pins_at_elbows() {
    assert_legacy_mirrored_pins_block_route("connect_pins").await;
}

#[tokio::test]
async fn batch_connect_pins_does_not_join_legacy_mirrored_pins_at_elbows() {
    assert_legacy_mirrored_pins_block_route("batch_connect_pins").await;
}

async fn assert_legacy_mirrored_pins_block_route(tool: &str) {
    let legacy = LEGACY_ELBOW_SEED
        .replace(
            "(at 41.91 25.4 90)\n\t\t(mirror x)",
            "(at 41.91 25.4 90.5)\n\t\t(mirror x)",
        )
        .replace(
            "(at 21.59 38.1 270)\n\t\t(mirror x)",
            "(at 21.59 38.1 90.5)\n\t\t(mirror y)",
        );
    assert_eq!(legacy.matches("90.5)").count(), 2);
    let reasons = [
        "the horizontal-first route would join R32 pin 1 at (38.1, 25.4)",
        "the vertical-first route would join R33 pin 1 at (25.4, 38.1)",
    ];
    let request = json!({"ref1": "R30", "pin1": "1", "ref2": "R31", "pin2": "1"});
    let sheet = Sheet::with_source(LEGACY_ELBOW_NAME, legacy.as_bytes()).await;
    let args = if tool == "connect_pins" {
        request
    } else {
        json!({"connections": [request]})
    };
    let (is_error, body) = sheet.call(tool, args).await;
    assert!(is_error, "{body}");
    if tool == "connect_pins" {
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert_eq!(body["error"]["reasons"], json!(reasons), "{body}");
    } else {
        assert_eq!(body["connected_count"], 0, "{body}");
        assert_eq!(
            body["errors"],
            json!([format!("R30.1 -> R31.1: {}", reasons.join("; "))]),
            "{body}"
        );
    }
    assert_eq!(std::fs::read_to_string(&sheet.path).unwrap(), legacy);
}

fn first_pin_pair(from: &str, to: &str) -> Value {
    json!({"ref1": from, "pin1": "1", "ref2": to, "pin2": "1"})
}

#[tokio::test]
async fn named_pin_route_does_not_treat_a_nearby_wire_as_a_requested_tap() {
    // The requested vertical route starts at (25.4, 25.4). The nearby wire
    // meets that route at a dot 0.005 mm later, not at its requested start.
    let sheet = Sheet::with_source("nearby_wire_tap.kicad_sch", NEARBY_TAP.as_bytes()).await;
    let (is_error, body) = sheet
        .call("connect_pins", first_pin_pair("R30", "R33"))
        .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    assert_eq!(
        body["error"]["reasons"],
        json!(["the straight route would join label FOREIGN at (20.32, 25.405) on net FOREIGN"]),
        "{body}"
    );
    assert_eq!(std::fs::read_to_string(&sheet.path).unwrap(), NEARBY_TAP);
}

#[tokio::test]
async fn a_later_batch_pair_can_extend_an_earlier_pairs_net() {
    let sheet = Sheet::with_source(LEGACY_ELBOW_NAME, LEGACY_ELBOW_SEED.as_bytes()).await;
    let (is_error, body) = sheet
        .call(
            "batch_connect_pins",
            json!({"connections": [first_pin_pair("R30", "R33"), first_pin_pair("R33", "R31")]}),
        )
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["connected_count"], 2, "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");
    sheet.assert_wires(&[&[[25.4, 25.4], [25.4, 38.1]], &[[25.4, 38.1], [38.1, 38.1]]]);
}

#[tokio::test]
async fn named_pin_routes_refuse_an_unloadable_unrelated_symbol_without_writing() {
    let token = "(at 41.91 25.4 90)\n\t\t(mirror x)";
    assert_eq!(LEGACY_ELBOW_SEED.matches(token).count(), 1);
    let reason = "R32 has a saved symbol angle KiCad cannot load";
    for angle in [" 45", " bad", "", " \"90\"", " -90"] {
        let unloadable =
            LEGACY_ELBOW_SEED.replace(token, &format!("(at 41.91 25.4{angle})\n\t\t(mirror x)"));
        let sheet = Sheet::with_source(LEGACY_ELBOW_NAME, unloadable.as_bytes()).await;
        let pair = first_pin_pair("R30", "R33");
        for (tool, args) in [
            ("connect_pins", pair.clone()),
            ("batch_connect_pins", json!({"connections": [pair]})),
        ] {
            let (is_error, body) = sheet.call(tool, args).await;
            assert!(is_error, "{tool} with {angle:?}: {body}");
            if tool == "connect_pins" {
                assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
                assert_eq!(body["error"]["reasons"], json!([reason]), "{body}");
            } else {
                assert_eq!(body["connected_count"], 0, "{body}");
                assert_eq!(
                    body["errors"],
                    json!([format!("R30.1 -> R33.1: {reason}")]),
                    "{body}"
                );
            }
            assert_eq!(std::fs::read_to_string(&sheet.path).unwrap(), unloadable);
        }
    }
}
