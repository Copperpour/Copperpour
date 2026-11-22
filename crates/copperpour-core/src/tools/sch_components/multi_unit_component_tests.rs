use super::*;
use crate::mcp::{error::extract_error_kind, protocol::ToolContent};
use crate::tools::ServerConfig;
use std::sync::Arc;

const SCHEMATIC: &str = r#"(kicad_sch
  (version 20260306)
  (uuid "11111111-1111-4111-8111-111111111111")
  (lib_symbols
    (symbol "Test:DUAL"
      (symbol "DUAL_1_1"
        (pin input line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
      (symbol "DUAL_2_1"
        (pin output line (at 0 0 0) (length 0) (name "Y") (number "2"))
      )
    )
    (symbol "Test:DUAL_NEW"
      (symbol "DUAL_NEW_1_1"
        (pin input line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
      (symbol "DUAL_NEW_2_1"
        (pin output line (at 0 0 0) (length 0) (name "Y") (number "2"))
      )
    )
  )
  (symbol
    (lib_id "Test:DUAL")
    (at 100 100 0)
    (unit 1)
    (uuid "22222222-2222-4222-8222-222222222222")
    (property "Reference" "U1" (at 100 98 0))
    (property "Value" "OLD" (at 100 102 0))
    (property "Footprint" "" (at 100 100 0))
    (property "Datasheet" "" (at 100 100 0))
    (property "Note" "OLD" (at 100 100 0) (hide yes))
    (instances
      (project "multi"
        (path "/11111111-1111-4111-8111-111111111111"
          (reference "U1")
          (unit 1)
        )
      )
    )
  )
  (symbol
    (lib_id "Test:DUAL")
    (at 100 120 180)
    (unit 2)
    (uuid "33333333-3333-4333-8333-333333333333")
    (property "Reference" "U1" (at 100 118 0))
    (property "Value" "OLD" (at 100 122 0))
    (property "Footprint" "" (at 100 120 0))
    (property "Datasheet" "" (at 100 120 0))
    (instances
      (project "multi"
        (path "/11111111-1111-4111-8111-111111111111"
          (reference "U1")
          (unit 2)
        )
      )
    )
  )
  (sheet_instances (path "/" (page "1")))
)
"#;

fn context() -> ToolContext {
    ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(crate::router::ToolRouter::new()),
    )
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multi.kicad_sch");
    std::fs::write(&path, SCHEMATIC).unwrap();
    (directory, path)
}

/// A real eeschema save (KiCad's ecc83 demo): tabs, CRLF, and U1 placed
/// as units 2 and 3 of the embedded `ecc83-pp:ECC83` dual triode. The
/// hand-written `SCHEMATIC` above shares this module's own serialization
/// habits, so only this file exercises the indentation- and
/// dialect-matching branches against what KiCad actually writes.
fn eeschema_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ecc83.kicad_sch");
    std::fs::write(
        &path,
        include_str!("../../../tests/fixtures/ecc83_multiunit.kicad_sch"),
    )
    .unwrap();
    (directory, path)
}

fn native_readback_intent_mismatch(field: &str) {
    let (_directory, path) = eeschema_fixture();
    let mut committed = cse::Schematic::load(&path).unwrap();
    let target = component_target_from_source(&path, &committed.to_source(), "U1").unwrap();
    assert!(verified_component_readback(&path, &committed, &target).is_ok());
    // Keep UUID and Reference intact. Change every unit's hierarchy together
    // so the expected-path comparison, not a cross-unit conflict, must fire.
    for symbol in committed
        .symbols
        .iter_mut()
        .filter(|symbol| symbol.reference() == Some("U1"))
    {
        match field {
            "unit" => {
                symbol.unit += 10;
                for (project, instance_path) in symbol.instance_paths() {
                    symbol.set_instance_path(&project, &instance_path, "U1", symbol.unit);
                }
            }
            "lib_id" => symbol.lib_id = "Device:WRONG".to_owned(),
            "x" => symbol.at.x += 1.27,
            "y" => symbol.at.y += 1.27,
            "rotation" => symbol.at.rotation = Some(symbol.at.rotation.unwrap_or(0.0) + 90.0),
            "mirror" => symbol.set_mirror(Some("y")),
            "project" | "path" => {
                let previous = symbol.instance_paths();
                symbol
                    .raw_sub_nodes
                    .retain(|node| node.tag() != Some("instances"));
                for (project, instance_path) in previous {
                    symbol.set_instance_path(
                        if field == "project" {
                            "wrong-project"
                        } else {
                            &project
                        },
                        if field == "path" {
                            "/wrong/path"
                        } else {
                            &instance_path
                        },
                        "U1",
                        symbol.unit,
                    );
                }
            }
            _ => unreachable!(),
        }
    }
    committed.overwrite().unwrap();
    let reloaded = cse::Schematic::load(&path).unwrap();
    let error = verified_component_readback(&path, &reloaded, &target).unwrap_err();
    assert_eq!(
        extract_error_kind(&error).as_deref(),
        Some("stale_target"),
        "{field}: {error:?}"
    );
}

#[test]
fn native_readback_intent_unit() {
    native_readback_intent_mismatch("unit");
}
#[test]
fn native_readback_intent_library() {
    native_readback_intent_mismatch("lib_id");
}
#[test]
fn native_readback_intent_x() {
    native_readback_intent_mismatch("x");
}
#[test]
fn native_readback_intent_y() {
    native_readback_intent_mismatch("y");
}
#[test]
fn native_readback_intent_rotation() {
    native_readback_intent_mismatch("rotation");
}
/// A reflection that reaches the response but not the file — or one that
/// appears in the file without being asked for — must be caught by the
/// same readback that already guards rotation. Mirror was bound as
/// placement intent for exactly this (#450).
#[test]
fn native_readback_intent_mirror() {
    native_readback_intent_mismatch("mirror");
}
#[test]
fn native_readback_intent_project() {
    native_readback_intent_mismatch("project");
}
#[test]
fn native_readback_intent_path() {
    native_readback_intent_mismatch("path");
}

#[test]
fn native_readback_intent_requested_properties() {
    for field in ["Value", "Footprint", "Datasheet", "MPN", "Group"] {
        let (_directory, path) = eeschema_fixture();
        let mut committed = cse::Schematic::load(&path).unwrap();
        let target = component_target_from_source(&path, &committed.to_source(), "U1")
            .unwrap()
            .with_fields(&BTreeMap::from([(
                field.to_owned(),
                "requested-value".to_owned(),
            )]));
        for symbol in committed
            .symbols
            .iter_mut()
            .filter(|symbol| symbol.reference() == Some("U1"))
        {
            symbol.set_property(field, "requested-value");
        }
        committed.overwrite().unwrap();
        assert!(
            verified_component_readback(&path, &cse::Schematic::load(&path).unwrap(), &target)
                .is_ok()
        );
        committed
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference() == Some("U1"))
            .unwrap()
            .set_property(field, "wrong-value");
        committed.overwrite().unwrap();
        let error =
            verified_component_readback(&path, &cse::Schematic::load(&path).unwrap(), &target)
                .unwrap_err();
        assert_eq!(
            extract_error_kind(&error).as_deref(),
            Some("stale_target"),
            "{field}"
        );
    }
}

#[test]
fn native_readback_instance_conflicts() {
    for corruption in ["project", "duplicate", "unit", "cross-unit"] {
        let (_directory, path) = eeschema_fixture();
        let mut committed = cse::Schematic::load(&path).unwrap();
        let target = component_target_from_source(&path, &committed.to_source(), "U1").unwrap();
        let symbol = committed
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference() == Some("U1"))
            .unwrap();
        let (project, instance_path) = symbol.instance_paths()[0].clone();
        match corruption {
            "project" => {
                symbol.set_instance_path("other-project", &instance_path, "U1", symbol.unit)
            }
            "unit" => symbol.set_instance_path(&project, &instance_path, "U1", symbol.unit + 1),
            "cross-unit" => {
                symbol
                    .raw_sub_nodes
                    .retain(|node| node.tag() != Some("instances"));
                symbol.set_instance_path(&project, "/other/path", "U1", symbol.unit);
            }
            "duplicate" => {
                let instances = symbol
                    .raw_sub_nodes
                    .iter()
                    .find(|node| node.tag() == Some("instances"))
                    .unwrap()
                    .clone();
                symbol.raw_sub_nodes.push(instances);
            }
            _ => unreachable!(),
        }
        if corruption != "cross-unit" {
            let error = checked_instance_paths(&path, symbol)
                .unwrap_err()
                .into_result();
            assert_eq!(
                extract_error_kind(&error).as_deref(),
                Some("ambiguous_target"),
                "{corruption}"
            );
        }
        committed.overwrite().unwrap();
        let reloaded = cse::Schematic::load(&path).unwrap();
        let error = verified_component_readback(&path, &reloaded, &target).unwrap_err();
        assert_eq!(
            extract_error_kind(&error).as_deref(),
            Some("ambiguous_target"),
            "{corruption}"
        );
        let preflight = component_target_from_source(&path, &reloaded.to_source(), "U1")
            .unwrap_err()
            .into_result();
        assert_eq!(
            extract_error_kind(&preflight).as_deref(),
            Some("ambiguous_target"),
            "{corruption}"
        );
    }
}

/// The nested fixture's `bank.kicad_sch`, saved by eeschema, is placed as
/// south (page 2) and north (page 10). eeschema writes each `Reference`
/// property from the first instance in page order, so R3's property shows
/// south's R3 while its north record names R10.
fn sheet_placed_twice() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bank.kicad_sch");
    std::fs::write(
        &path,
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/bank.kicad_sch"),
    )
    .unwrap();
    (directory, path)
}

#[tokio::test]
async fn a_field_moves_on_a_sheet_placed_twice() {
    let (_directory, path) = sheet_placed_twice();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R3",
                "field_placements": { "Value": { "x": 162.56, "y": 53.34 } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    let unit = &result["units"][0];
    assert_eq!(unit["field_placements"]["Value"]["x"], 162.56);
    assert_eq!(unit["field_placements"]["Value"]["y"], 53.34);
    assert_eq!(unit["fields"]["Reference"], "R3");
    assert_eq!(unit["instance_references"], json!(["R10", "R3"]));
}

/// A record the property does not agree with is still stale, before the
/// write and in the readback: a property that no record names (a rename
/// that reached the property alone), a record of another prefix, and a
/// record without a designator.
#[tokio::test]
async fn a_stale_record_is_refused_on_a_sheet_placed_twice() {
    for (corruption, from, to, reference, reason) in [
        (
            "missing",
            "\t\t\t\t\t(reference \"R10\")\n",
            "",
            "R3",
            "has incomplete instance metadata",
        ),
        (
            "renamed",
            "(property \"Reference\" \"R3\"",
            "(property \"Reference\" \"R99\"",
            "R99",
            "its records name R10, R3 and its Reference is R99",
        ),
        (
            "prefix",
            "(reference \"R10\")",
            "(reference \"C10\")",
            "R3",
            "its records name C10, R3 and its Reference is R3",
        ),
    ] {
        let (_directory, path) = sheet_placed_twice();
        let source = std::fs::read_to_string(&path).unwrap();
        assert_eq!(source.matches(from).count(), 1, "{corruption}");
        let source = source.replacen(from, to, 1);
        std::fs::write(&path, &source).unwrap();
        let refused = handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": reference,
                "field_placements": { "Value": { "x": 162.56, "y": 53.34 } }
            }),
            &context(),
        )
        .await
        .unwrap();
        assert_eq!(
            extract_error_kind(&refused).as_deref(),
            Some("stale_target"),
            "{corruption}"
        );
        assert!(
            tool_error_summary(&refused).contains(reason),
            "{corruption}: {}",
            tool_error_summary(&refused)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);

        let committed = cse::Schematic::load(&path).unwrap();
        let readback = component_mutation_readback_from_schematic(
            &path,
            &committed,
            &["57ee0d14-2c2d-442a-b0f1-343e05b94ebd".to_owned()],
            None,
        )
        .unwrap_err();
        assert_eq!(
            extract_error_kind(&readback).as_deref(),
            Some("stale_target"),
            "{corruption}"
        );
        assert!(
            tool_error_summary(&readback).contains(reason),
            "{corruption}"
        );
    }
}

/// The readback holds each record's designator to what was bound, so a
/// write that renumbers one is caught although the property still names
/// a record.
#[test]
fn a_renumbered_record_fails_the_readback_on_a_sheet_placed_twice() {
    let (_directory, path) = sheet_placed_twice();
    let source = std::fs::read_to_string(&path).unwrap();
    let target = component_target_from_source(&path, &source, "R3").unwrap();
    assert!(
        verified_component_readback(&path, &cse::Schematic::load(&path).unwrap(), &target).is_ok()
    );
    assert_eq!(source.matches("(reference \"R10\")").count(), 1);
    std::fs::write(
        &path,
        source.replacen("(reference \"R10\")", "(reference \"R12\")", 1),
    )
    .unwrap();
    let error = verified_component_readback(&path, &cse::Schematic::load(&path).unwrap(), &target)
        .unwrap_err();
    assert_eq!(extract_error_kind(&error).as_deref(), Some("stale_target"));
    assert!(tool_error_summary(&error).contains("observed instance_references differs"));
}

/// A rename would reach only the instance whose record carries the old
/// designator, so on a sheet placed twice it is refused before writing.
#[tokio::test]
async fn a_rename_is_ambiguous_on_a_sheet_placed_twice() {
    let (_directory, path) = sheet_placed_twice();
    let before = std::fs::read_to_string(&path).unwrap();
    let refused = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "R3", "new_reference": "R17" }),
        &context(),
    )
    .await
    .unwrap();
    assert_eq!(
        extract_error_kind(&refused).as_deref(),
        Some("ambiguous_target")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

const NESTED_PAGES: [(&str, &str); 4] = [
    (
        "nested_pages.kicad_pro",
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_pro"),
    ),
    (
        "nested_pages.kicad_sch",
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_sch"),
    ),
    (
        "bank.kicad_sch",
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/bank.kicad_sch"),
    ),
    (
        "cell.kicad_sch",
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/cell.kicad_sch"),
    ),
];

/// The nested fixture's project, with [`write_fixture_lib_table`]'s
/// project table.
fn nested_pages_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (name, content) in NESTED_PAGES {
        std::fs::write(directory.path().join(name), content).unwrap();
    }
    write_fixture_lib_table(directory.path());
    directory
}

/// A project table of two libraries KiCad 10.0.6 saved: `Device`, holding
/// the stock `Device:R`, and `Fields`, whose two-pin `FIELDS_BARE`
/// `create_symbol` made. The project table is read first, so the
/// developer's installed libraries cannot answer.
fn write_fixture_lib_table(directory: &std::path::Path) {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let lib = |name: &str, file: &str| {
        format!(
            "  (lib (name \"{name}\") (type \"KiCad\") (uri {}) (options \"\") (descr \"\"))\n",
            crate::tools::library::quote_lib_table_string(
                &fixtures.join(file).display().to_string()
            )
        )
    };
    std::fs::write(
        directory.join("sym-lib-table"),
        format!(
            "(sym_lib_table\n  (version 7)\n{}{})\n",
            lib("Device", "reference_prefix_kicad10/Device.kicad_sym"),
            lib("Fields", "symbol_fields_kicad10.kicad_sym")
        ),
    )
    .unwrap();
}

async fn served_handler() -> crate::mcp::handler::McpHandler {
    crate::mcp::handler::McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: false,
        eager_toolsets: true,
    })
    .await
    .unwrap()
}

/// A `tools/call` the way a client makes it, which must succeed.
async fn served_call(
    handler: &crate::mcp::handler::McpHandler,
    tool: &str,
    arguments: &serde_json::Value,
) {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    let result = response.result.expect("a JSON-RPC result");
    assert_ne!(
        result["isError"],
        json!(true),
        "{tool} {arguments}: {result}"
    );
}

/// Each edit tool, served as a client calls it, on the whole project: it
/// succeeds, changes the part's sheet and no other, and the part keeps
/// every record. R3's records read `R10, R3`, already sorted; R6's, on
/// `cell.kicad_sch` placed four times, read `R15, R13, R6, R8`.
#[tokio::test]
async fn every_edit_tool_is_served_on_a_sheet_placed_twice() {
    let handler = served_handler().await;
    for (sheet, reference, uuid, records) in [
        (
            "bank.kicad_sch",
            "R3",
            "57ee0d14-2c2d-442a-b0f1-343e05b94ebd",
            &["R10", "R3"][..],
        ),
        (
            "cell.kicad_sch",
            "R6",
            "9dfa3ea7-feb4-46a5-8eec-8b2d81900c0c",
            &["R13", "R15", "R6", "R8"][..],
        ),
    ] {
        for (tool, arguments) in [
            (
                "edit_schematic_component",
                json!({"field_placements": {"Value": {"x": 162.56, "y": 53.34}}}),
            ),
            ("edit_schematic_component", json!({"value": "10k"})),
            ("move_schematic_component", json!({"x": 170.18, "y": 50.8})),
            ("rotate_schematic_component", json!({"rotation": 180.0})),
            ("mirror_schematic_component", json!({"mirror": "y"})),
            (
                "add_component_annotation",
                json!({"key": "Note", "value": "x"}),
            ),
            ("group_components", json!({"group_name": "G"})),
            (
                "replace_component",
                json!({"new_lib_id": "Fields:FIELDS_BARE"}),
            ),
        ] {
            let directory = nested_pages_project();
            let path = directory.path().join(sheet);
            let mut arguments = arguments;
            arguments["schematic"] = json!(path.display().to_string());
            if tool == "group_components" {
                arguments["references"] = json!([reference]);
            } else {
                arguments["reference"] = json!(reference);
            }
            served_call(&handler, tool, &arguments).await;
            // The edit lands in the part's sheet alone.
            for (name, content) in NESTED_PAGES {
                if !name.ends_with(".kicad_sch") {
                    continue;
                }
                let now = std::fs::read_to_string(directory.path().join(name)).unwrap();
                if name == sheet {
                    assert_ne!(
                        now, content,
                        "{reference} {tool}: the edit reached the file"
                    );
                } else {
                    assert_eq!(now, content, "{reference} {tool}: {name}");
                }
            }
            let committed = cse::Schematic::load(&path).unwrap();
            let symbol = committed
                .symbols
                .iter()
                .find(|symbol| symbol.uuid == uuid)
                .unwrap();
            assert_eq!(symbol.reference(), Some(reference), "{tool}");
            let mut found = symbol
                .instances()
                .into_iter()
                .filter_map(|instance| instance.reference)
                .collect::<Vec<_>>();
            found.sort();
            assert_eq!(found, records, "{reference} {tool} {arguments}");
        }
    }
}

/// A placement writes its one designator to every record, so a part
/// placed on a reused sheet repeats it: the edit tools bind it as one
/// designator, and a rename reaches both records.
#[tokio::test]
async fn a_part_placed_on_a_sheet_placed_twice_is_edited_and_renamed() {
    let handler = served_handler().await;
    let directory = nested_pages_project();
    let bank = directory.path().join("bank.kicad_sch");
    let records = |reference: &str| {
        let committed = cse::Schematic::load(&bank).unwrap();
        let symbol = committed
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .unwrap();
        let found = symbol
            .instances()
            .into_iter()
            .filter_map(|instance| instance.reference)
            .collect::<Vec<_>>();
        (found, symbol.property("Value").map(str::to_owned))
    };
    served_call(
        &handler,
        "add_schematic_component",
        &json!({"schematic": bank, "lib_id": "Device:R", "reference": "R50",
                "x": 228.6, "y": 76.2}),
    )
    .await;
    assert_eq!(records("R50").0, ["R50", "R50"]);
    for (tool, arguments) in [
        ("edit_schematic_component", json!({"value": "10k"})),
        ("move_schematic_component", json!({"x": 228.6, "y": 88.9})),
        ("edit_schematic_component", json!({"new_reference": "R51"})),
    ] {
        let mut arguments = arguments;
        arguments["schematic"] = json!(bank);
        arguments["reference"] = json!("R50");
        served_call(&handler, tool, &arguments).await;
    }
    assert_eq!(
        records("R51"),
        (
            vec!["R51".to_owned(), "R51".to_owned()],
            Some("10k".to_owned())
        )
    );
}

#[tokio::test]
async fn native_mutations_verify_each_units_intended_state() {
    let (_directory, path) = eeschema_fixture();
    body(
        handle_move_schematic_component(
            &json!({"schematic": path, "reference": "U1", "x": 110.1, "y": 120.2}),
            &context(),
        )
        .await
        .unwrap(),
    );
    body(
        handle_rotate_schematic_component(
            &json!({"schematic": path, "reference": "U1", "rotation": 450.0}),
            &context(),
        )
        .await
        .unwrap(),
    );
    let edited = body(handle_edit_schematic_component(&json!({"schematic": path, "reference": "U1", "new_reference": "U99", "value": "updated", "footprint": "Package:New", "datasheet": "https://example.invalid/datasheet", "fields": {"MPN": "requested"}}), &context()).await.unwrap());
    assert_eq!(edited["reference"], "U99");
    for unit in edited["units"].as_array().unwrap() {
        assert_eq!(unit["fields"]["MPN"], "requested");
        assert_eq!(unit["fields"]["Value"], "updated");
        assert_eq!(unit["fields"]["Footprint"], "Package:New");
        assert_eq!(
            unit["fields"]["Datasheet"],
            "https://example.invalid/datasheet"
        );
    }
    let grouped = body(
        handle_group_components(
            &json!({"schematic": path, "references": ["U99"], "group_name": "native-group"}),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(grouped["grouped_count"], 1);
}

#[tokio::test]
async fn a_real_eeschema_multi_unit_component_is_seen_whole() {
    let (_directory, path) = eeschema_fixture();
    let result = body(
        handle_get_schematic_component(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        result["unit_count"], 3,
        "U1 is placed as both triodes plus the heater unit"
    );
    let mut units: Vec<i64> = result["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| unit["unit"].as_i64().unwrap())
        .collect();
    units.sort_unstable();
    assert_eq!(units, [1, 2, 3]);
}

#[tokio::test]
async fn annotating_a_real_eeschema_file_reaches_every_unit_in_its_own_dialect() {
    let (_directory, path) = eeschema_fixture();
    let result = body(
        handle_add_component_annotation(
            &json!({
                "schematic": path,
                "reference": "U1",
                "key": "MPN",
                "value": "ECC83-JJ"
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["added_units"], 3, "{result}");
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        source.matches("(property \"MPN\" \"ECC83-JJ\"").count(),
        3,
        "the property lands in every unit block"
    );
    // The inserted lines must follow the file's own indentation (tabs) —
    // a 2-space insert in a tab-indented eeschema file is exactly the
    // drift the KiCad-authored fixture exists to catch.
    for line in source.lines().filter(|line| line.contains("\"MPN\"")) {
        assert!(
            line.starts_with('\t'),
            "inserted property must be tab-indented like its file: {line:?}"
        );
    }
}

fn body(result: CallToolResult) -> serde_json::Value {
    assert!(!result.is_error, "mutation unexpectedly failed: {result:?}");
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text result");
    };
    serde_json::from_str(text).unwrap()
}

fn instances(path: &std::path::Path) -> Vec<copperpour_sexp::schematic::SymbolInstance> {
    let (_, tree) = read_schematic(path).unwrap();
    extract_symbol_instances(&tree)
        .into_iter()
        .filter(|instance| instance.reference == "U1" || instance.reference == "U9")
        .collect()
}

#[tokio::test]
async fn delete_removes_every_placed_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["deleted_units"], 2);
    assert!(instances(&path).is_empty());
}

#[tokio::test]
async fn move_translates_every_unit_by_one_shared_delta() {
    let (_directory, path) = fixture();
    let result = body(
        handle_move_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "x": 110.0,
                "y": 110.0
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["moved_units"], 2);
    assert_eq!(result["schematic"], path.display().to_string());
    assert_eq!(result["units"].as_array().unwrap().len(), 2);
    assert_eq!(result["placements"], result["units"]);
    let mut placed = instances(&path);
    placed.sort_by_key(|instance| instance.unit);
    assert!((placed[0].x - placed[1].x).abs() < 0.001);
    assert!(((placed[1].y - placed[0].y) - 20.0).abs() < 0.001);
}

/// U1's placements in the real eeschema save, by unit: x, y.
fn ecc83_units(path: &std::path::Path) -> Vec<(u32, f64, f64)> {
    let (_, tree) = read_schematic(path).unwrap();
    let mut placed = extract_symbol_instances(&tree)
        .into_iter()
        .filter(|instance| instance.reference == "U1")
        .map(|instance| (instance.unit, instance.x, instance.y))
        .collect::<Vec<_>>();
    placed.sort_by_key(|unit| unit.0);
    placed
}

/// With `unit`, that placement alone goes to x, y; the saved file holds
/// the other units where KiCad put them, byte for byte, and the reply says
/// one unit moved. Without it, every unit moves
/// (`move_translates_every_unit_by_one_shared_delta`).
#[tokio::test]
async fn move_with_unit_moves_only_that_placement() {
    let (_directory, path) = eeschema_fixture();
    assert_eq!(
        ecc83_units(&path),
        vec![(1, 160.02, 64.77), (2, 157.48, 107.95), (3, 63.5, 74.93)],
        "fixture premise"
    );
    let before = std::fs::read_to_string(&path).unwrap();
    let schema = tools()
        .into_iter()
        .find(|tool| tool.name == "move_schematic_component")
        .unwrap()
        .input_schema;
    assert_eq!(schema["properties"]["unit"]["type"], "integer");
    let result = body(
        handle_move_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "x": 101.6,
                "y": 50.8,
                "unit": 2
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["moved_units"], 1, "{result}");
    let moved = result["placements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| {
            (
                unit["unit"].as_u64().unwrap(),
                unit["x"].clone(),
                unit["y"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(moved.len(), 3, "{result}");
    assert_eq!(moved[1], (2, json!(101.6), json!(50.8)), "{result}");
    let after = ecc83_units(&path);
    assert_eq!(after[1], (2, 101.6, 50.8));
    assert_eq!((after[0], after[2]), ((1, 160.02, 64.77), (3, 63.5, 74.93)));
    // Units 1 and 3 are the same text, not just the same numbers.
    let committed = std::fs::read_to_string(&path).unwrap();
    for number in [1, 3] {
        let block = |source: &str| {
            let marker = format!("(unit {number})");
            let at = source.find("(lib_id \"ecc83-pp:ECC83\")").unwrap();
            let at = source[at..].find(&marker).map(|i| i + at).unwrap();
            let open = source[..at].rfind("(symbol").unwrap();
            let close = source[at..].find("(instances").unwrap() + at;
            source[open..close].to_owned()
        };
        assert_eq!(block(&before), block(&committed), "unit {number} changed");
    }
}

/// A unit the component has no placement for, 0 included, is an invalid
/// argument that names the placed units, and nothing is written.
#[tokio::test]
async fn move_with_an_unplaced_unit_is_refused_without_writing() {
    for unit in [0, 4] {
        let (_directory, path) = eeschema_fixture();
        let before = std::fs::read_to_string(&path).unwrap();
        let result = handle_move_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "x": 101.6,
                "y": 50.8,
                "unit": unit
            }),
            &context(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "unit {unit}");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument"),
            "unit {unit}"
        );
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text");
        };
        assert!(text.contains("placed: 1, 2, 3"), "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}

/// A second placement of a unit that is already placed, a state KiCad
/// reports as duplicate items, placed unannotated and renamed, since a
/// placement under the designator is refused. A move that names that unit
/// is refused as an ambiguous target, as every component edit refuses it,
/// and writes nothing.
#[tokio::test]
async fn move_with_a_unit_on_two_placements_is_ambiguous_and_writes_nothing() {
    let (_directory, path) = eeschema_placement_fixture();
    let placed = handle_add_schematic_component(
        &json!({
            "schematic": path,
            "lib_id": "ecc83-pp:ECC83",
            "x": 101.6,
            "y": 25.4,
            "unit": 1
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");
    let renamed = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "U?", "new_reference": "U1" }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!renamed.is_error, "{renamed:?}");
    let before = std::fs::read_to_string(&path).unwrap();
    let result = handle_move_schematic_component(
        &json!({
            "schematic": path,
            "reference": "U1",
            "x": 101.6,
            "y": 50.8,
            "unit": 1
        }),
        &context(),
    )
    .await
    .unwrap();
    assert_eq!(
        crate::mcp::error::extract_error_kind(&result).as_deref(),
        Some("ambiguous_target")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Moving a field's text must move the text and nothing else: the value
/// stays, the symbol's own `(at ...)` stays, and the position is read back
/// from the committed file rather than echoed.
#[tokio::test]
async fn field_placement_moves_the_text_and_keeps_its_value() {
    let (_directory, path) = eeschema_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let body_at = before
        .find("(property \"Reference\" \"P3\"")
        .map(|at| before[..at].to_owned())
        .expect("P3 exists");
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "P3",
                "field_placements": { "Value": { "x": 12.5, "y": 34.5, "rotation": 90.0 } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    let observed = &result["units"][0]["field_placements"]["Value"];
    assert_eq!(observed["x"], 12.5);
    assert_eq!(observed["y"], 34.5);
    assert_eq!(observed["rotation"], 90.0);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("(at 12.5 34.5 90)"));
    assert_eq!(
        after[..after.find("(property \"Reference\" \"P3\"").unwrap()],
        body_at,
        "nothing before the edited symbol may change"
    );
}

/// KiCad stores field `(at x y rot)` relative to the parent symbol, so on
/// a 90° or 270° symbol `rotation: 90` draws horizontally and `rotation: 0`
/// draws vertically. Both `edit_schematic_component` and
/// `batch_edit_schematic_components` keep the stored symbol-relative angle,
/// report `drawn_orientation` (`horizontal`/`vertical`) in each unit's
/// `field_placements`, and document that contract in the schema.
#[tokio::test]
async fn field_placement_rotation_is_symbol_relative_and_reports_drawn_orientation() {
    let (_directory, path) = eeschema_fixture();
    let handler = served_handler().await;

    let schema = field_placements_schema();
    let rot_desc = schema["additionalProperties"]["properties"]["rotation"]["description"]
        .as_str()
        .expect("rotation schema description");
    assert!(rot_desc.contains("relative to the parent symbol"));
    assert!(rot_desc.contains("drawn_orientation"));

    async fn call_json(
        handler: &crate::mcp::handler::McpHandler,
        tool: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("a JSON-RPC result");
        assert_ne!(result["isError"], json!(true), "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().expect("text body");
        serde_json::from_str(text).expect("json body")
    }

    // C2 is placed at 270° in ecc83_multiunit.kicad_sch.
    for (rot, expected_orientation) in [(90.0, "horizontal"), (0.0, "vertical")] {
        let res = call_json(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": path,
                "reference": "C2",
                "field_placements": { "Value": { "rotation": rot } }
            }),
        )
        .await;
        let val = &res["units"][0]["field_placements"]["Value"];
        assert_eq!(val["rotation"], rot);
        assert_eq!(val["drawn_orientation"], expected_orientation);
    }

    // Rotate P3 (initially 0°) to 90° and verify the same swap.
    call_json(
        &handler,
        "rotate_schematic_component",
        json!({ "schematic": path, "reference": "P3", "rotation": 90 }),
    )
    .await;
    for (rot, expected_orientation) in [(90.0, "horizontal"), (0.0, "vertical")] {
        let res = call_json(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": path,
                "reference": "P3",
                "field_placements": { "Value": { "rotation": rot } }
            }),
        )
        .await;
        let val = &res["units"][0]["field_placements"]["Value"];
        assert_eq!(val["rotation"], rot);
        assert_eq!(val["drawn_orientation"], expected_orientation);
    }

    // Sibling caller: batch_edit_schematic_components on 180° R1 and 270° C2.
    let batch_res = call_json(
        &handler,
        "batch_edit_schematic_components",
        json!({
            "schematic": path,
            "edits": [
                { "reference": "R1", "field_placements": { "Value": { "rotation": 90 } } },
                { "reference": "C2", "field_placements": { "Reference": { "rotation": 90 } } }
            ]
        }),
    )
    .await;
    assert_eq!(
        batch_res["updated"][0]["units"][0]["field_placements"]["Value"]["drawn_orientation"],
        "vertical"
    );
    assert_eq!(
        batch_res["updated"][1]["units"][0]["field_placements"]["Reference"]["drawn_orientation"],
        "horizontal"
    );
}

/// `hide` writes `(hide yes)` as a direct child of the placed symbol's
/// property. That is not the form KiCad's own writer uses on a placement
/// — it nests the flag in `(effects ...)` there, and uses the direct
/// child in `lib_symbols` — but the two are equivalent to KiCad 10.0.6,
/// which exports byte-identical SVG from either and round-trips this one
/// unchanged. The other form is covered by
/// `field_placement_reads_the_hidden_flag_kicad_nests_in_effects`.
#[tokio::test]
async fn field_placement_hides_then_shows_a_field() {
    let (_directory, path) = eeschema_fixture();
    for (hide, expected) in [(true, 1usize), (false, 0usize)] {
        let result = handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "P3",
                "field_placements": { "Value": { "hide": hide } }
            }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let after = std::fs::read_to_string(&path).unwrap();
        // Scope to P3's own Value property. No `unwrap_or(0)` fallback:
        // a search that finds nothing must fail the test, not silently
        // count over the whole file.
        let value_start = after
            .find("(property \"Value\" \"POWER\"")
            .expect("P3 carries a Value property");
        let (_, value_end) =
            copperpour_sexp::writer::find_enclosing_block(&after, "property", value_start)
                .expect("the Value property is a balanced block");
        let block = &after[value_start..value_end];
        assert_eq!(
            block.matches("(hide yes)").count(),
            expected,
            "hide={hide} produced {block}"
        );
    }
}

/// KiCad nests a *placement's* hidden flag inside the property's
/// `(effects …)`; the property-level token is what `lib_symbols`
/// definitions carry. Reading only the property's direct children
/// therefore reported every field KiCad itself hid as visible, and —
/// because the writer's own check is textual and does see the nested
/// token — turned "hide an already-hidden field" into a post-write
/// refusal over a file that was already correct. This fixture is real
/// KiCad output and hides Datasheet and Description in KiCad's own form.
#[tokio::test]
async fn field_placement_reads_the_hidden_flag_kicad_nests_in_effects() {
    let (_directory, path) = eeschema_fixture();
    // Scope every count to R1's own placement. No whole-file count and no
    // `unwrap_or(0)`: a search that finds nothing must fail the test.
    let hidden_tokens = |content: &str| {
        let blocks = find_all_symbol_instance_blocks(content, "R1");
        assert_eq!(blocks.len(), 1, "R1 is placed exactly once here");
        let (start, end) = blocks[0];
        content[start..end].matches("(hide yes)").count()
    };
    assert_eq!(
        hidden_tokens(&std::fs::read_to_string(&path).unwrap()),
        2,
        "the fixture hides Datasheet and Description"
    );

    // The readback describes the file, not merely its direct children.
    let observed = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R1",
                "field_placements": { "Value": { "rotation": 90.0 } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    for field in ["Datasheet", "Description"] {
        assert_eq!(
            observed["units"][0]["field_placements"][field]["hide"],
            json!(true),
            "{field} is hidden in the file"
        );
    }

    // Hiding one again is a no-op the post-write comparison must accept,
    // and it must not write a second token beside KiCad's.
    let before = hidden_tokens(&std::fs::read_to_string(&path).unwrap());
    let observed = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R1",
                "field_placements": { "Datasheet": { "hide": true } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        observed["units"][0]["field_placements"]["Datasheet"]["hide"],
        json!(true)
    );
    assert_eq!(
        hidden_tokens(&std::fs::read_to_string(&path).unwrap()),
        before,
        "hiding an already-hidden field must not add a second token"
    );

    // Showing it removes KiCad's nested token, leaving the other alone.
    let observed = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R1",
                "field_placements": { "Datasheet": { "hide": false } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        observed["units"][0]["field_placements"]["Datasheet"]["hide"],
        json!(false)
    );
    assert_eq!(
        observed["units"][0]["field_placements"]["Description"]["hide"],
        json!(true),
        "the neighbouring hidden field is untouched"
    );
    assert_eq!(
        hidden_tokens(&std::fs::read_to_string(&path).unwrap()),
        before - 1
    );
}

/// The post-write comparison, tested directly: a silent write failure
/// cannot be induced through the tool, so the guard is exercised here.
#[test]
fn placement_landed_compares_only_what_was_requested() {
    let seen = json!({ "x": 10.0, "y": 20.0, "rotation": 90.0, "hide": false });
    let want = |x, y, rotation, hide| FieldPlacement {
        x,
        y,
        rotation,
        hide,
    };
    assert!(placement_landed(&seen, &want(Some(10.0), None, None, None)));
    assert!(placement_landed(
        &seen,
        &want(None, None, None, Some(false))
    ));
    assert!(placement_landed(&seen, &want(None, None, None, None)));
    assert!(!placement_landed(
        &seen,
        &want(Some(10.5), None, None, None)
    ));
    assert!(!placement_landed(
        &seen,
        &want(None, Some(21.0), None, None)
    ));
    assert!(!placement_landed(&seen, &want(None, None, Some(0.0), None)));
    assert!(!placement_landed(
        &seen,
        &want(None, None, None, Some(true))
    ));
    // A field the readback never saw cannot be reported as placed.
    assert!(!placement_landed(
        &json!(null),
        &want(Some(10.0), None, None, None)
    ));
}

#[test]
fn placement_landed_compares_normalised_rotation() {
    let seen_90 = json!({ "x": 10.0, "y": 20.0, "rotation": 90.0, "hide": false });
    let seen_450 = json!({ "x": 10.0, "y": 20.0, "rotation": 450.0, "hide": false });
    let want = |rotation| FieldPlacement {
        x: None,
        y: None,
        rotation: Some(rotation),
        hide: None,
    };
    assert!(placement_landed(&seen_90, &want(450.0)));
    assert!(placement_landed(&seen_90, &want(-270.0)));
    assert!(!placement_landed(&seen_450, &want(450.0)));
}

/// `450` and `-90` are written and reported as `90` and `270`, the form
/// `rotate_schematic_component` writes. KiCad 10.0.8 keeps a field angle
/// as written on resave, so this is a canonical form, not a resave match.
#[tokio::test]
async fn field_placement_normalises_rotation_before_writing_and_readback() {
    let (_directory, path) = eeschema_fixture();
    let handler = served_handler().await;

    for (requested, expected) in [(450.0, 90.0), (-90.0, 270.0), (360.0, 0.0), (-0.0, 0.0)] {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "edit_schematic_component",
                    "arguments": {
                        "schematic": path,
                        "reference": "R1",
                        "field_placements": { "Value": { "rotation": requested } }
                    }
                }
            }))
            .await
            .expect("tools/call response");
        let result = response.result.expect("JSON-RPC result");
        assert_ne!(result["isError"], json!(true), "{result}");
        let parsed: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            parsed["units"][0]["field_placements"]["Value"]["rotation"],
            expected
        );
        // Read R1's Value `(at …)` itself: other R1 properties sit at the
        // same point with angle 0, so a substring search can't see a `-0`.
        let after = std::fs::read_to_string(&path).unwrap();
        let (start, end) = find_all_symbol_instance_blocks(&after, "R1")[0];
        let block = &after[start..end];
        let value = &block[block.find("(property \"Value\"").unwrap()..];
        let at = &value[value.find("(at ").unwrap()..];
        let at = &at[..=at.find(')').unwrap()];
        let expected_at = format!("(at 157.48 85.09 {})", cse::types::fmt_f64(expected));
        assert_eq!(at, expected_at, "requested {requested}");
    }
}

/// The batch path parses the candidate itself; an unparseable one is
/// `malformed_file`, and the batch aborts.
#[test]
fn batch_field_placement_planning_types_an_unparseable_schematic() {
    let mut content = "(kicad_sch (version 20250114)".to_string();
    let Err(failure) = plan_batch_field_placements(
        std::path::Path::new("broken.kicad_sch"),
        &mut content,
        "R1",
        &json!({ "field_placements": { "Value": { "x": 1.0 } } }),
    ) else {
        panic!("an unparseable schematic must not plan");
    };
    assert!(failure.abort_batch);
    assert_eq!(
        extract_error_kind(&failure.error).as_deref(),
        Some("malformed_file"),
        "{:?}",
        failure.error
    );
}

/// A property value placed before `(unit N)` may contain literal `(unit …)`
/// text. Unit selection must read the symbol's own `(unit N)` child rather
/// than scanning the raw block text.
#[tokio::test]
async fn field_placement_unit_selection_ignores_unit_text_inside_property_values() {
    let (_directory, path) = eeschema_fixture();
    let original = std::fs::read_to_string(&path).unwrap();
    let hostile = original.replacen(
            "\t\t(unit 2)",
            "\t\t(property \"Note\" \"see (unit bad) and (unit 1)\"\n\t\t\t(at 0 0 0)\n\t\t\t(effects (font (size 1.27 1.27)) (hide yes))\n\t\t)\n\t\t(unit 2)",
            1,
        );
    assert_ne!(hostile, original, "fixture must contain U1 unit 2");
    std::fs::write(&path, &hostile).unwrap();

    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "unit": 2,
                "field_placements": { "Value": { "x": 10.0, "y": 10.0 } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["changes"].as_array().unwrap().len(), 1);
    let units = result["units"].as_array().unwrap();
    let u1 = units.iter().find(|u| u["unit"] == 1).unwrap();
    let u2 = units.iter().find(|u| u["unit"] == 2).unwrap();
    assert_eq!(u2["field_placements"]["Value"]["x"], 10.0);
    assert_eq!(u2["field_placements"]["Value"]["y"], 10.0);
    assert_ne!(u1["field_placements"]["Value"]["x"], 10.0);
}

/// A symbol block whose `(unit …)` cannot be parsed (or is absent) must be
/// refused as `malformed_file` instead of defaulting to unit 1 and
/// mutating the wrong unit.
#[tokio::test]
async fn field_placement_refuses_malformed_unit_without_writing() {
    for (replacement, expected_fragment) in [
        ("\t\t(unit bad)", "bad"),
        ("\t\t(unit 1)\n\t\t(unit 2)", "more than one"),
        ("", "no (unit"),
    ] {
        let (_directory, path) = eeschema_fixture();
        let original = std::fs::read_to_string(&path).unwrap();
        let (start, end) = find_all_symbol_instance_blocks(&original, "R1")[0];
        let block = &original[start..end];
        let broken_block = block.replacen("\t\t(unit 1)", replacement, 1);
        assert_ne!(broken_block, block, "R1 must carry (unit 1)");
        let corrupted = apply_edits(original, vec![SexpEdit::replace(start, end, broken_block)]);
        std::fs::write(&path, &corrupted).unwrap();

        let result = handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R1",
                "unit": 1,
                "fields": { "Description": "must not land" },
                "field_placements": { "Value": { "x": 10.0, "y": 10.0 } }
            }),
            &context(),
        )
        .await
        .unwrap();

        assert!(result.is_error, "{result:?}");
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("malformed_file"),
            "{result:?}"
        );
        let message = format!("{result:?}");
        assert!(
            message.contains(expected_fragment),
            "refusal must name `{expected_fragment}`: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            corrupted,
            "refusal must leave the file byte-identical"
        );
    }

    // Non-positive and extra-token `(unit …)` forms are also refused by
    // `symbol_unit_from_block` itself.
    for bad_unit in ["(unit 0)", "(unit 1 2)"] {
        let block = format!("(symbol (lib_id \"Device:R\") {bad_unit})");
        let err = symbol_unit_from_block(&block, "R1").unwrap_err();
        assert!(
            matches!(err, PlacementRefusal::MalformedFile(ref msg) if msg.contains(bad_unit)),
            "expected MalformedFile naming `{bad_unit}`, got {err:?}"
        );
    }
}

/// A field's value is arbitrary text and may itself contain "(at " or
/// "(hide ". Searching the whole property block finds the token inside the
/// quoted value and rewrites *that* — the result is still valid
/// S-expression, so nothing downstream notices and the field's text is
/// silently changed. Found by adversarial review, not by the happy path.
#[tokio::test]
async fn field_placement_never_rewrites_a_value_that_looks_like_a_placement() {
    let (_directory, path) = eeschema_fixture();
    for (field, value) in [("Note", "mounted (at 45 deg)"), ("Trap", "see (hide yes)")] {
        let result = handle_edit_schematic_component(
            &json!({ "schematic": path, "reference": "P3", "fields": { field: value } }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
    }
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "P3",
            "field_placements": { "Note": { "x": 11.0, "y": 22.0 }, "Trap": { "hide": false } }
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains(r#""mounted (at 45 deg)""#),
        "the value must survive a move of its own placement"
    );
    assert!(
        after.contains(r#""see (hide yes)""#),
        "the value must survive a change to its own visibility"
    );
    assert!(
        after.contains("(at 11 22 0)"),
        "the placement must still move"
    );
}

/// Removing the hide token must not take whatever shares its line. Files
/// are not all written one node per line — this fixture is deliberately
/// not KiCad's own layout, because the hazard only exists in layouts KiCad
/// does not produce but is still asked to read.
#[tokio::test]
async fn field_placement_hide_removal_keeps_a_neighbour_on_the_same_line() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sameline.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n\t(uuid \"u\")\n\t(lib_symbols)\n\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at 10 10 0)\n\t\t(unit 1)\n\t\t(uuid \"s\")\n\t\t(property \"Reference\" \"R1\"\n\t\t\t(at 1 2 0) (hide yes)\n\t\t\t(effects (font (size 1 1)))\n\t\t)\n\t)\n)\n",
        )
        .unwrap();
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "R1",
            "field_placements": { "Reference": { "hide": false } }
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("(at 1 2 0)"),
        "the field's own position must survive: {after}"
    );
    assert!(
        !after.contains("(hide yes)"),
        "the flag must be gone: {after}"
    );
}

/// A field position is absolute, so one coordinate cannot be right for a
/// symbol placed as several units. Writing it to each would stack a quad
/// gate's four Values on one point and report success.
#[tokio::test]
async fn field_placement_refuses_a_position_across_units_without_one_named() {
    let (_directory, path) = fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "U1",
            "field_placements": { "Value": { "x": 10.0, "y": 10.0 } }
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Naming the unit is how the caller says which placement it means.
#[tokio::test]
async fn field_placement_targets_one_named_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "unit": 2,
                "field_placements": { "Value": { "x": 10.0, "y": 10.0 } }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .matches("(at 10 10 ")
            .count(),
        1,
        "only the named unit's field moves"
    );
}

/// The *file's* existing placement can be malformed, which is a different
/// door from a malformed request. Dropping the unparseable token and
/// defaulting the gap to zero shifts the rest left: `(at bad 85.09 90)`
/// reads as x=85.09, y=90, so a rotation-only edit would commit
/// `(at 85.09 90 0)` — moving the text to a position the caller never
/// asked for and the file never held. Refused before anything is written.
#[tokio::test]
async fn field_placement_refuses_a_malformed_existing_placement_without_writing() {
    let (_directory, path) = eeschema_fixture();
    let original = std::fs::read_to_string(&path).unwrap();
    let corrupted = original.replacen(
        "(property \"Value\" \"1.5K\"\n\t\t\t(at 157.48 85.09 90)",
        "(property \"Value\" \"1.5K\"\n\t\t\t(at bad 85.09 90)",
        1,
    );
    assert_ne!(
        corrupted, original,
        "the fixture must still carry R1's Value placement"
    );
    std::fs::write(&path, &corrupted).unwrap();

    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "R1",
            "field_placements": { "Value": { "rotation": 0.0 } }
        }),
        &context(),
    )
    .await
    .unwrap();

    assert!(result.is_error, "{result:?}");
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("malformed_file"),
        "{result:?}"
    );
    // Assert the reason, not merely that something failed: this handler
    // has several ways to error, and a test that accepts any of them
    // passes with the strict parse gone.
    let message = format!("{result:?}");
    assert!(
        message.contains("No fields were updated on 'R1': Value:")
            && message.contains("non-numeric x")
            && message.contains("bad"),
        "the refusal must preserve the message and name the malformed coordinate: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        corrupted,
        "a refusal must leave the file byte-identical"
    );
}

/// A malformed placement must abort the *whole* call, not just its own
/// field. This handler edits text and commits once at the end, so a valid
/// sibling edit in the same request would otherwise be committed while the
/// call reported a refusal. The placement-only test cannot see this: with
/// nothing else in the request there is no sibling to commit, so it passes
/// either way.
#[tokio::test]
async fn malformed_placement_aborts_a_combined_edit_without_writing() {
    let (_directory, path) = eeschema_fixture();
    let original = std::fs::read_to_string(&path).unwrap();
    let corrupted = original.replacen(
        "(property \"Value\" \"1.5K\"\n\t\t\t(at 157.48 85.09 90)",
        "(property \"Value\" \"1.5K\"\n\t\t\t(at bad 85.09 90)",
        1,
    );
    assert_ne!(
        corrupted, original,
        "the fixture must still carry R1's Value placement"
    );
    std::fs::write(&path, &corrupted).unwrap();

    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            // A perfectly good sibling edit, which must not be committed.
            "fields": { "Description": "sibling edit that must not land" },
            "reference": "R1",
            "field_placements": { "Value": { "rotation": 0.0 } }
        }),
        &context(),
    )
    .await
    .unwrap();

    assert!(result.is_error, "{result:?}");
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("malformed_file"),
        "{result:?}"
    );
    let message = format!("{result:?}");
    assert!(
        message.contains("non-numeric x") && message.contains("bad"),
        "the refusal must name the malformed coordinate: {message}"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after, corrupted,
        "a refusal must leave the file byte-identical"
    );
    assert!(
        !after.contains("sibling edit that must not land"),
        "the sibling field edit must not have been committed"
    );
}

/// `unit` is a u64 from JSON narrowed to u32. `as` truncates rather than
/// refusing, so 4294967297 becomes 1 and would silently move unit 1's
/// field text — a real unit the caller never named.
#[tokio::test]
async fn an_oversized_unit_is_refused_rather_than_wrapping_to_a_real_unit() {
    let (_directory, path) = fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "U1",
            "unit": 4_294_967_297u64,
            "field_placements": { "Value": { "x": 10.0, "y": 10.0 } }
        }),
        &context(),
    )
    .await
    .unwrap();

    assert!(result.is_error, "{result:?}");
    // Assert the reason: wrapping would produce a *successful* edit of
    // unit 1, so a test that accepted any error would pass with the
    // checked conversion removed only if it also checked the file.
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("invalid_argument"),
        "{result:?}"
    );
    let message = format!("{result:?}");
    assert!(
        message.contains("out of range"),
        "the refusal must say why: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "no unit's field text may move"
    );
}

/// A malformed entry is refused rather than partly applied — an `x` that
/// is not a number is a caller asking for a move.
#[tokio::test]
async fn field_placement_refuses_a_non_numeric_coordinate_without_writing() {
    let (_directory, path) = eeschema_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "P3",
            // `hide` is valid, so the entry is not empty: only the
            // coordinate check can refuse this one.
            "field_placements": { "Value": { "x": "over there", "hide": true } }
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    // Assert the reason, not merely that something failed: this handler
    // has several ways to error, and a test that accepts any of them
    // passes with the coordinate check gone.
    let message = format!("{result:?}");
    assert!(
        message.contains("Value.x") && message.contains("expected a number"),
        "the refusal must name the coordinate: {message}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn rotate_preserves_the_units_relative_orientation() {
    let (_directory, path) = fixture();
    let result = body(
        handle_rotate_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "rotation": 90.0
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["rotated_units"], 2);
    assert_eq!(result["rotation"], 90.0);
    assert_eq!(result["placements"], result["units"]);
    assert!(result["units"].as_array().unwrap().iter().any(|unit| {
        unit["uuid"] == "33333333-3333-4333-8333-333333333333" && unit["rotation"] == 270.0
    }));
    let mut placed = instances(&path);
    placed.sort_by_key(|instance| instance.unit);
    assert_eq!(placed[0].rotation, 90.0);
    assert_eq!(placed[1].rotation, 270.0);
}

/// The same real eeschema save as [`eeschema_fixture`], but written under
/// the file stem its own instance paths name (`ecc83-pp`). Placement
/// preflights the project name against those paths and rightly refuses a
/// mismatch, so a placement test needs the two to agree.
fn eeschema_placement_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ecc83-pp.kicad_sch");
    std::fs::write(
        &path,
        include_str!("../../../tests/fixtures/ecc83_multiunit.kicad_sch"),
    )
    .unwrap();
    (directory, path)
}

/// A misspelled axis is a caller asking for a reflection. Placing the
/// symbol unmirrored and reporting success is exactly the failure this
/// argument exists to end, so the placement refuses and writes nothing.
#[tokio::test]
async fn placing_with_an_unknown_mirror_axis_refuses_without_writing() {
    let (_directory, path) = fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path,
            "lib_id": "Device:R",
            "x": 100.0,
            "y": 40.0,
            "reference": "R8",
            "mirror": "Y"
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// `"none"` is the explicit way to say unmirrored. It must place the
/// symbol without a `(mirror ...)` token at all — eeschema has no
/// `(mirror none)` and would not load one. Placed into a real eeschema
/// save, which already carries mirrored symbols of its own, so the
/// assertion has to be scoped to the symbol this test placed.
#[tokio::test]
async fn placing_with_mirror_none_writes_no_token() {
    let (_directory, path) = eeschema_placement_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let mirrors_before = before.matches("(mirror ").count();
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path,
            "lib_id": "ecc83-pp:R",
            "x": 100.0,
            "y": 40.0,
            "reference": "R8",
            "mirror": "none"
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let committed = cse::Schematic::load(&path).unwrap();
    assert_eq!(
        committed
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some("R8"))
            .expect("placed symbol")
            .mirror,
        None,
        "\"none\" must leave the symbol without a mirror token"
    );
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .matches("(mirror ")
            .count(),
        mirrors_before,
        "no mirror token may be added, and none of the file's own may be lost"
    );
}

/// The placement argument, and the #101 half of it: a mirrored body puts
/// its Reference and Value where the reflection lands them. The site that
/// builds this transform hardcoded `mirror_x: false, mirror_y: false`, so
/// a mirrored symbol's fields sat on its unmirrored side.
#[tokio::test]
async fn placing_mirrored_reflects_the_body_and_its_field_anchors() {
    let (_directory, path) = eeschema_placement_fixture();
    for (reference, x, mirror) in [("R8", 100.0, None), ("R9", 140.0, Some("y"))] {
        let mut args = json!({
            "schematic": path,
            "lib_id": "ecc83-pp:R",
            "x": x,
            "y": 40.0,
            "reference": reference
        });
        if let Some(mirror) = mirror {
            args["mirror"] = json!(mirror);
        }
        let result = handle_add_schematic_component(&args, &context())
            .await
            .unwrap();
        assert!(!result.is_error, "placement failed for {reference}");
    }
    let committed = cse::Schematic::load(&path).unwrap();
    let mirror_of = |reference: &str| -> Option<String> {
        committed
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .expect("placed symbol")
            .mirror
            .clone()
    };
    assert_eq!(mirror_of("R8"), None);
    assert_eq!(mirror_of("R9"), Some("y".to_string()));
    let field_x = |reference: &str| -> f64 {
        let symbol = committed
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .expect("placed symbol");
        let property = symbol
            .properties
            .iter()
            .find(|property| property.name == "Reference")
            .expect("Reference property");
        property
            .sub_nodes
            .iter()
            .filter(|node| node.tag() == Some("at"))
            .find_map(cse::At::from_sexp)
            .expect("Reference carries an (at ...) node")
            .x
    };
    let origin_x = |reference: &str| -> f64 {
        committed
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .expect("placed symbol")
            .at
            .x
    };
    // Compare each anchor against its own origin, not against the other
    // placement: the requested coordinates snap to the 1.27mm grid, so
    // the two origins are not the 40mm apart they were asked for, and a
    // test comparing raw field coordinates passes whatever the transform
    // does.
    let plain = field_x("R8") - origin_x("R8");
    let mirrored = field_x("R9") - origin_x("R9");
    assert!(
        plain.abs() > 1e-6,
        "the unmirrored anchor must be off-origin for this comparison to mean anything"
    );
    assert!(
        (mirrored + plain).abs() < 1e-6,
        "(mirror y) must negate the Reference anchor's screen-X: \
             unmirrored {plain}, mirrored {mirrored}"
    );
}

/// The delta arithmetic can push a trailing unit past 360° — a unit at
/// 270° following a +90° turn computes 360°, which eeschema never writes
/// (it stores 0/90/180/270 and re-saves anything else). The stored and
/// reported angle must be the normalized one, or the response diverges
/// from the file the moment KiCad touches it.
#[tokio::test]
async fn rotation_past_a_full_turn_normalizes_instead_of_writing_360() {
    let (_directory, path) = fixture();
    for target in [90.0, 180.0] {
        body(
            handle_rotate_schematic_component(
                &json!({
                    "schematic": path,
                    "reference": "U1",
                    "rotation": target
                }),
                &context(),
            )
            .await
            .unwrap(),
        );
    }
    let mut placed = instances(&path);
    placed.sort_by_key(|instance| instance.unit);
    assert_eq!(placed[0].rotation, 180.0);
    assert_eq!(
        placed[1].rotation, 0.0,
        "270° + 90° must store 0°, not 360°"
    );
    let source = std::fs::read_to_string(&path).unwrap();
    assert!(
        !source.contains("(at 40 20 360)") && !source.contains(" 360)"),
        "no unnormalized angle may reach the file"
    );
}

#[tokio::test]
async fn edit_updates_shared_fields_on_every_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "value": "NEW",
                "fields": { "MPN": "A\\\"B" }
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["value"], "NEW");
    assert_eq!(result["fields"]["MPN"], "A\\\"B");
    assert!(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|unit| unit["fields"]["Value"] == "NEW" && unit["fields"]["MPN"] == "A\\\"B"));
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source.matches("(property \"Value\" \"NEW\"").count(), 2);
    assert_eq!(
        source.matches("(property \"MPN\" \"A\\\\\\\"B\"").count(),
        2
    );
}

/// #679's fixture, as eeschema 10.0.6 saved it: R1 and all three units of
/// U1 carry `LCSC` and `MPN`.
fn fields_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("component_fields.kicad_sch");
    std::fs::write(
        &path,
        include_str!("../../../tests/fixtures/component_fields_kicad10.kicad_sch"),
    )
    .unwrap();
    (directory, path)
}

#[tokio::test]
async fn remove_fields_deletes_a_field_from_every_unit() {
    let (_directory, path) = fields_fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({ "schematic": path, "reference": "U1", "remove_fields": ["MPN"] }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!(["MPN"]));
    assert_eq!(result["absent_fields"], json!([]));
    let units = result["units"].as_array().unwrap();
    assert_eq!(units.len(), 3);
    assert!(units
        .iter()
        .all(|unit| unit["fields"].get("MPN").is_none() && unit["fields"]["LCSC"] == "C679002"));

    // KiCad 10.0.6 resaves this output byte for byte (provenance in
    // component_fields_kicad10.removed.README.md): U1's three MPN blocks
    // go, R1's stays, and every other line is as eeschema wrote it.
    let resaved =
        include_str!("../../../tests/fixtures/component_fields_kicad10.removed.kicad_sch");
    assert_eq!(resaved.matches("(property \"MPN\"").count(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), resaved);
}

#[tokio::test]
async fn remove_fields_takes_a_field_from_the_units_that_carry_it() {
    // Only unit 1 of U1 carries Note.
    let (_directory, path) = fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({ "schematic": path, "reference": "U1", "remove_fields": ["Note"] }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!(["Note"]));
    assert_eq!(result["changes"], json!(["Note removed (1 unit(s))"]));
    let source = std::fs::read_to_string(&path).unwrap();
    assert!(!source.contains("(property \"Note\""), "{source}");
    assert_eq!(source.matches("(property \"Datasheet\"").count(), 2);
}

/// Two names are planned on the same file and deleted in one pass: both
/// go from every unit of U1, adjacent as they are, and R1 keeps its own.
#[tokio::test]
async fn two_fields_are_removed_in_one_call() {
    let (_directory, path) = fields_fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({ "schematic": path, "reference": "U1", "remove_fields": ["LCSC", "MPN"] }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!(["LCSC", "MPN"]));
    assert_eq!(result["absent_fields"], json!([]));
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source.matches("(property \"LCSC\"").count(), 1);
    assert_eq!(source.matches("(property \"MPN\"").count(), 1);
    assert!(
        source.contains("(property \"LCSC\" \"C679001\""),
        "{source}"
    );
    assert!(
        source.contains("(property \"MPN\" \"MPN-679-R1\""),
        "{source}"
    );
}

#[tokio::test]
async fn a_field_is_carried_to_a_new_name_in_one_call() {
    let (_directory, path) = fields_fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "fields": { "Part Number": "MPN-679-U1" },
                "remove_fields": ["MPN"]
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!(["MPN"]));
    assert!(result["units"].as_array().unwrap().iter().all(|unit| {
        unit["fields"]["Part Number"] == "MPN-679-U1" && unit["fields"].get("MPN").is_none()
    }));
}

/// A rename onto a designator another part carries: the removal counts
/// only the bound part's units, and the other part keeps its field.
#[tokio::test]
async fn removals_ahead_of_a_rename_onto_a_designator_in_use_touch_only_the_bound_part() {
    let (_directory, path) = fields_fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "R1",
                "new_reference": "U1",
                "remove_fields": ["MPN"]
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!(["MPN"]));
    assert!(
        result["changes"]
            .as_array()
            .unwrap()
            .contains(&json!("MPN removed (1 unit(s))")),
        "{result}"
    );
    let source = std::fs::read_to_string(&path).unwrap();
    assert!(!source.contains("MPN-679-R1"), "{source}");
    assert_eq!(
        source.matches("(property \"MPN\" \"MPN-679-U1\"").count(),
        3
    );
}

/// The fields fixture with R1's `MPN` written as KiCad writes a private
/// field.
fn private_mpn_fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let (directory, path) = fields_fixture();
    let private = std::fs::read_to_string(&path).unwrap().replacen(
        "(property \"MPN\" \"MPN-679-R1\"",
        "(property private \"MPN\" \"MPN-679-R1\"",
        1,
    );
    std::fs::write(&path, &private).unwrap();
    (directory, path, private)
}

/// The structured refusal a private field draws, by field, name and part.
fn assert_private_refusal(result: &CallToolResult) {
    assert!(result.is_error, "{result:?}");
    assert_eq!(
        extract_error_kind(result).as_deref(),
        Some("invalid_argument")
    );
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text result");
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["error"]["field"], "remove_fields", "{body}");
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("'MPN' is a private field on 'R1'"),
        "{body}"
    );
}

#[tokio::test]
async fn a_private_field_is_refused_by_name() {
    let (_directory, path, private) = private_mpn_fixture();
    let result = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "R1", "remove_fields": ["MPN"] }),
        &context(),
    )
    .await
    .unwrap();
    assert_private_refusal(&result);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), private);
}

/// A private name refuses the whole call before its first edit: neither
/// the other removal nor the `fields` edit beside it is written.
#[tokio::test]
async fn a_private_field_refuses_the_whole_call_before_any_edit() {
    let (_directory, path, private) = private_mpn_fixture();
    let result = handle_edit_schematic_component(
        &json!({
            "schematic": path,
            "reference": "R1",
            "fields": { "Tolerance": "1%" },
            "remove_fields": ["LCSC", "MPN"]
        }),
        &context(),
    )
    .await
    .unwrap();
    assert_private_refusal(&result);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), private);
}

#[tokio::test]
async fn a_private_field_value_is_updated_in_place_and_stays_private() {
    let (_directory, path, private) = private_mpn_fixture();
    let spaced = private
        .replacen(
            r#"(property "Reference" "R1""#,
            "(property \"Reference\"\n\t\t\t\"R1\"",
            1,
        )
        .replacen(
            r#"property private "MPN""#,
            "property private\t\n\t\t\t\"MPN\"",
            1,
        );
    assert_ne!(spaced, private);
    std::fs::write(&path, &spaced).unwrap();
    let result = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "R1", "fields": { "MPN": "NEW \"PART\"" } }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let expected = spaced.replacen("\"MPN-679-R1\"", r#""NEW \"PART\"""#, 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
}

#[tokio::test]
async fn a_name_both_public_and_private_on_one_unit_is_refused() {
    let (_directory, path, private) = private_mpn_fixture();
    let doubled = private.replacen(
        "(property private \"MPN\" \"MPN-679-R1\"",
        "(property \"MPN\" \"PUBLIC\" (at 0 0 0))\n\t\t(property private \"MPN\" \"MPN-679-R1\"",
        1,
    );
    std::fs::write(&path, &doubled).unwrap();
    let (refused, body) = call_served_edit(
        &served_edit_handler().await,
        json!({ "schematic": path, "reference": "R1", "fields": { "MPN": "NEW-PART" } }),
    )
    .await;
    assert!(refused, "{body}");
    assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), doubled);
}

fn multi_unit_private_mpn_source(source: &str, keep_public_copy: bool) -> String {
    let public = r#"(property "MPN" "MPN-679-U1""#;
    let mut copies = source.match_indices(public);
    let (first, _) = copies.next().unwrap();
    let (second, _) = copies.next().unwrap();
    let (third, _) = copies.next().unwrap();
    assert!(copies.next().is_none());
    let remove = if keep_public_copy {
        &[third][..]
    } else {
        &[second, third][..]
    };
    let mut edits = vec![SexpEdit::insert(first + "(property ".len(), "private ")];
    for &start in remove {
        let (start, end) = find_block_with_leading_whitespace(source, start).unwrap();
        edits.push(SexpEdit::delete(start, end));
    }
    apply_edits(source.to_string(), edits)
}

#[tokio::test]
async fn missing_unit_copies_of_a_private_field_are_added_as_private() {
    let (_directory, path) = fields_fixture();
    let before = multi_unit_private_mpn_source(&std::fs::read_to_string(&path).unwrap(), false);
    let (start, end) = find_all_symbol_instance_blocks(&before, "U1")
        .into_iter()
        .find(|&(start, end)| !before[start..end].contains(r#"property private "MPN""#))
        .unwrap();
    let block = &before[start..end];
    let node = parse_sexp(block).unwrap();
    let uuid = node.find_str("uuid").unwrap().to_owned();
    let (x, y, _) = parse_at(&node).unwrap();
    let spaced_block = block.replacen("(at ", "(at\t", 1);
    assert_ne!(spaced_block, block);
    let before = apply_edits(before, vec![SexpEdit::replace(start, end, spaced_block)]);
    std::fs::write(&path, &before).unwrap();
    let result = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "U1", "fields": { "MPN": "NEW-PART" } }),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let saved = cse::Schematic::load(&path).unwrap();
    let units: Vec<_> = saved
        .symbols
        .iter()
        .filter(|unit| unit.reference() == Some("U1"))
        .collect();
    assert_eq!(units.len(), 3);
    for unit in units {
        let mut mpns = unit.properties.iter().filter(|field| field.name == "MPN");
        let mpn = mpns.next().unwrap();
        assert!(mpn.private);
        assert_eq!(mpn.value, "NEW-PART");
        assert!(mpns.next().is_none());
    }
    let tree = parse_sexp(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let unit = tree
        .find_all("symbol")
        .into_iter()
        .find(|node| node.find_str("uuid") == Some(uuid.as_str()))
        .unwrap();
    let mpn = unit
        .find_all("property")
        .into_iter()
        .find(|node| node.get(2).and_then(|name| name.as_str()) == Some("MPN"))
        .unwrap();
    assert_eq!(parse_at(mpn), Some((x, y, 0.0)));
}

#[tokio::test]
async fn a_missing_unit_copy_with_mixed_privacy_is_refused_without_writing() {
    let (_directory, path) = fields_fixture();
    let before = multi_unit_private_mpn_source(&std::fs::read_to_string(&path).unwrap(), true);
    std::fs::write(&path, &before).unwrap();
    let result = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "U1", "fields": { "MPN": "NEW-PART" } }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{result:?}");
    assert!(
        matches!(&result.content[..], [ToolContent::Text { text }]
                if text.contains("private and public copies of 'MPN'")),
        "{result:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn a_name_is_matched_whole_not_as_a_prefix() {
    let (_directory, path) = fields_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = body(
        handle_edit_schematic_component(
            &json!({ "schematic": path, "reference": "U1", "remove_fields": ["MP"] }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["absent_fields"], json!(["MP"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn removing_an_absent_field_reports_it_and_writes_nothing() {
    let (_directory, path) = fields_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "remove_fields": ["Tolerance", "Tolerance"]
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["removed_fields"], json!([]));
    assert_eq!(result["absent_fields"], json!(["Tolerance"]));
    assert_eq!(result["changes"], json!([]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn remove_fields_refusals_leave_the_file_unchanged() {
    for (extra, reason) in [
        (json!({ "remove_fields": ["Value"] }), "mandatory"),
        (json!({ "remove_fields": ["description"] }), "mandatory"),
        (
            json!({ "remove_fields": ["LCSC", "MPN"], "fields": { "MPN": "X" } }),
            "also named in fields",
        ),
        (
            json!({ "remove_fields": ["MPN"], "field_placements": { "MPN": { "hide": false } } }),
            "also named in field_placements",
        ),
        (json!({ "remove_fields": "MPN" }), "array"),
        (json!({ "remove_fields": null }), "array"),
        (json!({ "remove_fields": [""] }), "non-empty"),
        (json!({ "remove_fields": [7] }), "non-empty"),
    ] {
        let (_directory, path) = fields_fixture();
        let before = std::fs::read_to_string(&path).unwrap();
        let mut args = extra.clone();
        args["schematic"] = json!(path);
        args["reference"] = json!("U1");
        let result = handle_edit_schematic_component(&args, &context())
            .await
            .unwrap();
        assert!(result.is_error, "{extra} must be refused");
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("invalid_argument"),
            "{extra}"
        );
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        assert!(text.contains(reason), "{extra}: {text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{extra}");
    }
}

async fn served_edit_handler() -> crate::mcp::handler::McpHandler {
    crate::mcp::handler::McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: false,
    })
    .await
    .unwrap()
}

async fn call_served_edit(
    handler: &crate::mcp::handler::McpHandler,
    arguments: serde_json::Value,
) -> (bool, serde_json::Value) {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "edit_schematic_component", "arguments": arguments}});
    let result = handler
        .handle_message(message)
        .await
        .unwrap()
        .result
        .unwrap();
    let body = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    (result["isError"] == true, body)
}

#[tokio::test]
async fn served_remove_fields_removes_refuses_and_stays_out_of_other_replies() {
    let (_directory, path) = fields_fixture();
    let schematic = path.display().to_string();
    let handler = served_edit_handler().await;
    let call = |arguments| call_served_edit(&handler, arguments);
    let before = std::fs::read_to_string(&path).unwrap();
    for (remove_fields, field) in [
        (json!(null), "remove_fields"),
        (json!("MPN"), "remove_fields"),
        (json!([7]), "remove_fields[0]"),
        (json!(["Description"]), "remove_fields"),
    ] {
        let (refused, body) = call(json!({
            "schematic": schematic,
            "reference": "U1",
            "remove_fields": remove_fields
        }))
        .await;
        assert!(refused, "{remove_fields} must be refused: {body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "{remove_fields}"
        );
    }

    let (refused, removed) = call(json!({
        "schematic": schematic,
        "reference": "U1",
        "remove_fields": ["MPN"]
    }))
    .await;
    assert!(!refused, "{removed}");
    assert_eq!(removed["removed_fields"], json!(["MPN"]));
    assert_eq!(removed["absent_fields"], json!([]));
    let units = removed["units"].as_array().unwrap();
    assert_eq!(units.len(), 3);
    assert!(
        units
            .iter()
            .all(|unit| unit["fields"].get("MPN").is_none() && unit["fields"]["LCSC"] == "C679002"),
        "{removed}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        include_str!("../../../tests/fixtures/component_fields_kicad10.removed.kicad_sch")
    );

    let (refused, edited) = call(json!({
        "schematic": schematic,
        "reference": "U1",
        "fields": { "LCSC": "C679003" }
    }))
    .await;
    assert!(!refused, "{edited}");
    assert!(
        edited.get("removed_fields").is_none() && edited.get("absent_fields").is_none(),
        "{edited}"
    );
}

#[tokio::test]
async fn a_field_added_then_removed_leaves_the_eeschema_file_as_it_was() {
    let (_directory, path) = eeschema_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    for args in [
        json!({ "fields": { "Tolerance": "1%" } }),
        json!({ "remove_fields": ["Tolerance"] }),
    ] {
        let mut args = args;
        args["schematic"] = json!(path);
        args["reference"] = json!("U1");
        body(
            handle_edit_schematic_component(&args, &context())
                .await
                .unwrap(),
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test(flavor = "current_thread")]
async fn a_removal_missing_from_the_prospective_file_is_refused_without_writing() {
    let (_directory, path) = fields_fixture();
    let before = std::fs::read_to_string(&path).unwrap();
    // The writer's result keeps one unit's MPN.
    COMPONENT_PROSPECTIVE_FAULT.with(|fault| {
        *fault.borrow_mut() = Some((
            "(property \"LCSC\" \"C679002\"".to_owned(),
            "(property \"MPN\" \"MPN-679-U1\")\n\t\t(property \"LCSC\" \"C679002\"".to_owned(),
        ));
    });
    let result = handle_edit_schematic_component(
        &json!({ "schematic": path, "reference": "U1", "remove_fields": ["MPN"] }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text result");
    };
    assert!(text.contains("MPN is still present"), "{text}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn rename_updates_rendered_and_netlist_references_on_every_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_edit_schematic_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "new_reference": "U9"
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["reference"], "U9");
    assert_eq!(result["requested_reference"], "U1");
    assert!(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|unit| unit["fields"]["Reference"] == "U9"
            && unit["instance_references"] == json!(["U9"])));
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source.matches("(property \"Reference\" \"U9\"").count(), 2);
    assert_eq!(source.matches("(reference \"U9\")").count(), 2);
    assert_eq!(instances(&path).len(), 2);
}

#[tokio::test]
async fn annotation_repairs_a_field_missing_from_one_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_add_component_annotation(
            &json!({
                "schematic": path,
                "reference": "U1",
                "key": "Note",
                "value": "NEW"
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["updated_units"], 1);
    assert_eq!(result["added_units"], 1);
    assert!(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|unit| unit["fields"]["Note"] == "NEW"));
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source.matches("(property \"Note\" \"NEW\"").count(), 2);
}

#[tokio::test]
async fn grouping_adds_one_property_to_every_unit() {
    let (_directory, path) = fixture();
    let result = body(
        handle_group_components(
            &json!({
                "schematic": path,
                "group_name": "Logic",
                "references": ["U1"]
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["grouped"], json!(["U1"]));
    assert_eq!(result["schematic"], result["components"][0]["schematic"]);
    assert_eq!(
        result["components"][0]["schematic"],
        path.display().to_string()
    );
    assert!(result["components"][0]["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|unit| unit["fields"]["Group"] == "Logic"));
    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source.matches("(property \"Group\" \"Logic\"").count(), 2);
}

#[tokio::test]
async fn pin_locations_include_every_units_real_placement() {
    let (_directory, path) = fixture();
    let result = body(
        handle_get_schematic_pin_locations(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["unit_count"], 2);
    assert_eq!(result["pins"].as_array().unwrap().len(), 2);
    assert!(result["pins"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pin| pin["number"] == "1" && pin["unit"] == 1 && pin["y"] == 100.0));
    assert!(result["pins"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pin| pin["number"] == "2" && pin["unit"] == 2 && pin["y"] == 120.0));
}

#[tokio::test]
async fn component_summary_lists_every_placement() {
    let (_directory, path) = fixture();
    let result = body(
        handle_get_schematic_component(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["unit_count"], 2);
    assert_eq!(
        result["unit_count"].as_u64().unwrap() as usize,
        result["units"].as_array().unwrap().len()
    );
    assert!(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .any(|unit| unit["unit"] == 2 && unit["y"] == 120.0));
}

#[tokio::test]
async fn get_reads_the_lowest_unit_and_list_reads_each_unit() {
    // `Note` is on the unit saved first: the lowest as written, and not
    // once it is renumbered 3.
    let renumbered = SCHEMATIC.replace("(unit 1)", "(unit 3)");
    assert_ne!(renumbered, SCHEMATIC);
    for (source, lowest_has_note) in [(SCHEMATIC.to_owned(), true), (renumbered, false)] {
        let (_directory, path) = fixture();
        std::fs::write(&path, source).unwrap();
        let has_note = |value: &serde_json::Value| value["properties"].get("Note").is_some();

        let component = body(
            handle_get_schematic_component(
                &json!({ "schematic": path, "reference": "U1" }),
                &context(),
            )
            .await
            .unwrap(),
        );
        assert_eq!(has_note(&component), lowest_has_note, "{component}");

        let listed = body(
            handle_list_schematic_components(&json!({ "schematic": path }), &context())
                .await
                .unwrap(),
        );
        for row in listed["components"].as_array().unwrap() {
            assert_eq!(has_note(row), row["y"] == 100.0, "{listed}");
        }
    }
}

#[tokio::test]
async fn a_repeated_property_reads_its_first_value() {
    let property = "    (property \"Value\" \"OLD\" (at 100 102 0))\n";
    let repeated = property.replace("OLD", "NEW");
    let source = SCHEMATIC.replacen(property, &format!("{property}{repeated}"), 1);
    assert_ne!(source, SCHEMATIC);
    let (_directory, path) = fixture();
    std::fs::write(&path, source).unwrap();

    let component = body(
        handle_get_schematic_component(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(component["properties"]["Value"], "OLD", "{component}");
    assert_eq!(component["value"], "OLD", "{component}");
    let listed = body(
        handle_list_schematic_components(
            &json!({ "schematic": path, "verbose": true }),
            &context(),
        )
        .await
        .unwrap(),
    );
    for row in listed["components"].as_array().unwrap() {
        assert_eq!(row["properties"]["Value"], row["value"], "{listed}");
    }
}

#[tokio::test]
async fn replace_changes_every_unit_and_preserves_unit_numbers() {
    let (_directory, path) = fixture();
    let result = body(
        handle_replace_component(
            &json!({
                "schematic": path,
                "reference": "U1",
                "new_lib_id": "Test:DUAL_NEW"
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["units_replaced"], 2);
    let mut placed = instances(&path);
    placed.sort_by_key(|instance| instance.unit);
    assert_eq!(
        placed
            .iter()
            .map(|instance| instance.unit)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(placed
        .iter()
        .all(|instance| instance.lib_id == "Test:DUAL_NEW"));
}

#[tokio::test]
async fn replace_rejects_an_ambiguous_unit_override_without_writing() {
    let (_directory, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    let result = handle_replace_component(
        &json!({
            "schematic": path,
            "reference": "U1",
            "new_lib_id": "Test:DUAL_NEW",
            "unit": 1
        }),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// Parts replaced in turn by one symbol share one embedded copy of it:
/// the copy the first replace writes, its name on the line after
/// `(symbol`, and the copy eeschema wrote, its name on the same line.
#[tokio::test]
async fn replace_by_one_symbol_keeps_one_embedded_copy() {
    const SHEET: &str = include_str!("../../../tests/fixtures/trace_point_kicad10.kicad_sch");
    for new_lib_id in ["Fields:FIELDS_BARE", "Device:R"] {
        let directory = tempfile::tempdir().unwrap();
        write_fixture_lib_table(directory.path());
        let path = directory.path().join("trace.kicad_sch");
        std::fs::write(&path, SHEET).unwrap();
        for reference in ["R1", "R2"] {
            let result = handle_replace_component(
                &json!({ "schematic": path, "reference": reference, "new_lib_id": new_lib_id }),
                &context(),
            )
            .await
            .unwrap();
            assert!(!result.is_error, "{new_lib_id} {reference}: {result:?}");
            let written = std::fs::read_to_string(&path).unwrap();
            if new_lib_id == "Fields:FIELDS_BARE" && reference == "R1" {
                assert!(
                    !written.contains("(symbol \"Fields:FIELDS_BARE\""),
                    "premise: the first replace writes the name on its own line"
                );
            }
            let tree = parse_sexp(&written).unwrap();
            let copies = tree.find("lib_symbols").map_or(0, |libraries| {
                libraries
                    .find_all("symbol")
                    .into_iter()
                    .filter(|symbol| {
                        symbol.get(1).and_then(|value| value.as_str()) == Some(new_lib_id)
                    })
                    .count()
            });
            assert_eq!(copies, 1, "{new_lib_id} after replacing {reference}");
        }
    }
}

#[tokio::test]
async fn move_region_moves_the_selected_unit_not_unit_one() {
    let (_directory, path) = fixture();
    let result = body(
        handle_move_region(
            &json!({
                "schematic": path,
                "x1": 95.0,
                "y1": 115.0,
                "x2": 105.0,
                "y2": 125.0,
                "dx": 10.0,
                "dy": 0.0
            }),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result["moved_unit_count"], 1);
    assert_eq!(result["placements"][0]["unit"], 2);
    let mut placed = instances(&path);
    placed.sort_by_key(|instance| instance.unit);
    assert_eq!(placed[0].x, 100.0, "unit 1 must stay put");
    assert_ne!(placed[1].x, 100.0, "selected unit 2 must move");
}

#[test]
fn shared_readback_refuses_missing_or_renamed_bound_identity() {
    let (_directory, path) = fixture();
    let committed = cse::Schematic::load(&path).unwrap();
    let target = component_target_from_source(&path, SCHEMATIC, "U1").unwrap();

    let wrong_reference = component_mutation_readback_from_schematic(
        &path,
        &committed,
        &target.uuids(),
        Some("U404"),
    )
    .unwrap_err();
    assert_eq!(
        extract_error_kind(&wrong_reference).as_deref(),
        Some("stale_target")
    );

    let missing_uuid = component_mutation_readback_from_schematic(
        &path,
        &committed,
        &["missing-uuid".to_owned()],
        None,
    )
    .unwrap_err();
    assert_eq!(
        extract_error_kind(&missing_uuid).as_deref(),
        Some("stale_target")
    );
}

#[test]
fn shared_readback_refuses_wrong_document_and_mismatched_fields() {
    let (directory, path) = fixture();
    let other = directory.path().join("other.kicad_sch");
    std::fs::write(&other, SCHEMATIC).unwrap();
    let committed = cse::Schematic::load(&path).unwrap();
    let committed_other = cse::Schematic::load(&other).unwrap();
    let target = component_target_from_source(&path, SCHEMATIC, "U1").unwrap();

    let wrong_document = component_mutation_readback_from_schematic(
        &path,
        &committed_other,
        &target.uuids(),
        Some("U1"),
    )
    .unwrap_err();
    assert_eq!(
        extract_error_kind(&wrong_document).as_deref(),
        Some("stale_target")
    );

    let mismatched_field = verified_component_readback(
        &path,
        &committed,
        &target.with_fields(&BTreeMap::from([(
            "Value".to_owned(),
            "NOT-COMMITTED".to_owned(),
        )])),
    )
    .unwrap_err();
    assert_eq!(
        extract_error_kind(&mismatched_field).as_deref(),
        Some("stale_target")
    );
}

#[test]
fn shared_readback_reports_the_committed_models_document_path() {
    let (_directory, path) = fixture();
    let committed = cse::Schematic::load(&path).unwrap();
    let target = component_target_from_source(&path, SCHEMATIC, "U1").unwrap();
    let differently_spelled = path
        .parent()
        .unwrap()
        .join(".")
        .join(path.file_name().unwrap());
    assert_ne!(
        differently_spelled.display().to_string(),
        committed.filepath().display().to_string()
    );

    let observed = component_mutation_readback_from_schematic(
        &differently_spelled,
        &committed,
        &target.uuids(),
        Some("U1"),
    )
    .unwrap();

    assert_eq!(
        observed["schematic"],
        committed.filepath().display().to_string()
    );
    assert_ne!(
        observed["schematic"],
        differently_spelled.display().to_string()
    );
}

#[test]
fn duplicate_bound_uuid_and_property_identities_are_ambiguous() {
    let (_directory, path) = fixture();
    let committed = cse::Schematic::load(&path).unwrap();
    let uuid = "22222222-2222-4222-8222-222222222222".to_owned();
    let duplicate_bound_uuid = component_mutation_readback_from_schematic(
        &path,
        &committed,
        &[uuid.clone(), uuid.clone()],
        Some("U1"),
    )
    .unwrap_err();
    assert_eq!(
        extract_error_kind(&duplicate_bound_uuid).as_deref(),
        Some("ambiguous_target")
    );

    let property = "    (property \"Note\" \"OLD\" (at 100 100 0) (hide yes))\n";
    let duplicate_source = SCHEMATIC.replacen(property, &format!("{property}{property}"), 1);
    assert_ne!(duplicate_source, SCHEMATIC);
    std::fs::write(&path, duplicate_source).unwrap();
    let committed = cse::Schematic::load(&path).unwrap();
    let duplicate_property =
        component_mutation_readback_from_schematic(&path, &committed, &[uuid], Some("U1"))
            .unwrap_err();
    assert_eq!(
        extract_error_kind(&duplicate_property).as_deref(),
        Some("ambiguous_target")
    );
}

#[tokio::test]
async fn duplicate_reference_unit_is_ambiguous_before_a_move() {
    let original = SCHEMATIC.replacen("    (unit 2)\n", "    (unit 1)\n", 1);
    assert_ne!(original, SCHEMATIC);
    let (directory, path) = fixture();
    std::fs::write(&path, &original).unwrap();
    let result = handle_move_schematic_component(
        &json!({ "schematic": path, "reference": "U1", "x": 110.0, "y": 110.0 }),
        &context(),
    )
    .await
    .unwrap();

    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("ambiguous_target")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    drop(directory);
}

#[tokio::test]
async fn missing_reference_is_stale_and_does_not_mutate_either_document() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.kicad_sch");
    let other = directory.path().join("other.kicad_sch");
    std::fs::write(&target, SCHEMATIC).unwrap();
    std::fs::write(&other, SCHEMATIC).unwrap();
    let result = handle_rotate_schematic_component(
        &json!({ "schematic": target, "reference": "MISSING", "rotation": 90.0 }),
        &context(),
    )
    .await
    .unwrap();

    assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), SCHEMATIC);
    assert_eq!(std::fs::read_to_string(&other).unwrap(), SCHEMATIC);
}

#[tokio::test]
async fn successful_move_names_only_the_requested_document_from_readback() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.kicad_sch");
    let other = directory.path().join("other.kicad_sch");
    std::fs::write(&target, SCHEMATIC).unwrap();
    std::fs::write(&other, SCHEMATIC).unwrap();
    let result = body(
        handle_move_schematic_component(
            &json!({ "schematic": target, "reference": "U1", "x": 110.0, "y": 110.0 }),
            &context(),
        )
        .await
        .unwrap(),
    );

    assert_eq!(result["schematic"], target.display().to_string());
    assert_ne!(std::fs::read_to_string(&target).unwrap(), SCHEMATIC);
    assert_eq!(std::fs::read_to_string(&other).unwrap(), SCHEMATIC);
}

#[test]
fn stale_target_revision_refuses_a_prepared_component_edit() {
    let (_directory, path) = fixture();
    let target = component_target_from_source(&path, SCHEMATIC, "U1").unwrap();
    let candidate = set_property_value(SCHEMATIC, "U1", "Value", "PLANNED", false)
        .unwrap()
        .0;
    let command = SchematicCommand::replace_items_from_document(
        SCHEMATIC,
        &candidate,
        target.item_ids().unwrap(),
        "planned edit",
    )
    .unwrap();
    let newer = SCHEMATIC.replacen(
        "(property \"Value\" \"OLD\"",
        "(property \"Value\" \"NEWER\"",
        1,
    );
    std::fs::write(&path, &newer).unwrap();

    assert!(matches!(
        commit_command(&path, &command).unwrap_err(),
        SexpError::Conflict { .. } | SexpError::ItemConflict { .. }
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), newer);
}

#[tokio::test(flavor = "current_thread")]
async fn component_mutation_handlers_refuse_bad_prospective_results_without_writing() {
    for (operation, written, intended) in [
        (
            "edit_schematic_component",
            "WRITER-VALUE",
            "REQUESTED-VALUE",
        ),
        ("add_component_annotation", "WRITER-NOTE", "REQUESTED-NOTE"),
        ("group_components", "WRITER-GROUP", "REQUESTED-GROUP"),
    ] {
        let (_directory, path) = eeschema_fixture();
        let before = std::fs::read_to_string(&path).unwrap();
        COMPONENT_PROSPECTIVE_FAULT.with(|fault| {
            *fault.borrow_mut() = Some((intended.to_owned(), written.to_owned()));
        });
        let refusal = match operation {
            "edit_schematic_component" => {
                handle_edit_schematic_component(
                    &json!({
                        "schematic": path.display().to_string(),
                        "reference": "U1",
                        "value": intended,
                    }),
                    &context(),
                )
                .await
            }
            "add_component_annotation" => {
                handle_add_component_annotation(
                    &json!({
                        "schematic": path.display().to_string(),
                        "reference": "U1",
                        "key": "Note",
                        "value": intended,
                    }),
                    &context(),
                )
                .await
            }
            "group_components" => {
                handle_group_components(
                    &json!({
                        "schematic": path.display().to_string(),
                        "references": ["U1"],
                        "group_name": intended,
                    }),
                    &context(),
                )
                .await
            }
            _ => unreachable!(),
        }
        .unwrap();

        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("stale_target"),
            "{operation} must refuse the prospective mismatch"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "{operation} must not commit before its intent is proven"
        );
    }
}

#[tokio::test]
async fn component_mutation_handlers_do_not_create_an_absent_schematic() {
    for operation in [
        "edit_schematic_component",
        "add_component_annotation",
        "group_components",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent.kicad_sch");
        let outcome = match operation {
            "edit_schematic_component" => {
                handle_edit_schematic_component(
                    &json!({
                        "schematic": path.display().to_string(),
                        "reference": "U1",
                        "value": "X",
                    }),
                    &context(),
                )
                .await
            }
            "add_component_annotation" => {
                handle_add_component_annotation(
                    &json!({
                        "schematic": path.display().to_string(),
                        "reference": "U1",
                        "key": "Note",
                        "value": "X",
                    }),
                    &context(),
                )
                .await
            }
            "group_components" => {
                handle_group_components(
                    &json!({
                        "schematic": path.display().to_string(),
                        "references": ["U1"],
                        "group_name": "X",
                    }),
                    &context(),
                )
                .await
            }
            _ => unreachable!(),
        };
        assert!(outcome.is_err(), "{operation} must refuse an absent target");
        assert!(!path.exists(), "{operation} must not create its target");
    }
}

#[test]
fn a_post_commit_verification_failure_says_the_file_may_have_changed() {
    let (_directory, path) = fixture();
    let error = crate::tools::mutation_outcome_uncertain(
        &path,
        "edit_schematic_component",
        "observed Value differs from bound intent",
    );
    assert_eq!(
        extract_error_kind(&error).as_deref(),
        Some("mutation_outcome_uncertain")
    );
    let ToolContent::Text { text } = &error.content[0] else {
        panic!("expected text result");
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["error"]["path"], path.display().to_string());
    assert_eq!(body["error"]["operation"], "edit_schematic_component");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("may have changed"));
}
