//! A save between a tool's read and its write, through served `tools/call`.
//!
//! The writer refuses before the rename, so the file holds the other
//! program's save and nothing else: a tool that hands the writer's `Conflict`
//! to dispatch with `?` must answer `conflict`, as one that maps it itself
//! does. A tool that wrote the same file earlier in the call answers
//! `mutation_outcome_uncertain` instead. The files and the saves played over
//! them are KiCad 10 saves described in their own READMEs; `writer::test_hooks`
//! plays the save.

use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;
use copperpour_sexp::writer::test_hooks;
use serde_json::{json, Value};
use std::path::Path;

/// Tools whose one write hands a `Conflict` to dispatch with `?`: through
/// the schematic editor's `overwrite`, and through `write_atomic_if_unchanged`
/// directly on a schematic, a symbol library and a footprint; and a closed
/// board's footprint edit, through `ClosedBoardError::into_result`. Each is
/// (tool, file, the other program's save of it, path argument, arguments).
fn cases() -> Vec<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Value,
)> {
    const FIELDS: &str = "component_fields_kicad10.kicad_sch";
    const FIELDS_SAVED: &str = "component_fields_kicad10.removed.kicad_sch";
    vec![
        (
            "add_schematic_net_label",
            FIELDS,
            FIELDS_SAVED,
            "schematic",
            json!({"net": "SAVED", "x": 10.16, "y": 10.16}),
        ),
        (
            "add_no_connect",
            FIELDS,
            FIELDS_SAVED,
            "schematic",
            json!({"x": 10.16, "y": 10.16}),
        ),
        (
            "batch_add_wire",
            FIELDS,
            FIELDS_SAVED,
            "schematic",
            json!({"wires": [{"x1": 10.16, "y1": 10.16, "x2": 20.32, "y2": 10.16}]}),
        ),
        (
            "batch_connect_to_net",
            FIELDS,
            FIELDS_SAVED,
            "schematic",
            json!({"net_name": "SAVED", "pins": [{"reference": "R1", "pin_number": "1"}]}),
        ),
        (
            "move_labels_by_offset",
            "label_move_kicad10.kicad_sch",
            "label_move_kicad10.moved.kicad_sch",
            "schematic",
            json!({"net": "PWR", "dx": 2.54, "dy": 0}),
        ),
        (
            "edit_footprint_pad",
            "c_0603_1608metric_kicad10.kicad_mod",
            "sd_card_device_kicad10.kicad_mod",
            "footprint_path",
            json!({"pad_number": "1", "width": 1.3}),
        ),
        (
            "delete_symbol",
            "symbol_fields_kicad10.kicad_sym",
            "symbol_fields_kicad10.edited.kicad_sym",
            "library_path",
            json!({"symbol_name": "FIELDS_BARE"}),
        ),
        (
            "move_component",
            "inner_layer_flip_front.kicad_pcb",
            "inner_layer_flip_back.kicad_pcb",
            "board",
            json!({"reference": "R1", "x": 120.0, "y": 110.0}),
        ),
        (
            "repeat_layout",
            "repeat_layout_kicad10.kicad_pcb",
            "repeat_layout_mismatch_kicad10.kicad_pcb",
            "board",
            json!({"anchor_reference": "R1"}),
        ),
    ]
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        auto_load_toolsets: true,
        eager_toolsets: true,
        ..ServerConfig::default()
    })
    .await
    .expect("handler builds")
}

async fn call(handler: &McpHandler, tool: &str, arguments: Value) -> (Value, Value) {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    let result = response.result.expect("a result");
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let body = serde_json::from_str(text).unwrap_or_else(|_| json!({ "message": text }));
    (result, body)
}

fn fixture(name: &str) -> String {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::read_to_string(fixtures.join(name)).unwrap()
}

/// What `tool` answered, or `None` when it is the `conflict` refusal naming
/// the file, with the other program's save left as it saved it.
async fn wrong_answer(
    handler: &McpHandler,
    tool: &str,
    name: &str,
    save: &str,
    key: &str,
    mut arguments: Value,
) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, fixture(name)).unwrap();
    let saved = fixture(save);
    test_hooks::save_before_compare(&path, saved.clone());
    arguments[key] = json!(path.display().to_string());

    let (result, body) = call(handler, tool, arguments).await;

    if test_hooks::armed(&path) {
        return Some(format!("never reached the write: {body}"));
    }
    if std::fs::read_to_string(&path).unwrap() != saved {
        return Some("overwrote the other program's save".to_string());
    }
    let expected = json!({ "kind": "conflict", "paths": [path.display().to_string()] });
    let observed = json!({ "kind": body["error"]["kind"], "paths": body["error"]["paths"] });
    let names_the_tool = body["message"]
        .as_str()
        .is_some_and(|message| message.contains(tool));
    (result["isError"] != json!(true) || observed != expected || !names_the_tool)
        .then(|| format!("{body}"))
}

#[tokio::test]
async fn a_save_before_the_write_is_a_conflict() {
    let handler = handler().await;
    let mut wrong = Vec::new();
    for (tool, name, save, key, arguments) in cases() {
        if let Some(answer) = wrong_answer(&handler, tool, name, save, key, arguments).await {
            wrong.push(format!("{tool}: {answer}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `create_schematic` writes the file, then loads and rewrites it: a save
/// between the two finds a file the call already created.
#[tokio::test]
async fn create_schematic_reports_a_save_after_its_create_as_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("created.kicad_sch");
    let saved = fixture("label_move_kicad10.kicad_sch");
    test_hooks::save_before_compare(&path, saved.clone());
    let (_, body) = call(
        &handler().await,
        "create_schematic",
        json!({ "path": path.display().to_string() }),
    )
    .await;
    assert!(!test_hooks::armed(&path), "never reached the rewrite");
    assert_eq!(
        body["error"]["kind"],
        json!("mutation_outcome_uncertain"),
        "{body}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

/// `add_power_symbol` writes the symbol, then rewrites the sheet with the
/// junction its pin needs mid-wire: a save before that second write finds a
/// sheet the call already changed.
#[tokio::test]
async fn add_power_symbol_reports_a_save_before_its_junction_as_uncertain() {
    let name = "two_name_nets.kicad_sch";
    // GND's pin on (110.49, 90.17), mid-span on the wire (110.49, 87.63)-(110.49, 92.71).
    let arguments = |path: &Path| {
        json!({ "schematic": path.display().to_string(), "power_net": "GND",
                "x": 110.49, "y": 90.17 })
    };
    let handler = handler().await;

    // Control: the junction is a second write.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, fixture(name)).unwrap();
    let (result, body) = call(&handler, "add_power_symbol", arguments(&path)).await;
    assert_ne!(result["isError"], json!(true), "{body}");
    assert_eq!(
        body["junctions_added"],
        json!([{ "x": 110.49, "y": 90.17 }]),
        "{body}"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let original = fixture(name);
    std::fs::write(&path, &original).unwrap();
    // The first write finds the file as read; the second finds a save.
    let saved = fixture("label_move_kicad10.kicad_sch");
    test_hooks::save_before_compare(&path, original);
    test_hooks::save_before_compare(&path, saved.clone());
    let (_, body) = call(&handler, "add_power_symbol", arguments(&path)).await;
    assert!(
        !test_hooks::armed(&path),
        "never reached the junction's write"
    );
    assert_eq!(
        body["error"]["kind"],
        json!("mutation_outcome_uncertain"),
        "{body}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

/// Dispatch reads the conflict out of any error chain, from either writer,
/// and never mistakes a readback mismatch, a lock, or a transaction's or an
/// item's conflict for one.
#[test]
fn only_a_revision_conflict_is_classified_as_one() {
    use crate::tools::revision_conflict_path;
    use copperpour_sexp::SexpError;
    let path = Path::new("/designs/board.kicad_pcb");

    let sexp = anyhow::Error::new(SexpError::Conflict {
        path: path.to_path_buf(),
    })
    .context("while adding a net");
    assert_eq!(revision_conflict_path(&sexp), Some(path));
    let editor = anyhow::Error::new(copperpour_schematic_editor::Error::Conflict(
        path.to_path_buf(),
    ));
    assert_eq!(revision_conflict_path(&editor), Some(path));

    let replaced = anyhow::Error::new(SexpError::ReadbackMismatch {
        path: path.to_path_buf(),
    });
    assert_eq!(revision_conflict_path(&replaced), None);
    let locked = anyhow::Error::new(SexpError::KiCadEditorLocked {
        path: path.to_path_buf(),
        lock_path: path.with_file_name("~board.kicad_pcb.lck"),
        inspection_error: None,
    });
    assert_eq!(revision_conflict_path(&locked), None);
    let transaction = anyhow::Error::new(SexpError::TransactionConflict {
        path: path.to_path_buf(),
        journal: path.with_extension("journal"),
        reason: "neither image".into(),
    });
    assert_eq!(revision_conflict_path(&transaction), None);
    let item = anyhow::Error::new(SexpError::ItemConflict {
        path: path.to_path_buf(),
        item: "R1".into(),
        reason: "changed".into(),
    });
    assert_eq!(revision_conflict_path(&item), None);
}

/// A KiCad editor lock that appears between the pre-write gate and the
/// writer's final refusal point must surface through dispatch as
/// `unsafe_file_fallback` for every closed-board footprint edit (#419).
#[tokio::test]
async fn a_kicad_lock_appearing_before_the_write_is_unsafe_file_fallback() {
    let handler = handler().await;
    let placement_fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../copperpour-sexp/tests/fixtures/placement/placement_fixture.kicad_pcb");
    let placement_source = std::fs::read_to_string(placement_fixture).unwrap();
    let flip_source = fixture("inner_layer_flip_front.kicad_pcb");
    let repeat_source = fixture("repeat_layout_kicad10.kicad_pcb");

    let board_cases: Vec<(&str, &str, Value)> = vec![
        (
            "move_component",
            &flip_source,
            json!({ "reference": "R1", "x": 120.0, "y": 110.0 }),
        ),
        (
            "rotate_component",
            &flip_source,
            json!({ "reference": "R1", "rotation": 90.0 }),
        ),
        (
            "set_component_placements",
            &flip_source,
            json!({ "placements": [{ "reference": "R1", "x": 120.0, "y": 110.0, "rotation": 90.0 }] }),
        ),
        (
            "flip_component",
            &flip_source,
            json!({ "reference": "R1", "layer": "B.Cu" }),
        ),
        (
            "place_decoupling_caps",
            &placement_source,
            json!({
                "ic_reference": "U1",
                "capacitor_references": ["C1", "C2"],
                "dry_run": false,
            }),
        ),
        (
            "refine_placement_force_directed",
            &placement_source,
            json!({
                "references": ["C1"],
                "max_displacement_mm": 20.0,
                "dry_run": false,
            }),
        ),
        (
            "repeat_layout",
            &repeat_source,
            json!({ "anchor_reference": "R1" }),
        ),
    ];

    let mut failures = Vec::new();
    for (tool, source, mut arguments) in board_cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.kicad_pcb");
        std::fs::write(&path, source).unwrap();
        test_hooks::lock_before_replace(&path, "someone@somewhere");
        arguments["board"] = json!(path.display().to_string());

        let (result, body) = call(&handler, tool, arguments).await;

        if test_hooks::armed(&path) {
            failures.push(format!("{tool}: never reached the write: {body}"));
            continue;
        }
        if std::fs::read_to_string(&path).unwrap() != source {
            failures.push(format!("{tool}: modified the board despite the lock"));
            continue;
        }
        if result["isError"] != json!(true)
            || body["error"]["kind"] != json!("unsafe_file_fallback")
            || body["error"]["path"] != json!(path.display().to_string())
            || body["error"]["reason"] != json!("kicad_lock_present")
        {
            failures.push(format!("{tool}: unexpected response: {body}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
