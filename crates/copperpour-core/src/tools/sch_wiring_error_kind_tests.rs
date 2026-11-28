//! `sch_wiring` refusals carry a typed `error.kind` through served dispatch
//! (#741), and leave the sheet untouched. The sheets are KiCad 10 resaves:
//! `coincident_labels_kicad10.kicad_sch` (two wires, a plain and a global
//! label `PWR` on one anchor) and `no_connect_carry_kicad10.kicad_sch`.

use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{body_of, served_call};
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const WIRE_UUID: &str = "1f044f87-1a20-43d1-a365-bf361a33fb02";
const SHEET_UUID: &str = "afdd538f-650b-442f-9e9d-dcbd035eb96c";
const GLOBAL_PWR_UUID: &str = "c6653f40-dca2-48f0-85fa-e9e371c6a1b0";
const MISSING_UUID: &str = "00000000-0000-0000-0000-000000000741";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn sheet(source: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sheet.kicad_sch");
    std::fs::write(&path, source).unwrap();
    (dir, path)
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..ServerConfig::default()
    })
    .await
    .unwrap()
}

/// The body of a refused `tools/call`, after checking that the sheet still
/// holds `before`.
async fn refusal(
    handler: &McpHandler,
    path: &Path,
    before: &str,
    name: &str,
    mut arguments: Value,
) -> Value {
    arguments["schematic"] = json!(path.to_str().unwrap());
    let result = served_call(handler, name, arguments).await;
    assert!(result.is_error, "{name} did not refuse");
    let body = body_of(&result);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        before,
        "{name} wrote"
    );
    body
}

fn assert_stale(body: &Value, target: &str, message: &str) {
    assert_eq!(body["error"]["kind"], "stale_target", "{body}");
    assert_eq!(body["error"]["target"], target, "{body}");
    assert_eq!(body["message"], message, "{body}");
    assert_eq!(body["error"]["reason"], message, "{body}");
}

#[tokio::test]
async fn wire_refusals_are_typed() {
    let source = fixture("coincident_labels_kicad10.kicad_sch");
    let (_dir, path) = sheet(&source);
    let handler = handler().await;

    let body = refusal(
        &handler,
        &path,
        &source,
        "delete_schematic_wire",
        json!({ "uuid": MISSING_UUID }),
    )
    .await;
    assert_stale(
        &body,
        &format!("wire {MISSING_UUID}"),
        &format!("Wire UUID '{MISSING_UUID}' not found"),
    );

    // No identity names `uuid`; some coordinates name the first missing one.
    for (arguments, field) in [
        (json!({}), "uuid"),
        (json!({ "x1": 1.0, "y1": 2.0, "y2": 3.0 }), "x2"),
    ] {
        let body = refusal(&handler, &path, &source, "delete_schematic_wire", arguments).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
    }

    // A label's uuid, and endpoints no wire has.
    for (arguments, target) in [
        (
            json!({ "uuid": GLOBAL_PWR_UUID }),
            format!("wire {GLOBAL_PWR_UUID}"),
        ),
        (
            json!({ "x1": 0.0, "y1": 0.0, "x2": 1.0, "y2": 1.0 }),
            "wire (0, 0)-(1, 1)".to_string(),
        ),
    ] {
        let body = refusal(&handler, &path, &source, "delete_schematic_wire", arguments).await;
        assert_stale(
            &body,
            &target,
            "Cannot locate a wire block matching the requested identity",
        );
    }

    let body = refusal(
        &handler,
        &path,
        &source,
        "batch_delete_schematic_wire",
        json!({ "uuids": [MISSING_UUID] }),
    )
    .await;
    assert_stale(
        &body,
        "uuids",
        &format!("No wires deleted: Wire UUID '{MISSING_UUID}' not found"),
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "split_wire_at_point",
        json!({ "x": 50.0, "y": 50.0 }),
    )
    .await;
    assert_stale(
        &body,
        "wire through (50, 50)",
        "No wire found passing through that point",
    );
}

/// A wire the quoted-uuid text search misses: its uuid unquoted, as KiCad
/// wrote before version 8, or repeated by the sheet earlier in the file. The
/// split finds it by its endpoints.
#[tokio::test]
async fn split_finds_a_wire_its_uuid_search_misses() {
    let quoted = format!(r#"(uuid "{WIRE_UUID}")"#);
    let fixture = fixture("coincident_labels_kicad10.kicad_sch");
    let handler = handler().await;
    for source in [
        fixture.replace(&quoted, &format!("(uuid {WIRE_UUID})")),
        fixture.replace(WIRE_UUID, SHEET_UUID),
    ] {
        let (_dir, path) = sheet(&source);
        let result = served_call(
            &handler,
            "split_wire_at_point",
            json!({ "schematic": path.to_str().unwrap(), "x": 120.0, "y": 97.79 }),
        )
        .await;
        assert!(!result.is_error, "{}", body_of(&result));
        let after = std::fs::read_to_string(&path).unwrap();
        let wires = copperpour_sexp::schematic::extract_wires(
            &copperpour_sexp::parser::parse_sexp(&after).unwrap(),
        );
        assert_eq!(wires.len(), 3, "the wire is now two halves");
    }
}

#[tokio::test]
async fn label_refusals_are_typed() {
    let source = fixture("coincident_labels_kicad10.kicad_sch");
    let (_dir, path) = sheet(&source);
    let handler = handler().await;

    let body = refusal(
        &handler,
        &path,
        &source,
        "delete_schematic_net_label",
        json!({ "net": "NOPE", "x": 0.0, "y": 0.0 }),
    )
    .await;
    assert_stale(
        &body,
        "label 'NOPE' at (0, 0)",
        "No label named 'NOPE' in this schematic",
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "delete_schematic_net_label",
        json!({ "net": "PWR", "x": 0.0, "y": 0.0 }),
    )
    .await;
    assert_stale(
        &body,
        "label 'PWR' at (0, 0)",
        "No label 'PWR' at (0, 0). Found 2 label(s) named 'PWR': \
         label at (114.3, 105.41); global_label at (114.3, 105.41)",
    );

    // The plain and the global `PWR` share one anchor, and the tool takes no
    // uuid to choose by.
    let body = refusal(
        &handler,
        &path,
        &source,
        "delete_schematic_net_label",
        json!({ "net": "PWR", "x": 114.3, "y": 105.41 }),
    )
    .await;
    let message = "2 labels named 'PWR' share position (114.3, 105.41) — delete by uuid is \
                   not supported yet; remove the duplicates in eeschema";
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    assert_eq!(body["error"]["operation"], "delete_schematic_net_label");
    assert_eq!(body["error"]["reasons"], json!([message]));
    assert_eq!(body["message"], message);

    let body = refusal(
        &handler,
        &path,
        &source,
        "rotate_schematic_label",
        json!({ "uuid": MISSING_UUID, "rotation": 90 }),
    )
    .await;
    assert_stale(
        &body,
        &format!("label {MISSING_UUID}"),
        &format!("No label with uuid '{MISSING_UUID}' in this schematic"),
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "rotate_schematic_label",
        json!({ "net": "PWR", "x": 0.0, "y": 0.0, "rotation": 90 }),
    )
    .await;
    assert_stale(
        &body,
        "label 'PWR' at (0, 0)",
        "No label 'PWR' at (0, 0). Found: \
         label at (114.3, 105.41); global_label at (114.3, 105.41)",
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "batch_rotate_labels",
        json!({ "labels": [
            { "uuid": GLOBAL_PWR_UUID, "rotation": 180 },
            { "net": "NOPE", "x": 1.0, "y": 2.0, "rotation": 90 }
        ] }),
    )
    .await;
    assert_stale(
        &body,
        "label 'NOPE' at (1, 2)",
        "labels[1]: No label named 'NOPE' in this schematic. No label was rotated.",
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "move_labels_by_offset",
        json!({ "net": "NOPE", "dx": 1.0, "dy": 1.0 }),
    )
    .await;
    assert_stale(
        &body,
        "label 'NOPE'",
        "No label named 'NOPE' in this schematic",
    );
}

#[tokio::test]
async fn no_connect_refusals_are_typed() {
    let source = fixture("no_connect_carry_kicad10.kicad_sch");
    let (_dir, path) = sheet(&source);
    let handler = handler().await;

    let body = refusal(
        &handler,
        &path,
        &source,
        "delete_no_connect",
        json!({ "x": 1.0, "y": 2.0 }),
    )
    .await;
    assert_stale(
        &body,
        "no_connect at (1, 2)",
        "No-connect not found at that position",
    );

    let body = refusal(
        &handler,
        &path,
        &source,
        "batch_delete_no_connect",
        json!({ "positions": [{ "x": 1.0, "y": 2.0 }] }),
    )
    .await;
    assert_stale(
        &body,
        "positions",
        "No no-connects deleted: No no-connect at (1, 2)",
    );

    // A bad item is the caller's to fix, even beside a missing marker.
    let body = refusal(
        &handler,
        &path,
        &source,
        "batch_delete_no_connect",
        json!({ "positions": [{ "x": 1.0, "y": 2.0 }, { "x": 1.0 }] }),
    )
    .await;
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "positions[1]", "{body}");
}

/// Dispatch checks `positions` against the schema first, so only a direct
/// call reaches the handler's own check.
#[tokio::test]
async fn batch_add_no_connect_without_positions_is_invalid_argument() {
    let source = fixture("no_connect_carry_kicad10.kicad_sch");
    let (_dir, path) = sheet(&source);
    let ctx = crate::tools::ToolContext::new(
        ServerConfig::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    );
    let result =
        super::handle_batch_add_no_connect(&json!({ "schematic": path.to_str().unwrap() }), &ctx)
            .await
            .unwrap();
    assert_eq!(
        crate::mcp::error::extract_error_kind(&result).as_deref(),
        Some("invalid_argument")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
}

#[tokio::test]
async fn connect_to_net_names_the_missing_half_of_a_pin() {
    let source = fixture("coincident_labels_kicad10.kicad_sch");
    let (_dir, path) = sheet(&source);
    let handler = handler().await;
    for (given, missing) in [
        (json!({ "reference": "R1" }), "pin_number"),
        (json!({ "pin_number": "1" }), "reference"),
    ] {
        let mut arguments = given;
        arguments["net"] = json!("N");
        let body = refusal(&handler, &path, &source, "connect_to_net", arguments).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], missing, "{body}");
        assert_eq!(
            body["message"],
            "'reference' and 'pin_number' must be given together"
        );
    }
}
