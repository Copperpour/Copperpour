use super::stub_obstacle_tests::call;
use copperpour_sexp::schematic::{
    extract_junctions, extract_labels, extract_wires, read_schematic,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/authoring_label_clearance_kicad10/amp_stage.kicad_sch");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amp_stage.kicad_sch");
    std::fs::copy(source, &path).unwrap();
    (dir, path)
}

async fn success(path: &Path, tool: &str, args: Value) -> Value {
    let (is_error, body) = call(path, tool, args).await;
    assert!(!is_error, "{tool}: {body}");
    body
}

async fn connect_j1_pin2(path: &Path, mut options: Value) -> Value {
    options["reference"] = json!("J1");
    options["pin_number"] = json!("2");
    options["net"] = json!("SIG_OUT");
    success(path, "connect_to_net", options).await
}

async fn assert_j1_pin2_refused(direction: &str) {
    let (_dir, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    let (is_error, body) = call(
        &path,
        "connect_to_net",
        json!({
            "reference": "J1", "pin_number": "2", "net": "SIG_OUT", "direction": direction
        }),
    )
    .await;
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
}

#[tokio::test]
async fn a_stub_up_onto_a_neighbouring_j1_pin_writes_nothing() {
    assert_j1_pin2_refused("up").await;
}

#[tokio::test]
async fn a_stub_down_onto_a_neighbouring_j1_pin_writes_nothing() {
    assert_j1_pin2_refused("down").await;
}

async fn new_findings(path: &Path, before: &[Value]) -> Vec<Value> {
    let after = success(path, "check_schematic_overlaps", json!({})).await;
    after["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| !before.contains(finding))
        .cloned()
        .collect()
}

fn assert_right_stub(result: &Value, end_x: f64, length: f64) {
    assert_eq!(result["direction"], "right");
    assert_eq!(
        result["wire"],
        json!({ "x1": 93.98, "y1": 91.44, "x2": end_x, "y2": 91.44 })
    );
    assert_eq!(
        result["label"],
        json!({ "x": end_x, "y": 91.44, "rotation": 0.0 })
    );
    assert_eq!(result["stub_length"], length);
}

#[test]
fn the_schema_leaves_an_omitted_length_delegated() {
    let tool = super::sch_wiring::tools()
        .into_iter()
        .find(|tool| tool.name == "connect_to_net")
        .unwrap();
    let schema = tool.input_schema["properties"]["stub_length"]
        .as_object()
        .unwrap();
    assert!(!schema.contains_key("default"), "{schema:?}");
}

#[tokio::test]
async fn a_delegated_stub_keeps_its_label_off_existing_text() {
    let (_dir, path) = fixture();
    let before = new_findings(&path, &[]).await;
    let result = connect_j1_pin2(&path, json!({})).await;

    assert_right_stub(&result, 99.06, 5.08);
    assert_eq!(result["text_overlaps"], json!([]));
    assert_eq!(result["bounds_unresolved"], json!([]));
    let (_, tree) = read_schematic(&path).unwrap();
    assert!(extract_labels(&tree)
        .iter()
        .any(|label| label.net == "SIG_OUT" && (label.x, label.y) == (99.06, 91.44)));
    assert_eq!(new_findings(&path, &before).await, Vec::<Value>::new());
}

#[tokio::test]
async fn an_explicit_stub_adds_only_itself_and_reports_the_text_it_covers() {
    let (_dir, path) = fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let findings = new_findings(&path, &[]).await;
    let result = connect_j1_pin2(&path, json!({ "direction": "right", "stub_length": 2.54 })).await;
    assert_right_stub(&result, 96.52, 2.54);
    let after = std::fs::read_to_string(&path).unwrap();
    let mut old = before.split_inclusive('\n').peekable();
    let inserted: String = after
        .split_inclusive('\n')
        .filter(|line| {
            if old.peek() == Some(line) {
                old.next();
                false
            } else {
                true
            }
        })
        .collect();
    assert_eq!(old.next(), None, "the call only inserts new lines");
    let added = copperpour_sexp::parse_sexp(&format!("(kicad_sch {inserted})")).unwrap();
    assert_eq!(added.find_all("wire").len(), 1);
    assert_eq!(added.find_all("label").len(), 1);
    assert_eq!(added.children().unwrap().len(), 3);

    let added_findings = new_findings(&path, &findings).await;
    assert_eq!(result["text_overlaps"], json!(added_findings));
    assert_eq!(added_findings.len(), 1, "{added_findings:#?}");
    assert_eq!(added_findings[0]["type"], "text_overlap");
    assert!(["a", "b"]
        .iter()
        .any(|side| added_findings[0][side]["text"] == "+3V3"));
}

#[tokio::test]
async fn when_no_safe_direction_clears_a_fixed_length_it_reports_the_first_safe_stub() {
    let (_dir, path) = fixture();
    let before = new_findings(&path, &[]).await;
    let result = connect_j1_pin2(&path, json!({ "direction": "auto", "stub_length": 2.54 })).await;
    assert_right_stub(&result, 96.52, 2.54);
    let added = new_findings(&path, &before).await;
    assert_eq!(added.len(), 1);
    assert_eq!(result["text_overlaps"], json!(added));
}

#[tokio::test]
async fn a_fixed_direction_can_lengthen() {
    let (_dir, path) = fixture();
    let result = connect_j1_pin2(&path, json!({ "direction": "right" })).await;
    assert_right_stub(&result, 99.06, 5.08);
    assert_eq!(result["text_overlaps"], json!([]));
}

#[tokio::test]
async fn a_clear_longer_stub_cannot_join_a_foreign_wire() {
    let (_dir, path) = fixture();
    success(
        &path,
        "add_wire",
        json!({ "x1": 99.06, "y1": 91.44, "x2": 99.06, "y2": 100.33 }),
    )
    .await;
    success(
        &path,
        "add_schematic_net_label",
        json!({ "net": "OTHER", "x": 99.06, "y": 100.33 }),
    )
    .await;
    let (_, before) = read_schematic(&path).unwrap();
    let result = connect_j1_pin2(&path, json!({ "direction": "right" })).await;
    assert_right_stub(&result, 96.52, 2.54);
    assert_eq!(result["text_overlaps"][0]["type"], "text_overlap");
    let (_, tree) = read_schematic(&path).unwrap();
    let wires = extract_wires(&tree);
    assert_eq!(wires.len(), extract_wires(&before).len() + 1);
    assert!(wires
        .iter()
        .any(|w| (w.x1, w.y1, w.x2, w.y2) == (93.98, 91.44, 96.52, 91.44)));
    assert_eq!(extract_junctions(&tree), extract_junctions(&before));
}

#[tokio::test]
async fn stub_length_uses_saved_endpoints_and_wire_keeps_the_requested_start() {
    let (_dir, path) = fixture();
    let result = success(
        &path,
        "connect_to_net",
        json!({
            "pin_x": 50.8000004, "pin_y": 45.7200004, "net": "SIG", "stub_length": 2.5400004
        }),
    )
    .await;
    assert_eq!(
        result["wire"],
        json!({ "x1": 50.8000004, "y1": 45.7200004, "x2": 53.340001, "y2": 45.72 })
    );
    let (_, tree) = read_schematic(&path).unwrap();
    assert!(extract_wires(&tree)
        .iter()
        .any(|w| (w.x1, w.y1, w.x2, w.y2) == (50.8, 45.72, 53.340001, 45.72)));
    assert_eq!(result["stub_length"], 2.540001);
}

#[tokio::test]
async fn fallback_keeps_the_first_safe_length_even_when_later_lengths_are_safe() {
    let (_dir, path) = fixture();
    success(
        &path,
        "add_schematic_net_label",
        json!({ "net": "EXISTING_LONG_LABEL", "x": 53.34, "y": 44.45 }),
    )
    .await;
    let result = success(
        &path,
        "connect_to_net",
        json!({ "pin_x": 50.8, "pin_y": 45.72, "net": "SIGNAL", "direction": "right" }),
    )
    .await;
    assert_eq!(
        result["label"],
        json!({ "x": 53.34, "y": 45.72, "rotation": 0.0 })
    );
    assert_eq!(result["stub_length"], 2.54);
    let findings = result["text_overlaps"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["type"], "text_overlap");
}

#[tokio::test]
async fn a_clear_default_matches_the_explicit_short_stub() {
    fn without_new_uuids(path: &Path) -> String {
        let (source, tree) = read_schematic(path).unwrap();
        let wire_uuid = tree
            .find_all("wire")
            .last()
            .unwrap()
            .find_str("uuid")
            .unwrap();
        let label_uuid = extract_labels(&tree)
            .into_iter()
            .find(|label| label.net == "SIG_IN" && label.x == 96.52 && label.y == 93.98)
            .unwrap()
            .uuid
            .unwrap();
        source
            .replace(wire_uuid, "wire")
            .replace(&label_uuid, "label")
    }

    let (_dir, path) = fixture();
    let (_explicit_dir, explicit_path) = fixture();
    let pin = json!({ "reference": "J1", "pin_number": "1", "net": "SIG_IN" });
    let mut fixed = pin.clone();
    fixed["direction"] = json!("right");
    fixed["stub_length"] = json!(2.54);
    let result = success(&path, "connect_to_net", pin).await;
    let explicit = success(&explicit_path, "connect_to_net", fixed).await;
    assert_eq!(result, explicit);
    assert_eq!(
        result["label"],
        json!({ "x": 96.52, "y": 93.98, "rotation": 0.0 })
    );
    assert_eq!(result["text_overlaps"], json!([]));
    assert_eq!(without_new_uuids(&path), without_new_uuids(&explicit_path));
}

async fn add_vertical_label(path: &Path) {
    success(
        path,
        "add_schematic_net_label",
        json!({ "net": "TURN_UP", "x": 50.8, "y": 50.8, "rotation": 90 }),
    )
    .await;
}

#[tokio::test]
async fn a_new_wire_can_cross_existing_text_without_its_label_overlapping() {
    let (_dir, path) = fixture();
    add_vertical_label(&path).await;
    let before = new_findings(&path, &[]).await;
    let result = success(
        &path,
        "connect_to_net",
        json!({
            "pin_x": 48.26, "pin_y": 45.72, "net": "SIG",
            "direction": "right", "stub_length": 3.81
        }),
    )
    .await;
    let added = new_findings(&path, &before).await;
    assert_eq!(added.len(), 1, "{added:#?}");
    assert_eq!(added[0]["type"], "wire_label_overlap");
    assert_eq!(added[0]["b"]["text"], "TURN_UP");
    assert_eq!(result["text_overlaps"], json!(added));
}

#[tokio::test]
async fn an_auto_direction_can_turn_at_a_fixed_length() {
    let (_dir, path) = fixture();
    add_vertical_label(&path).await;
    let result = success(
        &path,
        "connect_to_net",
        json!({ "pin_x": 48.26, "pin_y": 45.72, "net": "SIG", "stub_length": 3.81 }),
    )
    .await;
    assert_ne!(result["direction"], "right", "{result}");
    assert_eq!(result["stub_length"], 3.81);
    assert_eq!(result["text_overlaps"], json!([]));
}

#[tokio::test]
async fn the_longest_outward_stub_is_tried_before_turning() {
    let (_dir, path) = fixture();
    for (net, x) in [("X", 53.34), ("Y", 55.88)] {
        success(
            &path,
            "add_schematic_net_label",
            json!({ "net": net, "x": x, "y": 44.45 }),
        )
        .await;
    }
    let result = success(
        &path,
        "connect_to_net",
        json!({ "pin_x": 50.8, "pin_y": 45.72, "net": "SIGNAL" }),
    )
    .await;
    assert_eq!(result["direction"], "right");
    assert_eq!(result["stub_length"], 7.62);
    assert_eq!(result["text_overlaps"], json!([]));
}

async fn add_wire_to_the_right_of_label_anchor(path: &Path) {
    success(
        path,
        "add_wire",
        json!({ "x1": 53.34, "y1": 43.18, "x2": 53.34, "y2": 49.53 }),
    )
    .await;
}

#[tokio::test]
async fn a_fixed_label_without_a_stub_reports_the_wire_it_overlaps() {
    let (_dir, path) = fixture();
    add_wire_to_the_right_of_label_anchor(&path).await;
    let before = new_findings(&path, &[]).await;
    let result = success(
        &path,
        "connect_to_net",
        json!({
            "pin_x": 48.26, "pin_y": 45.72, "net": "SIGNAL", "stub_length": 0,
            "direction": "right", "label_type": "global_label"
        }),
    )
    .await;
    let added = new_findings(&path, &before).await;
    assert_eq!(added.len(), 1, "{added:#?}");
    assert_eq!(added[0]["type"], "wire_label_overlap");
    assert_eq!(added[0]["b"]["label_type"], "global_label");
    assert_eq!(result["text_overlaps"], json!(added));
    assert!(result["wire"].is_null());
    assert_eq!(result["stub_length"], 0.0);
}

#[tokio::test]
async fn a_delegated_zero_length_stub_turns_the_label_without_moving_its_anchor() {
    let (_dir, path) = fixture();
    add_wire_to_the_right_of_label_anchor(&path).await;
    let (_, before) = read_schematic(&path).unwrap();
    let result = success(
        &path,
        "connect_to_net",
        json!({
            "pin_x": 48.26, "pin_y": 45.72, "net": "SIGNAL", "stub_length": 0,
            "direction": "auto", "label_type": "global_label"
        }),
    )
    .await;
    assert_eq!(result["direction"], "left");
    assert_eq!(
        result["label"],
        json!({ "x": 48.26, "y": 45.72, "rotation": 180.0 })
    );
    assert_eq!(result["text_overlaps"], json!([]));
    assert!(result["wire"].is_null());
    assert_eq!(result["stub_length"], 0.0);
    let (_, after) = read_schematic(&path).unwrap();
    assert_eq!(before.find_all("wire"), after.find_all("wire"));
}

#[tokio::test]
async fn a_same_net_stub_can_report_collinear_overlap_without_a_foreign_join() {
    let (_dir, path) = fixture();
    success(
        &path,
        "add_wire",
        json!({ "x1": 50.8, "y1": 45.72, "x2": 53.34, "y2": 45.72 }),
    )
    .await;
    let result = success(
        &path,
        "connect_to_net",
        json!({
            "pin_x": 50.8, "pin_y": 45.72, "net": "SIGNAL", "direction": "right",
            "stub_length": 2.54
        }),
    )
    .await;
    let findings = result["text_overlaps"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["type"], "collinear_overlap");
    assert_ne!(findings[0]["a"]["uuid"], findings[0]["b"]["uuid"]);
}
