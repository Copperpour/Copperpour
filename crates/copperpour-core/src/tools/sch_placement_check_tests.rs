use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

const TOOL: &str = "check_schematic_placement";
/// KiCad 10.0.8-resaved; every pin on the 1.27 mm grid (README beside it).
const CROWDED: &str =
    include_str!("../../tests/fixtures/region_cleanup_kicad10/crowded/crowded.kicad_sch");
/// KiCad's own hv_converter demo; C4 pin 2 sits at (149.352, 84.328).
const OFF_GRID_DEMO: &str = include_str!("../../tests/fixtures/power_on_off_grid_pin.kicad_sch");
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
        let result = self.call(tool, json!({})).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }

    async fn error(&self) -> Value {
        let result = self.call(TOOL, json!({})).await;
        assert!(result.is_error, "{}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }
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
async fn the_grid_boundary_is_one_internal_unit() {
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
    assert!(
        pins.iter().any(|p| p["reference"] == "C4"
            && p["pin_number"] == "2"
            && p["x"] == 149.352
            && p["y"] == 84.328),
        "{report}"
    );
    for pin in pins {
        let (x, y) = (pin["x"].as_f64().unwrap(), pin["y"].as_f64().unwrap());
        assert!(
            !copperpour_sexp::geometry::is_on_schematic_pin_grid(x)
                || !copperpour_sexp::geometry::is_on_schematic_pin_grid(y),
            "{pin}"
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
    assert_eq!(sheet.error().await["error"]["kind"], "malformed_file");
    std::fs::remove_file(&sheet.path).unwrap();
    assert_eq!(sheet.error().await["error"]["kind"], "file_not_found");
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
