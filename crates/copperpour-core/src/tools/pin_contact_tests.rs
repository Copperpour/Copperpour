use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

const SCH: &str = include_str!("../../tests/fixtures/pin_contact_kicad10/pin_contact.kicad_sch");
const TOOL: &str = "find_pin_contacts";

struct Sheet {
    _dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
}

impl Sheet {
    async fn new(source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pin_contact.kicad_sch");
        std::fs::write(&path, source).unwrap();
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
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

    async fn call(&self, tool: &str, mut args: Value) -> crate::mcp::protocol::CallToolResult {
        args["schematic"] = json!(self.path);
        served_call(&self.handler, tool, args).await
    }

    async fn ok(&self, tool: &str, args: Value) -> Value {
        let result = self.call(tool, args).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }

    async fn report(&self) -> Value {
        self.ok(TOOL, json!({})).await
    }
}

fn pins(pins: &Value) -> Vec<String> {
    let mut names: Vec<_> = pins
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            format!(
                "{}.{}",
                p["reference"].as_str().unwrap(),
                p["pin_number"].as_str().unwrap()
            )
        })
        .collect();
    names.sort_unstable();
    names
}

fn contact<'a>(report: &'a Value, expected: &[&str]) -> &'a Value {
    report["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|contact| pins(&contact["pins"]) == expected)
        .unwrap_or_else(|| panic!("no contact {expected:?}: {report}"))
}

fn wire(a: (f64, f64), b: (f64, f64), suffix: u64) -> String {
    format!(
        "\n(wire (pts (xy {} {}) (xy {} {})) (stroke (width 0) (type default)) \
         (uuid \"00000000-0000-4000-8000-{suffix:012}\"))",
        a.0, a.1, b.0, b.1
    )
}

fn with_items(source: &str, items: &str) -> String {
    format!(
        "{source}{items}\n)\n",
        source = source.trim_end().strip_suffix(')').unwrap()
    )
}

fn ground_at(x: f64, y: f64, number: u32) -> String {
    format!(
        "\n(symbol (lib_id \"power:GND\") (at {x} {y} 0) (unit 1) \
         (property \"Reference\" \"#PWR{number}\") \
         (uuid \"00000000-0000-4000-8000-{number:012}\"))"
    )
}

#[tokio::test]
async fn contact_groups_keep_local_maximal_neighborhoods_without_chaining() {
    let source = with_items(
        SCH,
        &format!(
            "{}{}",
            ground_at(55.886, 50.8, 101),
            ground_at(55.892, 50.8, 102)
        ),
    );
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["contact_count"], 3, "{report}");
    let center = contact(&report, &["#PWR101.1", "#PWR102.1", "J1.1", "R1.1"]);
    assert_eq!(center["point"], json!({"x": 55.886, "y": 50.8}));

    let source = with_items(&source, &ground_at(55.898, 50.8, 103));
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["contact_count"], 4, "{report}");
    let next = contact(&report, &["#PWR101.1", "#PWR102.1", "#PWR103.1"]);
    assert_eq!(next["point"], json!({"x": 55.892, "y": 50.8}));
    contact(&report, &["#PWR101.1", "#PWR102.1", "J1.1", "R1.1"]);
}

#[tokio::test]
async fn a_hash_reference_does_not_make_a_resistor_a_power_symbol() {
    let source = SCH.replace(
        "(property \"Reference\" \"R1\"",
        "(property \"Reference\" \"#R1\"",
    );
    let report = Sheet::new(&source).await.report().await;
    let finding = contact(&report, &["#R1.1", "J1.1"]);
    for pin in finding["pins"].as_array().unwrap() {
        assert_eq!(pin["is_power_symbol"], false, "{pin}");
    }
}

#[tokio::test]
async fn pin_contacts_are_separate_from_overlaps_and_connectivity() {
    let sheet = Sheet::new(SCH).await;
    let report = sheet.report().await;
    assert_eq!(report["contact_count"], 3);
    let connector = contact(&report, &["J1.1", "R1.1"]);
    let ground = contact(&report, &["#PWR01.1", "R2.2"]);
    let flag = contact(&report, &["#PWR02.1", "R3.1"]);
    for (finding, at, power_count) in [
        (connector, json!({"x": 55.88, "y": 50.8}), 0),
        (ground, json!({"x": 76.2, "y": 54.61}), 1),
        (flag, json!({"x": 93.98, "y": 46.99}), 1),
    ] {
        assert_eq!(finding["kind"], "pin_contact");
        assert_eq!(finding["point"], at);
        let pins = finding["pins"].as_array().unwrap();
        assert!(pins
            .iter()
            .all(|p| p["unit"] == 1
                && uuid::Uuid::parse_str(p["symbol_uuid"].as_str().unwrap()).is_ok()));
        assert_eq!(
            pins.iter().filter(|p| p["is_power_symbol"] == true).count(),
            power_count
        );
    }
    assert_eq!(connector["wires"], json!([]));
    assert_eq!(
        flag["wires"][0]["uuid"],
        "6541e473-1d7d-47a5-882b-ef983629261d"
    );
    assert_eq!(flag["labels"], json!([]), "a power symbol is not a label");
    assert_eq!(
        sheet.ok("check_schematic_overlaps", json!({})).await["overlap_count"],
        0
    );
    let j = sheet
        .ok(
            "get_pin_connections",
            json!({"reference": "J1", "pin_number": "1"}),
        )
        .await;
    let r = sheet
        .ok(
            "get_pin_connections",
            json!({"reference": "R1", "pin_number": "1"}),
        )
        .await;
    assert!(j["net"].is_string());
    assert_eq!(j["net"], r["net"]);
    assert_eq!(sheet.report().await, report);
    assert_eq!(std::fs::read(&sheet.path).unwrap(), SCH.as_bytes());
    assert_eq!(
        std::fs::read_dir(sheet.path.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn electrical_checks_keep_their_fixture_results() {
    let sheet = Sheet::new(SCH).await;
    let orphans = sheet.ok("find_orphan_items", json!({})).await;
    let items = orphans["orphans"].as_array().unwrap();
    assert_eq!(items.len(), 14);
    assert!(items.iter().all(|item| item["type"] == "unconnected_pin"));
    assert_eq!(
        sheet.ok("validate_component_connections", json!({})).await["unconnected_count"],
        14
    );
    assert_eq!(
        sheet.ok("validate_wire_connections", json!({})).await["valid"],
        true
    );
    let single = sheet.ok("find_single_pin_nets", json!({})).await;
    let mut nets: Vec<_> = single["nets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["net"].as_str().unwrap())
        .collect();
    nets.sort_unstable();
    assert_eq!(nets, ["SIG_A", "SIG_B"]);
    assert_eq!(
        sheet.ok("audit_connections", json!({})).await["findings"],
        json!([])
    );
}

#[tokio::test]
async fn reports_contacts_created_by_placement_and_move() {
    let sheet = Sheet::new(SCH).await;
    sheet
        .ok(
            "add_schematic_component",
            json!({
                "lib_id": "Device:R", "reference": "R30", "value": "10k",
                "x": 123.19, "y": 139.7, "rotation": 90
            }),
        )
        .await;
    sheet
        .ok(
            "move_schematic_component",
            json!({"reference": "R23", "x": 99.06, "y": 147.32}),
        )
        .await;
    let report = sheet.report().await;
    assert_eq!(report["contact_count"], 5);
    contact(&report, &["J3.1", "R30.1"]);
    contact(&report, &["R22.2", "R23.1"]);
}

#[tokio::test]
async fn reports_a_saved_zero_length_wire_between_touching_pins() {
    let at = (55.88, 50.8);
    let source = with_items(SCH, &wire(at, at, 1));
    let report = Sheet::new(&source).await.report().await;
    let short = report["short_wires"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["length_mm"] == 0.0)
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(
        short["wire_uuids"],
        json!(["00000000-0000-4000-8000-000000000001"])
    );
    assert_eq!(pins(&short["start"]["pins"]), ["J1.1", "R1.1"]);
    assert_eq!(short["start"], short["end"]);
}

#[tokio::test]
async fn reports_contacts_created_by_power_placement() {
    let sheet = Sheet::new(SCH).await;
    for args in [
        json!({"power_net": "+5V", "reference": "R20", "pin_number": "1"}),
        json!({"power_net": "GND", "reference": "R20", "pin_number": "2"}),
        json!({"power_net": "PWR_FLAG", "reference": "R21", "pin_number": "1"}),
        json!({"power_net": "GND", "x": 76.2, "y": 115.57}),
    ] {
        sheet.ok("add_power_symbol", args).await;
    }
    for args in [
        json!({"power_net": "GND", "pins": [{"reference": "R21", "pin_number": "2"}]}),
        json!({"power_net": "+5V", "pins": [
            {"reference": "R22", "pin_number": "1"},
            {"reference": "R23", "pin_number": "1"}
        ]}),
    ] {
        sheet.ok("batch_add_power_symbol", args).await;
    }
    let report = sheet.report().await;
    assert_eq!(report["contact_count"], 10);
    for pin in [
        "R20.1", "R20.2", "R21.1", "R11.2", "R21.2", "R22.1", "R23.1",
    ] {
        assert!(
            report["contacts"].as_array().unwrap().iter().any(|c| {
                pins(&c["pins"]).iter().any(|name| name == pin)
                    && c["pins"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|p| p["is_power_symbol"] == true)
                        .count()
                        == 1
            }),
            "missing power contact at {pin}: {report}"
        );
    }
}

#[tokio::test]
async fn counts_different_units_of_one_reference_as_distinct_symbols() {
    const ECC83: &str = include_str!("../../tests/fixtures/ecc83_multiunit.kicad_sch");
    let source = ECC83.replace(
        "(at 157.48 107.95 0)\n\t\t(unit 2)",
        "(at 160.02 64.77 0)\n\t\t(unit 2)",
    );
    assert_ne!(source, ECC83);
    let report = Sheet::new(&source).await.report().await;
    let contact = report["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|contact| contact["point"] == json!({"x": 152.4, "y": 64.77}))
        .unwrap_or_else(|| panic!("{report}"));
    let pins = contact["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins[0]["reference"], pins[1]["reference"]);
    assert_eq!(pins[0]["unit"], 1);
    assert_eq!(pins[0]["pin_number"], "7");
    assert_eq!(pins[1]["unit"], 2);
    assert_eq!(pins[1]["pin_number"], "2");
    assert_ne!(pins[0]["symbol_uuid"], pins[1]["symbol_uuid"]);
}

#[tokio::test]
async fn hidden_pins_do_not_supply_a_second_symbol_at_a_contact() {
    let source = SCH.replacen(
        "(pin passive line",
        "(pin passive line\n\t\t\t\t\t(hide yes)",
        1,
    );
    assert_ne!(source, SCH);
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["contact_count"], 2, "{report}");
    assert!(report["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| !pins(&c["pins"]).contains(&"J1.1".to_owned())));
}

#[tokio::test]
async fn hidden_pins_are_not_short_wire_terminals() {
    let source = SCH.replace(
        "(at 0 -3.81 90)\n\t\t\t\t\t(length",
        "(at 0 -3.81 90)\n\t\t\t\t\t(hide yes)\n\t\t\t\t\t(length",
    );
    assert_ne!(source, SCH);
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
}

#[tokio::test]
async fn pins_stacked_in_one_symbol_do_not_create_a_contact_on_their_own() {
    let source = SCH.replace(
        "(symbol \"Conn_01x01_1_1\"",
        "(symbol \"Conn_01x01_1_1\" (pin passive line (at -5.08 0 0) \
         (length 3.81) (name \"Stacked\") (number \"2\"))",
    );
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["contact_count"], 3, "{report}");
    contact(&report, &["J1.1", "J1.2", "R1.1"]);
}

#[tokio::test]
async fn alternate_body_pins_are_only_reported_for_the_selected_style() {
    let source = SCH.replace(
        "(symbol \"Conn_01x01_1_1\"",
        "(symbol \"Conn_01x01_1_2\" (pin passive line (at -5.08 0 0) \
         (length 3.81) (name \"Alt\") (number \"2\")))\n\t\t\t(symbol \"Conn_01x01_1_1\"",
    );
    let report = Sheet::new(&source).await.report().await;
    contact(&report, &["J1.1", "R1.1"]);
    let alternate = source.replace(
        "(at 50.8 50.8 0)\n\t\t(mirror y)\n\t\t(unit 1)\n\t\t(body_style 1)",
        "(at 50.8 50.8 0)\n\t\t(mirror y)\n\t\t(unit 1)\n\t\t(body_style 2)",
    );
    assert_ne!(alternate, source);
    let report = Sheet::new(&alternate).await.report().await;
    contact(&report, &["J1.2", "R1.1"]);
}

#[tokio::test]
async fn short_wires_use_strict_configurable_length() {
    let sheet = Sheet::new(SCH).await;
    let report = sheet.report().await;
    assert_eq!(report["short_wire_count"], 1);
    let short = &report["short_wires"][0];
    assert_eq!(short["kind"], "short_pin_wire");
    assert_eq!(short["length_mm"], json!(1.27));
    assert_eq!(
        short["wire_uuids"],
        json!(["aff29961-cf6e-451e-a6db-44db76226c6a"])
    );
    assert_eq!(pins(&short["start"]["pins"]), ["R8.2"]);
    assert_eq!(pins(&short["end"]["pins"]), ["R9.1"]);
    for limit in [0.0, 1.27, 1.28, 2.55] {
        let result = sheet.ok(TOOL, json!({"min_wire_mm": limit})).await;
        assert_eq!(result["contacts"], report["contacts"]);
        let expected = if limit <= 1.27 {
            0
        } else if limit <= 2.54 {
            1
        } else {
            4
        };
        assert_eq!(result["short_wire_count"], expected, "{result}");
    }
    assert_eq!(sheet.ok(TOOL, json!({"min_wire_mm": null})).await, report);
}

#[tokio::test]
async fn short_wire_run_sums_segments_and_stops_at_each_inner_attachment() {
    let a = (63.5, 105.41);
    let joint = (63.5, 106.05);
    let b = (63.5, 106.68);
    let split = SCH.replace(
        "(xy 63.5 105.41) (xy 63.5 106.68)",
        "(xy 63.5 105.41) (xy 63.5 106.05)",
    );
    let split = with_items(&split, &wire(b, joint, 1));
    let result = Sheet::new(&split).await.report().await;
    assert_eq!(result["short_wire_count"], 1);
    assert_eq!(result["short_wires"][0]["length_mm"], 1.27);
    assert_eq!(
        result["short_wires"][0]["wire_uuids"],
        json!([
            "aff29961-cf6e-451e-a6db-44db76226c6a",
            "00000000-0000-4000-8000-000000000001"
        ])
    );
    let (x, y) = joint;
    let junction = format!("(junction (at {x} {y}) (diameter 0) (color 0 0 0 0))");
    let label = format!("(label \"TEST\" (at {x} {y} 0))");
    let no_connect = format!("(no_connect (at {x} {y}))");
    for item in [
        junction,
        label,
        no_connect,
        wire(joint, (a.0 + 3.0, y), 2),
        wire((x - 3.0, y), (x + 3.0, y), 2),
        wire((x - 3.0, y - 3.0), (x + 3.0, y + 3.0), 2),
    ] {
        let source = with_items(&split, &item);
        let report = Sheet::new(&source).await.report().await;
        assert_eq!(report["short_wire_count"], 0, "{item}: {report}");
    }
}

#[tokio::test]
async fn a_small_gap_between_wire_ends_does_not_join_runs() {
    let source = SCH.replace(
        "(xy 63.5 105.41) (xy 63.5 106.68)",
        "(xy 63.5 105.41) (xy 63.5 106.05)",
    );
    let source = with_items(&source, &wire((63.5, 106.056), (63.5, 106.68), 1));
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
}

#[tokio::test]
async fn a_wire_stopping_short_of_a_pin_does_not_end_a_pin_run() {
    let source = SCH.replace("(xy 63.5 106.68)", "(xy 63.5 106.674)");
    assert_ne!(source, SCH);
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
}

#[tokio::test]
async fn only_the_visible_pin_breaks_an_inner_wire_joint() {
    let source = SCH.replace(
        "(xy 63.5 105.41) (xy 63.5 106.68)",
        "(xy 63.5 105.41) (xy 63.5 106.05)",
    );
    let source = with_items(
        &source,
        &format!(
            "{}{}",
            wire((63.5, 106.05), (63.5, 106.68), 1),
            ground_at(63.5, 106.05, 101)
        ),
    );
    let visible = Sheet::new(&source).await.report().await;
    assert_eq!(visible["short_wire_count"], 2, "{visible}");

    let hidden = source.replace(
        "(pin power_in line\n\t\t\t\t\t(at 0 0 270)",
        "(pin power_in line (hide yes)\n\t\t\t\t\t(at 0 0 270)",
    );
    assert_ne!(hidden, source);
    let report = Sheet::new(&hidden).await.report().await;
    assert_eq!(report["short_wire_count"], 1, "{report}");
    let short = &report["short_wires"][0];
    assert_eq!(short["length_mm"], 1.27);
    assert_eq!(pins(&short["start"]["pins"]), ["R8.2"]);
    assert_eq!(pins(&short["end"]["pins"]), ["R9.1"]);
}

#[tokio::test]
async fn a_nearby_visible_pin_does_not_break_a_wire_joint() {
    let source = SCH.replace(
        "(xy 63.5 105.41) (xy 63.5 106.68)",
        "(xy 63.5 105.41) (xy 63.5 106.05)",
    );
    let source = with_items(
        &source,
        &format!(
            "{}{}",
            wire((63.5, 106.05), (63.5, 106.68), 1),
            ground_at(63.506, 106.05, 101)
        ),
    );
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 1, "{report}");
    assert_eq!(report["short_wires"][0]["length_mm"], 1.27);
}

#[tokio::test]
async fn coincident_pins_report_annotations_at_the_contact() {
    let source = with_items(
        SCH,
        "(junction (at 55.88 50.8) (diameter 0) (color 0 0 0 0)) \
         (global_label \"AT_CONTACT\" (at 55.88 50.8 0))",
    );
    let report = Sheet::new(&source).await.report().await;
    let contact = contact(&report, &["J1.1", "R1.1"]);
    assert_eq!(contact["junctions"], json!([{"x": 55.88, "y": 50.8}]));
    assert_eq!(contact["labels"][0]["net"], "AT_CONTACT");
}

#[tokio::test]
async fn labels_on_unsplit_wires_end_short_runs_but_terminal_labels_do_not() {
    let tapped = with_items(SCH, "(label \"TAP\" (at 63.5 106.045 0))");
    assert_eq!(
        Sheet::new(&tapped).await.report().await["short_wire_count"],
        0
    );
    let terminal = with_items(SCH, "(label \"TERMINAL\" (at 63.5 105.41 0))");
    assert_eq!(
        Sheet::new(&terminal).await.report().await["short_wire_count"],
        1
    );
}

#[tokio::test]
async fn dotted_tap_on_an_unsplit_wire_breaks_the_short_run() {
    let tapped = with_items(
        SCH,
        &format!(
            "{} (junction (at 63.5 106.045) (diameter 0) (color 0 0 0 0))",
            wire((63.5, 106.045), (70.0, 106.045), 40)
        ),
    );
    assert_eq!(
        Sheet::new(&tapped).await.report().await["short_wire_count"],
        0
    );
}

#[tokio::test]
async fn dotted_tap_just_inside_an_unsplit_wire_end_breaks_the_short_run() {
    let source = with_items(
        SCH,
        &format!(
            "{} (junction (at 63.5 106.674) (diameter 0) (color 0 0 0 0))",
            wire((63.5, 106.674), (70.0, 106.674), 40)
        ),
    );
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
}

#[tokio::test]
async fn directive_labels_are_reported_at_contacts_and_break_inner_joints() {
    let source = SCH.replace(
        "(xy 63.5 105.41) (xy 63.5 106.68)",
        "(xy 63.5 105.41) (xy 63.5 106.05)",
    );
    let source = with_items(
        &source,
        &format!(
            "{}\n(netclass_flag \"\" (at 55.88 50.8 0))\
             \n(netclass_flag \"\" (at 63.5 106.05 0))",
            wire((63.5, 106.05), (63.5, 106.68), 1)
        ),
    );
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
    let finding = contact(&report, &["J1.1", "R1.1"]);
    assert_eq!(finding["labels"][0]["kind"], "DirectiveLabel");
    assert!(finding["labels"][0]["net"].is_null());
}

#[tokio::test]
async fn directive_on_an_unsplit_wire_breaks_the_short_run() {
    let source = with_items(SCH, "(netclass_flag \"\" (at 63.5 106.045 0))");
    let report = Sheet::new(&source).await.report().await;
    assert_eq!(report["short_wire_count"], 0, "{report}");
}

#[tokio::test]
async fn invalid_min_wire_mm_is_refused_without_a_write() {
    let sheet = Sheet::new(SCH).await;
    for value in [json!(-0.5), json!("2.54")] {
        let result = sheet.call(TOOL, json!({"min_wire_mm": value})).await;
        assert!(result.is_error, "accepted {value}: {}", text_of(&result));
        let body: Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "min_wire_mm");
    }
    assert_eq!(std::fs::read(&sheet.path).unwrap(), SCH.as_bytes());
}

#[test]
fn minimum_is_checked_independently_by_schema_and_handler() {
    let file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
    std::fs::write(file.path(), SCH).unwrap();
    let definition = super::definition();
    for min in [json!(-0.5), json!("2.54")] {
        let args = json!({"schematic": file.path(), "min_wire_mm": min});
        assert!(!definition.input_validator.is_valid(&args), "{args}");
        let result = super::handle_find_pin_contacts(&args).unwrap();
        assert!(result.is_error, "direct handler accepted {args}");
        assert_eq!(std::fs::read(file.path()).unwrap(), SCH.as_bytes());
    }
}

#[tokio::test]
async fn a_non_schematic_is_not_an_empty_report() {
    let source = "(kicad_pcb (version 20260101))";
    let sheet = Sheet::new(source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}

#[tokio::test]
async fn an_unresolved_symbol_is_not_an_empty_pin_list() {
    let source = SCH.replace("(lib_id \"Device:R\")", "(lib_id \"Device:Missing\")");
    let sheet = Sheet::new(&source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}

#[tokio::test]
async fn an_unreadable_library_pin_is_not_silently_dropped() {
    let source = SCH.replacen(
        "(pin passive line\n\t\t\t\t\t(at -5.08 0 0)",
        "(pin passive line\n\t\t\t\t\t(at unknown 0 0)",
        1,
    );
    assert_ne!(source, SCH);
    let sheet = Sheet::new(&source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}

#[tokio::test]
async fn an_unreadable_wire_is_not_silently_dropped() {
    let source = SCH.replace("(xy 63.5 106.68)", "(xy 63.5 unknown)");
    assert_ne!(source, SCH);
    let sheet = Sheet::new(&source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}

#[tokio::test]
async fn an_unreadable_label_anchor_is_not_silently_dropped() {
    let source = with_items(SCH, "(label \"TAP\" (at 63.5 unknown 0))");
    let sheet = Sheet::new(&source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}

#[tokio::test]
async fn an_unreadable_directive_anchor_is_not_silently_dropped() {
    let source = with_items(SCH, "(netclass_flag \"\" (at 63.5 unknown 0))");
    let sheet = Sheet::new(&source).await;
    let result = sheet.call(TOOL, json!({})).await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(std::fs::read(&sheet.path).unwrap(), source.as_bytes());
}
