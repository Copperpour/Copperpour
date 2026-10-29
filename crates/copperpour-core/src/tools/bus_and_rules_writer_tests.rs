//! The bus tools and `set_layer_constraints` against a save between their
//! read and their write (#200).
//!
//! Each read the file, built its edit in memory and replaced the file
//! unconditionally, so a schematic or `.kicad_dru` saved in between went back
//! to the older read while both calls reported success. Driven through served
//! `tools/call`. The sheet is the KiCad-saved `label_move_kicad10.kicad_sch`
//! (see its README), whose R1 the fan-out case connects.

use crate::tools::project_file_writer_tests::{body_of, handler_talking_to, served_call, text_of};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SHEET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/label_move_kicad10.kicad_sch"
);

const BUS_TOOLS: [&str; 4] = [
    "add_bus",
    "batch_add_bus",
    "add_bus_entry",
    "connect_pins_to_bus",
];

fn bus_arguments(tool: &str, sheet: &Path) -> Value {
    let mut arguments = match tool {
        "add_bus" => json!({ "x1": 50.8, "y1": 20.32, "x2": 101.6, "y2": 20.32 }),
        "batch_add_bus" => {
            json!({ "buses": [{ "x1": 50.8, "y1": 20.32, "x2": 101.6, "y2": 20.32 }] })
        }
        "add_bus_entry" => json!({ "x": 60.96, "y": 17.78 }),
        "connect_pins_to_bus" => json!({
            "bus_y": 20.32,
            "connections": [{ "reference": "R1", "pin_number": "1", "net": "D0" }]
        }),
        other => panic!("no arguments for {other}"),
    };
    arguments["schematic"] = json!(sheet.to_string_lossy());
    arguments
}

/// Calls `tool` with a save of `newer` armed between its read and its write,
/// and checks it refused as `conflict` and kept `newer` byte for byte.
async fn assert_refused_and_kept(tool: &str, arguments: Value, file: &Path, newer: &str) {
    crate::tools::write_hook::set(newer.to_string());
    let result = served_call(&handler_talking_to("").await, tool, arguments).await;

    assert!(
        !crate::tools::write_hook::armed(),
        "{tool} bypassed the revision-checked write"
    );
    assert!(result.is_error, "{tool} reported success over a newer save");
    let body = body_of(&result);
    assert_eq!(body["error"]["kind"], "conflict", "{tool}: {body}");
    assert_eq!(
        body["error"]["paths"],
        json!([file.display().to_string()]),
        "{tool}: {body}"
    );
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        newer,
        "{tool} replaced the newer save"
    );
}

fn sheet_copy() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("label_move_kicad10.kicad_sch");
    std::fs::copy(SHEET, &sheet).unwrap();
    let source = std::fs::read_to_string(&sheet).unwrap();
    (dir, sheet, source)
}

#[tokio::test]
async fn each_bus_tool_refuses_a_sheet_saved_after_its_read() {
    for tool in BUS_TOOLS {
        let (_dir, sheet, source) = sheet_copy();
        // Another writer's no-connect, which the older read does not hold.
        let newer = crate::tools::sch_wiring::insert_before_close(
            &source,
            "(no_connect (at 25.4 25.4) (uuid \"2a6f7d1e-8f0b-4c55-9a4e-3f1c2b7d6e90\"))",
        );
        assert_refused_and_kept(tool, bus_arguments(tool, &sheet), &sheet, &newer).await;
    }
}

#[tokio::test]
async fn each_bus_tool_still_writes_an_unchanged_sheet() {
    for tool in BUS_TOOLS {
        let (_dir, sheet, source) = sheet_copy();
        let result = served_call(
            &handler_talking_to("").await,
            tool,
            bus_arguments(tool, &sheet),
        )
        .await;

        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        let written = std::fs::read_to_string(&sheet).unwrap();
        let tag = match tool {
            "add_bus_entry" => "(bus_entry",
            "connect_pins_to_bus" => "(label \"D0\"",
            // `(bus` alone would also count a bus entry.
            _ => "(bus\n",
        };
        assert!(
            written.matches(tag).count() > source.matches(tag).count(),
            "{tool} did not write its item"
        );
    }
}

/// A sheet removed or renamed after the read changed too: `conflict`, and no
/// file is written in its place.
#[tokio::test]
async fn a_sheet_removed_after_its_read_is_a_conflict() {
    let (_dir, sheet, source) = sheet_copy();
    std::fs::remove_file(&sheet).unwrap();

    let refusal =
        crate::tools::replace_if_unchanged(&sheet, "schematic", "add_bus", &source, source.clone())
            .unwrap()
            .expect("a refusal");

    assert_eq!(body_of(&refusal)["error"]["kind"], "conflict");
    assert!(!sheet.exists());
}

/// A call with nothing to add leaves the sheet's bytes alone.
#[tokio::test]
async fn bus_tools_with_nothing_to_add_do_not_write() {
    for (tool, mut arguments) in [
        ("batch_add_bus", json!({ "buses": [] })),
        (
            "connect_pins_to_bus",
            json!({ "bus_y": 20.32, "connections": [{ "reference": "R9", "pin_number": "1", "net": "D0" }] }),
        ),
    ] {
        let (_dir, sheet, source) = sheet_copy();
        arguments["schematic"] = json!(sheet.to_string_lossy());

        let result = served_call(&handler_talking_to("").await, tool, arguments).await;

        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), source, "{tool}");
    }
}

const LAYER_RULE: &str = "konnect:F.Cu:clearance";

fn rules_arguments(board: &Path) -> Value {
    json!({ "board": board.to_string_lossy(), "layer": "F.Cu", "min_clearance": 0.3 })
}

/// A rules file another writer saved: a hand-written rule this call must keep.
const NEWER_RULES: &str =
    "(version 1)\n(rule \"hand_written\"\n\t(constraint clearance (min 0.5mm)))\n";

#[tokio::test]
async fn set_layer_constraints_refuses_a_rules_file_saved_after_its_read() {
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("board.kicad_pcb");
    let rules = dir.path().join("board.kicad_dru");
    std::fs::write(&rules, "(version 1)\n").unwrap();

    assert_refused_and_kept(
        "set_layer_constraints",
        rules_arguments(&board),
        &rules,
        NEWER_RULES,
    )
    .await;
}

/// No rules file at the read: the write creates one, and does not replace a
/// file that appeared in between.
#[tokio::test]
async fn set_layer_constraints_refuses_a_rules_file_created_after_its_read() {
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("board.kicad_pcb");
    let rules = dir.path().join("board.kicad_dru");

    assert_refused_and_kept(
        "set_layer_constraints",
        rules_arguments(&board),
        &rules,
        NEWER_RULES,
    )
    .await;
}

#[tokio::test]
async fn set_layer_constraints_creates_then_updates_its_rules_file() {
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("board.kicad_pcb");
    let rules = dir.path().join("board.kicad_dru");
    let handler = handler_talking_to("").await;

    for _ in 0..2 {
        let result = served_call(&handler, "set_layer_constraints", rules_arguments(&board)).await;
        assert!(!result.is_error, "{}", text_of(&result));
    }

    let written = std::fs::read_to_string(&rules).unwrap();
    assert!(written.starts_with("(version 1)"), "{written}");
    assert_eq!(written.matches(LAYER_RULE).count(), 1, "{written}");
}
