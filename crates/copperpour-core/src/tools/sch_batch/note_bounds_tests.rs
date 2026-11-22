use super::*;

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::mcp::{handler::McpHandler, protocol::ToolContent};
use crate::tools::{ServerConfig, ToolContext};

fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    fs::copy(source, &path).unwrap();
    (dir, path)
}

fn body(result: &CallToolResult) -> Value {
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected a JSON text result");
    };
    serde_json::from_str(text).unwrap()
}

async fn layout(path: &Path, include_notes: Option<Value>) -> CallToolResult {
    let handler = McpHandler::new(ServerConfig {
        auto_load_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut arguments = json!({ "schematic": path });
    if let Some(value) = include_notes {
        arguments["include_notes"] = value;
    }
    let before = fs::read(path).unwrap();
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "get_schematic_layout", "arguments": arguments }
        }))
        .await
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(response.error.is_none(), "{response:?}");
    serde_json::from_value(response.result.unwrap()).unwrap()
}

async fn layout_ok(path: &Path, include_notes: Option<bool>) -> Value {
    let result = layout(path, include_notes.map(Value::Bool)).await;
    let body = body(&result);
    assert!(!result.is_error, "{body}");
    body
}

fn assert_measured(notes: &Value, uuid: &str, edges: [f64; 4]) {
    let note = note(notes, uuid);
    assert_eq!(note["kind"], "text", "{note}");
    assert_eq!(note["status"], "measured", "{note}");
    assert_eq!(note.get("reason"), Some(&Value::Null), "{note}");
    let bounds = &note["bounds"];
    // KiCad 10.0.8's SVG stroke extents, including half the plotted pen width.
    for (axis, expected) in ["x_min", "y_min", "x_max", "y_max", "width", "height"]
        .into_iter()
        .zip(
            edges
                .into_iter()
                .chain([edges[2] - edges[0], edges[3] - edges[1]]),
        )
    {
        let actual = bounds[axis].as_f64().unwrap();
        assert!(
            (actual - expected).abs() < 0.005,
            "{uuid} {axis}: {actual} vs {expected}"
        );
    }
}

#[tokio::test]
async fn opted_in_note_bounds_match_kicad_without_changing_symbol_bounds() {
    let (_dir, path) = fixture("schematic_texts_kicad10.kicad_sch");
    let before = layout_ok(&path, None).await;
    assert!(before.get("notes").is_none() && before.get("note_count").is_none());
    assert_eq!(before, layout_ok(&path, Some(false)).await);
    let mut after = layout_ok(&path, Some(true)).await;
    assert_eq!(
        after.as_object_mut().unwrap().remove("note_count"),
        Some(json!(6))
    );
    let notes = after.as_object_mut().unwrap().remove("notes").unwrap();
    assert_eq!(notes.as_array().unwrap().len(), 6);
    assert_eq!(before, after);
    // Visible stroke extents from KiCad 10.0.8's SVG, including half
    // the plotted pen width. The multiline bounds union both lines.
    for (uuid, expected) in [
        (
            "40894e37-894c-44d5-ae33-c8e4e9698a41",
            [25.7263, 23.6403, 35.2523, 25.0627],
        ),
        (
            "a065c718-bbfa-4223-b6a0-9bf4c1eb9358",
            [25.6659, 34.2957, 36.6433, 37.7627],
        ),
        (
            "eb4a9af4-2c22-4dbd-9eb1-7b7caee2242e",
            [76.0744, 38.2940, 78.4744, 56.0273],
        ),
        (
            "3b7acc9f-f585-43ba-8a32-23f5a595c079",
            [25.6659, 48.9194, 45.9566, 50.8861],
        ),
        (
            "4ff58621-eb4d-4742-8091-6668723e2c5f",
            [25.7263, 74.3799, 41.1791, 75.8627],
        ),
        (
            "ed2c0b10-61a7-40dc-abbd-ef466f734dab",
            [25.6659, 61.7403, 38.1551, 63.5861],
        ),
    ] {
        assert_measured(&notes, uuid, expected);
    }
}

fn note<'n>(notes: &'n Value, uuid: &str) -> &'n Value {
    notes
        .as_array()
        .unwrap()
        .iter()
        .find(|note| note["uuid"] == uuid)
        .unwrap_or_else(|| panic!("note {uuid} missing from {notes}"))
}

#[tokio::test]
async fn justified_multiline_and_turned_notes_match_kicad_plot() {
    let (_dir, path) = fixture("schematic_note_bounds_kicad10.kicad_sch");
    let body = layout_ok(&path, Some(true)).await;
    let notes = &body["notes"];
    assert_eq!(body["note_count"], 17);
    assert_eq!(notes.as_array().unwrap().len(), 17);
    for (uuid, expected) in [
        // "Centered\nthree\nlines", centred on both axes
        (
            "f56b7401-f7c8-4b9f-9d30-c333b7a9b100",
            [35.9929, 26.9387, 43.9467, 32.4503],
        ),
        // "Right top A\nRight top B", right top
        (
            "ca6c95c0-6d96-4ace-ae3e-4be71f266d22",
            [78.9985, 44.7262, 89.8550, 48.6166],
        ),
        // "Left top"
        (
            "d5b8802c-2764-4816-b0af-2654549d8164",
            [30.3263, 59.7262, 37.4939, 61.5720],
        ),
        // "Center bottom", centred horizontally
        (
            "6d0143cf-832c-4a54-be1e-9c8416087f6e",
            [53.3320, 73.2403, 66.6075, 74.6627],
        ),
        // "Right bottom"
        (
            "38bbb472-5326-4455-a90c-03aa610e58cf",
            [77.6681, 88.2403, 89.6736, 90.0861],
        ),
        // "Turned left\nbottom", 90, left bottom
        (
            "300152cf-c120-44e1-b065-ba317e3b58f3",
            [126.4457, 59.2323, 129.9127, 69.6050],
        ),
        // "Turned centre", 90, centred
        (
            "4f36e723-790d-4a3e-8f5b-be08565e7b84",
            [144.2333, 53.3238, 145.6557, 66.2970],
        ),
        // "Turned right\ntop", 90, right top
        (
            "dbe7e220-e5a8-4b5d-931b-b1343abf1494",
            [159.9762, 39.8949, 163.8666, 51.4771],
        ),
        // "Gap above\n\nGap below": the blank line still takes a pitch
        (
            "6514fd70-a095-43c1-aa3a-6966ba40102d",
            [30.2659, 104.1511, 39.9732, 110.0861],
        ),
        // "Big bold", 2.54 mm, thickness 0.508
        (
            "0c3d6182-64fd-41f8-9f43-b69fa2c85cc2",
            [30.6849, 131.6186, 46.1910, 135.5133],
        ),
        // "Upside note": written at 180, saved by KiCad at 0, left bottom
        (
            "53abdbd9-c1b2-4712-af44-c25d8904128d",
            [220.3263, 38.2403, 231.3643, 40.0861],
        ),
        // "Down note\nsecond": written at 270, saved by KiCad at 90
        (
            "73ad1334-085e-4303-a457-d4ccd9de0e50",
            [236.4457, 39.8371, 239.9127, 49.4840],
        ),
    ] {
        assert_measured(notes, uuid, expected);
    }
}

#[tokio::test]
async fn notes_this_model_cannot_measure_are_disclosed_not_guessed() {
    let (_dir, path) = fixture("schematic_note_bounds_kicad10.kicad_sch");
    let body = layout_ok(&path, Some(true)).await;
    let notes = &body["notes"];
    for (uuid, status, reason) in [
        (
            "f460394c-bdcd-4b6d-a408-b70807b6dd37",
            "unmeasured",
            json!("unsupported_markup"),
        ),
        (
            "c1d22f60-2227-4405-923d-7478e5339915",
            "unmeasured",
            json!("unresolved_text_variable"),
        ),
        (
            "7917c164-7034-461d-9234-3229aa59eca5",
            "unmeasured",
            json!("unsupported_text_style"),
        ),
        (
            "ab2d9c81-12c2-4d00-b919-c6ab3975748f",
            "unmeasured",
            json!("unsupported_glyph"),
        ),
        // Three spaces: KiCad plots no stroke for it either.
        (
            "ff078463-7992-4419-9dfb-58e08d1c830d",
            "no_ink",
            Value::Null,
        ),
    ] {
        let note = note(notes, uuid);
        assert_eq!(note["status"], status, "{note}");
        assert_eq!(note["reason"], reason, "{note}");
        assert_eq!(note["bounds"], Value::Null, "{note}");
    }
}

#[tokio::test]
async fn plot_bounds_include_explicit_pen_and_omit_final_empty_line() {
    let (_dir, path) = fixture("schematic_note_plot_edges_kicad10.kicad_sch");
    let body = layout_ok(&path, Some(true)).await;
    let notes = &body["notes"];
    for (uuid, edges) in [
        (
            "36a0fd8c-f260-4ea5-829b-5abd8a7e62a1",
            [40.3263, 38.2403, 49.4895, 39.6627],
        ),
        (
            "300b83bb-96da-423f-87fe-49615424e197",
            [40.1343, 54.7636, 50.4997, 56.1182],
        ),
        (
            "932cd07d-e05f-4811-b049-a90e9a2b2ba7",
            [28.5253, 74.6448, 39.6503, 76.6382],
        ),
        (
            "28de3046-1bf1-4966-aeb6-d96978a9776c",
            [40.1449, 108.2403, 53.6019, 110.0861],
        ),
    ] {
        assert_measured(notes, uuid, edges);
    }
    let expression = note(notes, "3cc63106-5143-40c8-9ead-13ffa877ca9a");
    assert_eq!(expression["status"], "unmeasured");
    assert_eq!(expression["reason"], "unresolved_text_variable");
    assert_eq!(expression.get("bounds"), Some(&Value::Null));
}

/// Copperpour output before any KiCad save: `add_schematic_text` writes
/// 180 and 270 as given. KiCad 10.0.8 plots these two notes with the
/// same ink as the 0 and 90 notes it saves in their place in
/// `schematic_note_bounds_kicad10.kicad_sch`; the model does not
/// normalise them yet.
#[tokio::test]
async fn unsaved_turned_notes_are_disclosed_not_guessed() {
    let (_dir, path) = fixture("schematic_note_rotation_unsaved.kicad_sch");
    let body = layout_ok(&path, Some(true)).await;
    assert_eq!(body["note_count"], json!(2));
    let notes = &body["notes"];
    // "Upside note" at 180, "Down note\nsecond" at 270.
    for uuid in [
        "68db779e-114a-4643-9c18-68ec6bdbdb83",
        "a4214d20-bddf-4c00-9611-4921d948d594",
    ] {
        let note = note(notes, uuid);
        assert_eq!(note["status"], "unmeasured", "{note}");
        assert_eq!(note["reason"], "unsupported_rotation", "{note}");
        assert_eq!(note["bounds"], Value::Null, "{note}");
    }
}

#[tokio::test]
async fn layout_reports_text_boxes_without_guessing_bounds() {
    let (_dir, path) = fixture("rigid_block_move_kicad10.kicad_sch");
    let body = layout_ok(&path, Some(true)).await;
    assert_eq!(body["note_count"], 1);
    assert_eq!(body["notes"].as_array().unwrap().len(), 1);
    let text_box = &body["notes"][0];
    assert_eq!(
        *text_box,
        json!({
            "uuid": "5e6a0c1d-8f3b-4c2a-9d7e-1b2c3d4e5f61",
            "kind": "text_box",
            "bounds": null,
            "status": "unmeasured",
            "reason": "unsupported_note_type"
        })
    );
}

fn assert_invalid_include_notes(result: &CallToolResult) {
    let body = body(result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "include_notes", "{body}");
}

#[tokio::test]
async fn served_layout_refuses_string_and_null_include_notes() {
    let (_dir, path) = fixture("schematic_texts_kicad10.kicad_sch");
    for invalid in [json!("true"), Value::Null] {
        assert_invalid_include_notes(&layout(&path, Some(invalid)).await);
    }
}

#[test]
fn layout_schema_refuses_string_and_null_include_notes() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == "get_schematic_layout")
        .unwrap();
    for invalid in [json!("true"), Value::Null] {
        let args = json!({"schematic": "saved.kicad_sch", "include_notes": invalid});
        let err = tool
            .input_validator
            .validate(&args)
            .expect_err("schema must refuse invalid type");
        assert_eq!(err.instance_path().as_str(), "/include_notes");
    }
}

#[tokio::test]
async fn direct_layout_refuses_string_and_null_include_notes() {
    let (_dir, path) = fixture("schematic_texts_kicad10.kicad_sch");
    let before = fs::read(&path).unwrap();
    let ctx = ToolContext::new(
        Default::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    );
    for invalid in [json!("true"), Value::Null] {
        let result = handle_get_layout(
            &json!({ "schematic": path, "include_notes": invalid }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_invalid_include_notes(&result);
    }
}
