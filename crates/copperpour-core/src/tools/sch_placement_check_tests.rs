use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

const TOOL: &str = "check_placement";
/// KiCad 10.0.8-resaved; every pin on the 1.27 mm grid (README beside it).
const CROWDED: &str =
    include_str!("../../tests/fixtures/region_cleanup_kicad10/crowded/crowded.kicad_sch");
/// KiCad's own hv_converter demo; C4 pin 2 sits at (149.352, 84.328).
const OFF_GRID_DEMO: &str = include_str!("../../tests/fixtures/power_on_off_grid_pin.kicad_sch");
const PIN_CONTACT: &str = include_str!("../../tests/fixtures/symbol_pin_contact_kicad10.kicad_sch");
const PIN_UNITS: &str = include_str!("../../tests/fixtures/power_on_pin_kicad10.kicad_sch");
const R1: &str = "0e4b969c-bf56-4471-a20e-1f3b408347b2";
const R3: &str = "31f08b8b-942d-4758-9b9b-d451a347415c";
const R4_FIRST: &str = "0a77040c-0ea5-41d4-abeb-7d443553e1ac";
const R4_SECOND: &str = "4582dc43-08fe-4b74-9db2-827dd5fcbe91";
/// U1's placement line in `CROWDED`, unique because of the lib_id before it.
const U1_AT: &str = "(lib_id \"Regulator_Linear:AMS1117-3.3\")\n\t\t(at 127 76.2 0)";

struct Sheet {
    _dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
}

impl Sheet {
    async fn new(name: &str, source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
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

    async fn ok(&self, tool: &str) -> Value {
        self.ok_with(tool, json!({})).await
    }

    async fn ok_with(&self, tool: &str, args: Value) -> Value {
        let result = self.call(tool, args).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }

    async fn error(&self) -> Value {
        self.error_with(json!({})).await
    }

    async fn error_with(&self, args: Value) -> Value {
        let result = self.call(TOOL, args).await;
        assert!(result.is_error, "{}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }
}

fn selector(symbol_uuid: &str, pin_number: &str) -> Value {
    json!({"symbol_uuid": symbol_uuid, "pin_number": pin_number})
}

fn pair(a: (&str, &str), b: (&str, &str), alignment: &str) -> Value {
    json!({"a": selector(a.0, a.1), "b": selector(b.0, b.1), "alignment": alignment})
}

fn pin_names(pins: &Value) -> Vec<String> {
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

fn shifted_u1(dx: &str) -> String {
    assert_eq!(CROWDED.matches(U1_AT).count(), 1);
    CROWDED.replace(
        U1_AT,
        &format!("(lib_id \"Regulator_Linear:AMS1117-3.3\")\n\t\t(at {dx} 76.2 0)"),
    )
}

#[tokio::test]
async fn an_on_grid_sheet_reports_no_off_grid_pins() {
    let sheet = Sheet::new("crowded.kicad_sch", CROWDED).await;
    let report = sheet.ok(TOOL).await;
    assert_eq!(report["off_grid_pin_count"], 0, "{report}");
    assert_eq!(report["off_grid_pins"], json!([]), "{report}");
    assert_eq!(report["pin_grid_mm"], 1.27);
    assert_eq!(report["source"], "saved");
    assert!(report["pin_count"].as_u64().unwrap() > 0, "{report}");
}

#[tokio::test]
async fn counts_only_placed_units_and_body_styles() {
    let sheet = Sheet::new("power_on_pin_kicad10.kicad_sch", PIN_UNITS).await;
    let report = sheet.ok(TOOL).await;
    // Five resistors, three amplifier pins, two amplifier power pins,
    // three NAND pins in the selected body style and two power symbols.
    assert_eq!(report["pin_count"], 20, "{report}");
    assert_eq!(report["off_grid_pin_count"], 0, "{report}");
}

#[tokio::test]
async fn compares_only_requested_axes_at_the_placed_pin_tips() {
    let sheet = Sheet::new("power_on_pin_kicad10.kicad_sch", PIN_UNITS).await;
    let baseline = sheet.ok(TOOL).await;
    assert_eq!(baseline["alignment_pair_count"], 0);
    assert_eq!(baseline["misaligned_pairs"], json!([]));
    for empty in [Value::Null, json!([])] {
        assert_eq!(
            sheet.ok_with(TOOL, json!({"alignment_pairs": empty})).await,
            baseline
        );
    }

    // Fixture README records the pin coordinates, including mirrored R3 and
    // two distinct placed symbols whose references are both R4.
    let report = sheet
        .ok_with(
            TOOL,
            json!({"alignment_pairs": [
                pair((R1, "1"), (R3, "2"), "row"),
                pair((R1, "1"), (R3, "1"), "row"),
                pair((R1, "1"), (R1, "2"), "column"),
                pair((R4_FIRST, "1"), (R4_SECOND, "1"), "column"),
            ]}),
        )
        .await;
    assert_eq!(report["alignment_pair_count"], 4, "{report}");
    assert_eq!(
        report["misaligned_pairs"],
        json!([
            {
                "pair_index": 1, "alignment": "row", "offset_mm": 7.62,
                "a": {"reference": "R1", "symbol_uuid": R1, "unit": 1, "pin_number": "1", "x": 100.33, "y": 76.2},
                "b": {"reference": "R3", "symbol_uuid": R3, "unit": 1, "pin_number": "1", "x": 160.02, "y": 83.82}
            },
            {
                "pair_index": 3, "alignment": "column", "offset_mm": 20.32,
                "a": {"reference": "R4", "symbol_uuid": R4_FIRST, "unit": 1, "pin_number": "1", "x": 180.34, "y": 116.84},
                "b": {"reference": "R4", "symbol_uuid": R4_SECOND, "unit": 1, "pin_number": "1", "x": 200.66, "y": 116.84}
            }
        ])
    );
    let mut without_alignment = report;
    without_alignment["alignment_pair_count"] = json!(0);
    without_alignment["misaligned_pairs"] = json!([]);
    assert_eq!(without_alignment, baseline);
    assert_eq!(std::fs::read_to_string(&sheet.path).unwrap(), PIN_UNITS);
}

#[tokio::test]
async fn alignment_counts_one_internal_unit_of_displacement() {
    let sheet = Sheet::new("crowded.kicad_sch", &shifted_u1("127.0001")).await;
    let result = sheet
        .ok_with(
            TOOL,
            json!({"alignment_pairs": [pair(
                ("523c30f7-d4fe-4561-9a1f-3f12842397a8", "1"),
                ("363a844c-3648-47ba-86d2-e9f873584d38", "1"),
                "column"
            )]}),
        )
        .await;
    assert_eq!(result["misaligned_pairs"][0]["offset_mm"], 0.0001);
}

#[tokio::test]
async fn invalid_pairs_refuse_the_whole_call_and_name_the_field() {
    let sheet = Sheet::new("power_on_pin_kicad10.kicad_sch", PIN_UNITS).await;
    let valid = pair((R1, "1"), (R3, "1"), "row");
    for (bad_pair, field) in [
        (
            pair(
                (R1, "1"),
                ("00000000-0000-4000-8000-000000000001", "1"),
                "row",
            ),
            "alignment_pairs[1].b.symbol_uuid",
        ),
        (
            pair((R1, "9"), (R3, "1"), "row"),
            "alignment_pairs[1].a.pin_number",
        ),
        (pair((R1, "1"), (R1, "1"), "row"), "alignment_pairs[1]"),
        (
            pair((R1, "1"), (R3, "1"), "diagonal"),
            "alignment_pairs[1].alignment",
        ),
    ] {
        let body = sheet
            .error_with(json!({"alignment_pairs": [valid, bad_pair]}))
            .await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
        assert!(body.get("misaligned_pairs").is_none(), "{body}");
    }
}

#[tokio::test]
async fn duplicate_symbol_uuid_is_ambiguous_even_if_only_one_copy_has_the_pin() {
    // Give R1 the UUID of the LM358's placed power unit. Only R1
    // has pin 1; the duplicated symbol identity must still refuse.
    const POWER_UNIT: &str = "4e0150a7-4ed4-49f1-a390-911978a30425";
    let source = PIN_UNITS.replace(R1, POWER_UNIT);
    let sheet = Sheet::new("power_on_pin_kicad10.kicad_sch", &source).await;
    let body = sheet
        .error_with(json!({"alignment_pairs": [pair((POWER_UNIT, "1"), (R3, "1"), "row")]}))
        .await;
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(
        body["error"]["field"], "alignment_pairs[0].a.symbol_uuid",
        "{body}"
    );
    assert_eq!(
        body["error"]["reason"],
        "symbol UUID is not unique on the saved sheet"
    );
}

#[tokio::test]
async fn same_number_at_one_point_resolves_but_at_two_points_refuses() {
    const A: &str = "00000000-0000-4000-8000-000000000001";
    const B: &str = "00000000-0000-4000-8000-000000000002";
    let source = |second_x| {
        format!(
            r#"(kicad_sch (version 20260306)
            (lib_symbols (symbol "Test:Dup" (symbol "Dup_1_1"
                (pin passive line (at 0 0 0) (length 1.27) (number "1"))
                (pin passive line (at {second_x} 0 0) (length 1.27) (number "1")))))
            (symbol (lib_id "Test:Dup") (at 12.7 25.4 0) (uuid "{A}"))
            (symbol (lib_id "Test:Dup") (at 25.4 25.4 0) (uuid "{B}")))"#
        )
    };
    let args = json!({"alignment_pairs": [pair((A, "1"), (B, "1"), "row")]});
    let stacked = Sheet::new("stacked.kicad_sch", &source(0.0)).await;
    let report = stacked.ok_with(TOOL, args.clone()).await;
    assert_eq!(report["pin_count"], 2, "{report}");
    assert_eq!(report["alignment_pair_count"], 1);
    assert_eq!(report["misaligned_pairs"], json!([]));

    let split = Sheet::new("split.kicad_sch", &source(1.27)).await;
    let body = split.error_with(args).await;
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(
        body["error"]["field"], "alignment_pairs[0].a.pin_number",
        "{body}"
    );
    assert_eq!(
        body["error"]["reason"],
        "pin number has multiple placed endpoints"
    );
}

/// Half a grid step moves only U1's three pins off the grid. Their expected
/// positions are KiCad's (fixture README) plus the shift, not this tool's math.
#[tokio::test]
async fn a_half_grid_shift_reports_exactly_the_moved_symbols_pins() {
    let sheet = Sheet::new("crowded.kicad_sch", &shifted_u1("127.635")).await;
    let report = sheet.ok(TOOL).await;
    assert_eq!(
        pin_names(&report["off_grid_pins"]),
        ["U1.1", "U1.2", "U1.3"],
        "{report}"
    );
    let at = |number: &str| {
        let pin = report["off_grid_pins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["pin_number"] == number)
            .unwrap();
        (pin["x"].as_f64().unwrap(), pin["y"].as_f64().unwrap())
    };
    assert_eq!(at("1"), (127.635, 83.82));
    assert_eq!(at("2"), (135.255, 76.2));
    assert_eq!(at("3"), (120.015, 76.2));
    assert_eq!(
        report["off_grid_pins"][0]["symbol_uuid"],
        "523c30f7-d4fe-4561-9a1f-3f12842397a8"
    );
}

/// One KiCad internal unit (0.0001 mm) of drift is off the grid; a whole
/// grid step is not.
#[tokio::test]
async fn one_internal_unit_of_drift_is_off_grid() {
    let drift = Sheet::new("crowded.kicad_sch", &shifted_u1("127.0001")).await;
    assert_eq!(drift.ok(TOOL).await["off_grid_pin_count"], 3);
    let step = Sheet::new("crowded.kicad_sch", &shifted_u1("128.27")).await;
    assert_eq!(step.ok(TOOL).await["off_grid_pin_count"], 0);
}

#[tokio::test]
async fn kicads_own_off_grid_pin_is_reported() {
    let sheet = Sheet::new("hv_converter.kicad_sch", OFF_GRID_DEMO).await;
    let report = sheet.ok(TOOL).await;
    let pins = report["off_grid_pins"].as_array().unwrap();
    // C4 is rotated. D2 uses the sheet-local lib_name, not its base lib_id.
    for (reference, x, y) in [("C4", 149.352, 84.328), ("D2", 131.572, 96.52)] {
        assert!(
            pins.iter().any(|p| p["reference"] == reference
                && p["pin_number"] == "2"
                && p["x"] == x
                && p["y"] == y),
            "{reference}.2 missing: {report}"
        );
    }
}

/// One source for overlaps: the placement check returns exactly what
/// check_schematic_overlaps returns for the same sheet.
#[tokio::test]
async fn overlaps_match_check_schematic_overlaps() {
    for (name, source) in [
        ("crowded.kicad_sch", CROWDED.to_owned()),
        ("crowded.kicad_sch", shifted_u1("127.635")),
        ("hv_converter.kicad_sch", OFF_GRID_DEMO.to_owned()),
    ] {
        let sheet = Sheet::new(name, &source).await;
        let placement = sheet.ok(TOOL).await;
        let overlaps = sheet.ok("check_schematic_overlaps").await;
        for key in [
            "overlap_count",
            "overlaps",
            "bounds_resolved",
            "bounds_unresolved",
        ] {
            assert_eq!(placement[key], overlaps[key], "{name} {key}");
        }
    }
    let crowded = Sheet::new("crowded.kicad_sch", CROWDED).await;
    assert!(crowded.ok(TOOL).await["overlap_count"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn a_collision_is_reported_but_a_pin_tip_contact_is_not() {
    let sheet = Sheet::new("symbol_pin_contact_kicad10.kicad_sch", PIN_CONTACT).await;
    let report = sheet.ok(TOOL).await;
    let overlaps = report["overlaps"].as_array().unwrap();
    let collides = |a: &str, b: &str| {
        overlaps.iter().any(|row| {
            row["type"] == "component_overlap"
                && ((row["a"] == a && row["b"] == b) || (row["a"] == b && row["b"] == a))
        })
    };
    assert!(collides("R12", "R13"), "{report}");
    assert!(!collides("R10", "#PWR01"), "{report}");
}

#[tokio::test]
async fn unresolved_pin_geometry_is_refused_not_reported_clean() {
    let source = CROWDED.replace(
        "(lib_id \"Regulator_Linear:AMS1117-3.3\")\n\t\t(at",
        "(lib_id \"Regulator_Linear:NOT_EMBEDDED\")\n\t\t(at",
    );
    assert_ne!(source, CROWDED);
    let sheet = Sheet::new("crowded.kicad_sch", &source).await;
    let body = sheet.error().await;
    assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
    assert_eq!(
        body["error"]["reason"],
        crate::tools::UNRESOLVED_PIN_GEOMETRY,
        "{body}"
    );
}

#[tokio::test]
async fn missing_and_non_schematic_files_are_named_errors() {
    let sheet = Sheet::new("crowded.kicad_sch", "(kicad_pcb (version 20260306))").await;
    let body = sheet.error().await;
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(body["error"]["path"], json!(sheet.path));
    std::fs::remove_file(&sheet.path).unwrap();
    assert_eq!(sheet.error().await["error"]["kind"], "file_not_found");
}

#[tokio::test]
async fn unreadable_overlap_inputs_are_not_treated_as_clean() {
    for (item, reason) in [
        (
            "(wire (pts (xy 1 2)))",
            "one or more wires have unreadable endpoints",
        ),
        (
            "(label \"SIG\" (at 1))",
            "one or more labels have unreadable anchors",
        ),
    ] {
        let source = format!("(kicad_sch (version 20260306) (lib_symbols) {item})");
        let sheet = Sheet::new("malformed.kicad_sch", &source).await;
        let body = sheet.error().await;
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["reason"], reason, "{body}");
    }
}

#[tokio::test]
async fn the_tool_is_read_only() {
    let shifted = shifted_u1("127.635");
    let sheet = Sheet::new("crowded.kicad_sch", &shifted).await;
    sheet.ok(TOOL).await;
    assert_eq!(std::fs::read_to_string(&sheet.path).unwrap(), shifted);
    let siblings: Vec<_> = std::fs::read_dir(sheet.path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(siblings, ["crowded.kicad_sch"]);
}
