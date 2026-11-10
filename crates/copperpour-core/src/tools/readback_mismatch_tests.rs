//! A save between a write and its readback, through served `tools/call`
//! (#418, #201, #122).
//!
//! The writer has replaced the file by then, so every tool must answer
//! `mutation_outcome_uncertain`, never a `conflict` or `stale_target` that
//! says nothing was written. The fixtures are KiCad 10 saves described in
//! their own READMEs; `writer::test_hooks` plays the other program's save.

use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;
use copperpour_sexp::writer::test_hooks;
use serde_json::{json, Value};
use std::path::Path;

/// One tool from each way a caller used to report the readback: a
/// structured `conflict` (#122, #201), `stale_target` from a command
/// commit, a plain error from the closed-board footprint edits, and a bare
/// `?` that reached dispatch as `handler_error`; then the board writers that
/// already reported it as uncertain, and must still.
fn cases() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    let poly = json!([{
        "type": "poly",
        "points": [{"x": -1.0, "y": -1.0}, {"x": 1.0, "y": -1.0}, {"x": 1.0, "y": 1.0}],
        "stroke_width_mm": 0.05,
        "fill": "none"
    }]);
    vec![
        (
            "set_footprint_metadata",
            "c_0603_1608metric_kicad10.kicad_mod",
            "footprint_path",
            json!({"description": "readback"}),
        ),
        (
            "set_footprint_graphics",
            "c_0603_1608metric_kicad10.kicad_mod",
            "footprint_path",
            json!({"selector": {"layer": "F.CrtYd"}, "mode": "replace", "graphics": poly}),
        ),
        (
            "set_footprint_models",
            "c_0603_1608metric_kicad10.kicad_mod",
            "footprint_path",
            json!({"mode": "replace", "models": [{"path": "../models/new.step"}]}),
        ),
        (
            "copy_routing_pattern",
            "issue_96.kicad_pcb",
            "board",
            json!({
                "src_x1": 124.0, "src_y1": 99.0, "src_x2": 129.0, "src_y2": 101.0,
                "dest_x": 140.0, "dest_y": 120.0
            }),
        ),
        (
            "delete_schematic_component",
            "component_fields_kicad10.kicad_sch",
            "schematic",
            json!({"reference": "R1"}),
        ),
        (
            "move_component",
            "inner_layer_flip_front.kicad_pcb",
            "board",
            json!({"reference": "R1", "x": 120.0, "y": 110.0}),
        ),
        (
            "flip_component",
            "inner_layer_flip_front.kicad_pcb",
            "board",
            json!({"reference": "NT1", "layer": "B.Cu"}),
        ),
        (
            "set_board_title_block",
            "issue_257_page_A4.kicad_pcb",
            "board",
            json!({"title": "Readback"}),
        ),
        (
            "set_board_page",
            "issue_257_page_A4.kicad_pcb",
            "board",
            json!({"size": "A3"}),
        ),
        (
            "add_board_text",
            "issue_257_page_A4.kicad_pcb",
            "board",
            json!({"text": "REV B", "x": 5.0, "y": 5.0}),
        ),
        (
            "move_schematic_component",
            "component_fields_kicad10.kicad_sch",
            "schematic",
            json!({"reference": "R1", "x": 50.8, "y": 50.8}),
        ),
    ]
}

/// No KiCad, so every board tool takes its file path.
async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds")
}

async fn call(handler: &McpHandler, tool: &str, arguments: Value) -> Value {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 418, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    response.result.expect("a result")
}

/// What `tool` answered, or `None` when it is the uncertain outcome the
/// writer's replacement calls for.
async fn wrong_answer(
    handler: &McpHandler,
    fixtures: &Path,
    tool: &str,
    fixture: &str,
    key: &str,
    mut arguments: Value,
) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture);
    let original = std::fs::read_to_string(fixtures.join(fixture)).unwrap();
    std::fs::write(&path, &original).unwrap();
    // The other program saves its own copy of the file: the original.
    test_hooks::save_after_replace(&path, original.clone());
    arguments[key] = json!(path.display().to_string());

    let result = call(handler, tool, arguments).await;

    if test_hooks::armed(&path) {
        return Some(format!("never reached the readback: {result}"));
    }
    if std::fs::read_to_string(&path).unwrap() != original {
        return Some("overwrote the other program's save".to_string());
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let body: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({ "message": text }));
    let error = &body["error"];
    let expected = json!({
        "kind": "mutation_outcome_uncertain",
        "operation": tool,
        "path": path.display().to_string(),
    });
    let observed = json!({
        "kind": error["kind"],
        "operation": error["operation"],
        "path": error["path"],
    });
    (result["isError"] != json!(true) || observed != expected).then(|| format!("{body}"))
}

#[tokio::test]
async fn a_save_after_the_write_is_an_uncertain_outcome() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let handler = handler().await;
    let mut wrong = Vec::new();
    for (tool, fixture, key, arguments) in cases() {
        if let Some(answer) = wrong_answer(&handler, &fixtures, tool, fixture, key, arguments).await
        {
            wrong.push(format!("{tool}: {answer}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Dispatch reads the mismatch out of any error chain, from either writer,
/// and never mistakes a refusal for one.
#[test]
fn only_a_readback_mismatch_is_classified_as_one() {
    use crate::tools::readback_mismatch_path;
    use copperpour_sexp::SexpError;
    let path = Path::new("/designs/board.kicad_pcb");

    let sexp = anyhow::Error::new(SexpError::ReadbackMismatch {
        path: path.to_path_buf(),
    })
    .context("while copying routing");
    assert_eq!(readback_mismatch_path(&sexp), Some(path));
    let editor = anyhow::Error::new(copperpour_schematic_editor::Error::ReadbackMismatch(
        path.to_path_buf(),
    ));
    assert_eq!(readback_mismatch_path(&editor), Some(path));

    let refused = anyhow::Error::new(SexpError::Conflict {
        path: path.to_path_buf(),
    });
    assert_eq!(readback_mismatch_path(&refused), None);
    let refused = anyhow::Error::new(copperpour_schematic_editor::Error::Conflict(
        path.to_path_buf(),
    ));
    assert_eq!(readback_mismatch_path(&refused), None);
}
