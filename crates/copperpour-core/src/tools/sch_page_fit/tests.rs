use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::{
    project_file_writer_tests::{body_of, served_call},
    ServerConfig,
};
use std::{collections::BTreeMap, fs, path::PathBuf};

const TOOL: &str = "check_schematic_page_fit";
const PORTRAIT: &str = "schematic_page_fit_portrait_kicad10.kicad_sch";
const CUSTOM: &str = "schematic_page_fit_custom_sheet_kicad10.kicad_sch";
const MARKED: &str = "page_fit_custom_marked.kicad_wks";
const UNMARKED: &str = "page_fit_custom_unmarked.kicad_wks";
const COVERAGE: &str = "schematic_page_fit_coverage_kicad10.kicad_sch";

struct Sheet {
    dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
}

impl Sheet {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = copy_fixture(dir.path(), name);
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Self { dir, path, handler }
    }

    fn configure(&self, name: &str) {
        let settings = json!({ "schematic": { "page_layout_descr_file": name } });
        fs::write(self.path.with_extension("kicad_pro"), settings.to_string()).unwrap();
    }

    async fn setup(&self, tool: &str, mut arguments: Value) -> Value {
        arguments["schematic"] = json!(self.path);
        let result = served_call(&self.handler, tool, arguments).await;
        let body = body_of(&result);
        assert!(!result.is_error, "{tool}: {body}");
        body
    }

    async fn call(&self, path: &Path) -> CallToolResult {
        let before = snapshot(self.dir.path());
        let result = served_call(&self.handler, TOOL, json!({ "schematic": path })).await;
        assert_eq!(
            snapshot(self.dir.path()),
            before,
            "the check must not write project files"
        );
        result
    }

    async fn report(&self) -> Value {
        let result = self.call(&self.path).await;
        let body = body_of(&result);
        assert!(!result.is_error, "{body}");
        body
    }
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            entries.extend(snapshot(&path));
            entries.insert(path, None);
        } else {
            let bytes = fs::read(&path).unwrap();
            entries.insert(path, Some(bytes));
        }
    }
    entries
}

fn copy_fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::copy(source, &path).unwrap();
    path
}

fn finding<'a>(body: &'a Value, uuid: &str) -> &'a Value {
    body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["uuid"] == uuid)
        .unwrap_or_else(|| panic!("no finding for {uuid}: {body}"))
}

fn near(actual: &Value, expected: f64) {
    let actual = actual.as_f64().unwrap();
    assert!((actual - expected).abs() < 0.005, "{actual} vs {expected}");
}

fn assert_box(actual: &Value, expected: [f64; 4]) {
    for (axis, expected) in ["x_min", "y_min", "x_max", "y_max"]
        .into_iter()
        .zip(expected)
    {
        near(&actual[axis], expected);
    }
}

fn assert_complete(report: &Value, count: usize, fits: bool) {
    assert_eq!(report["measured_count"], count, "{report}");
    assert_eq!(report["coverage_complete"], true, "{report}");
    assert_eq!(report["unmeasured"], json!([]), "{report}");
    assert_eq!(report["fits"], fits, "{report}");
}

fn assert_unavailable(report: &Value, reason: &str) {
    assert_eq!(report["drawing_area_unavailable"], reason, "{report}");
    assert_eq!(report["drawing_area"], Value::Null);
    assert_eq!(report["findings"], json!([]));
    assert_eq!(report["coverage_complete"], false);
    assert_eq!(report["fits"], Value::Null);
}

// The coordinates and controls below come from the KiCad 10.0.8 plots in
// schematic_page_fit_kicad10.README.md. Exact finding counts exclude the controls.
#[tokio::test]
async fn portrait_notes_cross_the_frame_and_the_title_block_independently() {
    let sheet = Sheet::new(PORTRAIT).await;
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "builtin");
    assert_box(
        &body["drawing_area"]["frame"],
        [12.0, 12.0, 198.0072, 285.0022],
    );
    assert_box(
        &body["drawing_area"]["title_block"],
        [90.0072, 253.0022, 198.0072, 285.0022],
    );
    assert_complete(&body, 4, false);
    assert_eq!(body["findings"].as_array().unwrap().len(), 2, "{body}");
    let outside = finding(&body, "6b4afa8a-f208-4bc5-be79-f0e327405a78");
    near(&outside["frame_overflow_mm"]["left"], 0.6737);
    assert_eq!(outside["title_block_intrusion"], false);
    let intrusion = finding(&body, "193e18ce-df0a-41c3-b92c-b3084cf504de");
    assert_box(
        &intrusion["bounds"],
        [95.3263, 273.2403, 113.3189, 274.6627],
    );
    assert_eq!(intrusion["title_block_intrusion"], true);
    assert_eq!(
        intrusion["frame_overflow_mm"],
        json!({"left": 0.0, "top": 0.0, "right": 0.0, "bottom": 0.0})
    );
}

#[tokio::test]
async fn landscape_a3_checks_note_ink_and_wire_centrelines() {
    let sheet = Sheet::new("schematic_page_fit_a3_landscape_kicad10.kicad_sch").await;
    let body = sheet.report().await;
    assert_box(
        &body["drawing_area"]["frame"],
        [12.0, 12.0, 407.989, 285.0022],
    );
    assert_box(
        &body["drawing_area"]["title_block"],
        [299.989, 253.0022, 407.989, 285.0022],
    );
    assert_complete(&body, 7, false);
    assert_eq!(body["findings"].as_array().unwrap().len(), 4, "{body}");
    let right = finding(&body, "0dfce099-79ac-4a75-b8d0-bdf9cc031d70");
    near(&right["frame_overflow_mm"]["right"], 6.8418);
    assert_eq!(right["title_block_intrusion"], false);
    let down = finding(&body, "70552b90-0d7a-463d-aab2-0b27965da363");
    near(&down["frame_overflow_mm"]["bottom"], 4.5578);
    assert_eq!(down["title_block_intrusion"], false);
    let note = finding(&body, "a53e7b30-d36e-4ca9-83b2-df9fa2a6de3a");
    assert_eq!(note["title_block_intrusion"], true);
    let wire = finding(&body, "8cbfcdde-6cca-472f-bb33-c11c5f77c426");
    assert_eq!(wire["kind"], "wire");
    assert_eq!(wire["title_block_intrusion"], true);
    assert_eq!(sheet.report().await, body);
}

#[tokio::test]
async fn notes_that_only_conflict_with_the_custom_worksheet_fit_the_default() {
    let body = Sheet::new(CUSTOM).await.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "builtin");
    assert_complete(&body, 3, true);
}

async fn custom_geometry(configured: &str) {
    let sheet = Sheet::new(CUSTOM).await;
    copy_fixture(sheet.dir.path(), MARKED);
    sheet.configure(configured);
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "custom", "{body}");
    assert_eq!(body["drawing_sheet"]["configured_path"], configured);
    assert_box(
        &body["drawing_area"]["frame"],
        [17.0, 17.0, 280.0022, 193.0072],
    );
    assert_box(
        &body["drawing_area"]["title_block"],
        [132.0022, 155.0072, 280.0022, 193.0072],
    );
    assert_complete(&body, 3, false);
    assert_eq!(body["findings"].as_array().unwrap().len(), 2, "{body}");
    let left = finding(&body, "af49ab94-c590-447a-bc71-da303ed76d53");
    near(&left["frame_overflow_mm"]["left"], 2.6737);
    assert_eq!(left["title_block_intrusion"], false);
    let inside = finding(&body, "6c4ada6a-f488-4955-9317-fde3702de0a8");
    assert_eq!(inside["title_block_intrusion"], true);
}

#[tokio::test]
async fn bare_custom_worksheet_name_uses_project_geometry() {
    custom_geometry(MARKED).await;
}

#[tokio::test]
async fn kiprjmod_custom_worksheet_name_uses_project_geometry() {
    custom_geometry(&format!("${{KIPRJMOD}}/{MARKED}")).await;
}

#[tokio::test]
async fn an_unmarked_custom_title_block_is_not_guessed() {
    let sheet = Sheet::new(CUSTOM).await;
    copy_fixture(sheet.dir.path(), UNMARKED);
    sheet.configure(UNMARKED);
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "custom");
    assert_unavailable(&body, "unrecognized_title_block");
}

#[tokio::test]
async fn a_missing_custom_worksheet_does_not_fall_back() {
    let sheet = Sheet::new(CUSTOM).await;
    sheet.configure(MARKED);
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "unresolved");
    assert_eq!(body["drawing_sheet"]["reason"], "unresolved_worksheet_path");
    assert_eq!(body["drawing_sheet"]["configured_path"], MARKED);
    assert_unavailable(&body, "unresolved_worksheet_path");
}

#[tokio::test]
async fn a_child_uses_the_owning_projects_worksheet_not_its_own_directory() {
    let sheet = Sheet::new(COVERAGE).await;
    let nested = sheet.dir.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let child = copy_fixture(&nested, CUSTOM);
    sheet
        .setup(
            "add_hierarchical_sheet",
            json!({
                "sheet_name": "Child", "sheet_file": format!("nested/{CUSTOM}"),
                "x": 50.8, "y": 30.48, "width": 25.4, "height": 15.24
            }),
        )
        .await;
    copy_fixture(sheet.dir.path(), MARKED);
    sheet.configure(&format!("${{KIPRJMOD}}\\{MARKED}"));
    let result = sheet.call(&child).await;
    let body = body_of(&result);
    assert!(!result.is_error, "{body}");
    assert_eq!(body["drawing_sheet"]["kind"], "custom");
    assert_complete(&body, 3, false);
    assert_eq!(body["findings"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_project_json_does_not_authorize_the_builtin_geometry() {
    let sheet = Sheet::new(CUSTOM).await;
    fs::write(sheet.path.with_extension("kicad_pro"), "{").unwrap();
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "unresolved");
    assert_unavailable(&body, "unreadable_project");
}

#[tokio::test]
async fn an_ordinary_symbol_label_and_no_connect_sheet_can_fit() {
    let body = Sheet::new(COVERAGE).await.report().await;
    assert_complete(&body, 4, true);
    assert_eq!(body["findings"], json!([]));
}

#[tokio::test]
async fn the_coverage_fixture_retains_its_legacy_answers() {
    // Observed through the same served tools on upstream main at fd58bfb1.
    let sheet = Sheet::new(COVERAGE).await;
    let pins = sheet
        .setup("get_schematic_pin_locations", json!({"reference": "R1"}))
        .await;
    assert_eq!(
        pins["pins"],
        json!([
            {"number": "1", "name": "", "unit": 1, "x": 101.6, "y": 85.09,
             "orientation_degrees": 90.0, "length_mm": 1.27},
            {"number": "2", "name": "", "unit": 1, "x": 101.6, "y": 92.71,
             "orientation_degrees": 270.0, "length_mm": 1.27}
        ])
    );
    for (pin, y, net) in [("1", 85.09, json!("SIG_A")), ("2", 92.71, Value::Null)] {
        let actual = sheet
            .setup(
                "get_pin_net_name",
                json!({"reference": "R1", "pin_number": pin}),
            )
            .await;
        assert_eq!(
            actual,
            json!({"reference": "R1", "pin": pin, "pin_x": 101.6, "pin_y": y, "net": net})
        );
    }
    let layout = sheet.setup("get_schematic_layout", json!({})).await;
    assert_eq!(layout["bounds_resolved"], 1);
    assert_eq!(layout["bounds_unresolved"], json!([]));
    assert_box(&layout["bounding_box"], [100.584, 85.09, 102.616, 92.71]);
    assert_eq!(
        sheet.setup("check_schematic_overlaps", json!({})).await,
        json!({"overlap_count": 0, "overlaps": [], "bounds_resolved": 1, "bounds_unresolved": []})
    );

    // Add positive witnesses for the legacy text and shape readers. Neither
    // crossing wire touches a pin or joins the named net.
    for [y1, y2] in [[86.36, 91.44], [74.93, 78.74]] {
        sheet
            .setup(
                "add_wire",
                json!({"x1": 104.14, "x2": 104.14, "y1": y1, "y2": y2}),
            )
            .await;
    }
    let overlaps = sheet.setup("check_schematic_overlaps", json!({})).await;
    assert_eq!(overlaps["overlap_count"], 2, "{overlaps}");
    let rows = overlaps["overlaps"].as_array().unwrap();
    let field = rows
        .iter()
        .find(|row| row["type"] == "wire_field_overlap")
        .unwrap();
    assert_eq!(field["b"]["reference"], "R1");
    assert_eq!(field["b"]["field"], "Reference");
    assert_box(
        &field["bounds_b"],
        [102.865401, 87.825943, 104.287801, 89.913581],
    );
    let label = rows
        .iter()
        .find(|row| row["type"] == "wire_label_overlap")
        .unwrap();
    assert_eq!(label["b"]["net"], "SIG_A");
    assert_box(
        &label["bounds_b"],
        [101.865968, 76.881201, 106.795987, 78.424554],
    );

    sheet
        .setup(
            "add_schematic_shapes",
            json!({"shapes": [
                {"type":"rect", "start":{"x":10.16,"y":30.48}, "end":{"x":20.32,"y":40.64}},
                {"type":"poly", "points":[{"x":50.8,"y":10.16},{"x":50.8,"y":20.32},{"x":60.96,"y":20.32}]}
            ]}),
        )
        .await;
    let shapes = sheet.setup("list_schematic_shapes", json!({})).await;
    assert_eq!(shapes["count"], 2, "{shapes}");
    assert_eq!(shapes["shapes"][0]["type"], "rect");
    assert_eq!(shapes["shapes"][0]["start"], json!({"x":10.16,"y":30.48}));
    assert_eq!(shapes["shapes"][0]["end"], json!({"x":20.32,"y":40.64}));
    assert_eq!(shapes["shapes"][1]["type"], "poly");
    assert_eq!(
        shapes["shapes"][1]["points"],
        json!([{"x":50.8,"y":10.16},{"x":50.8,"y":20.32},{"x":60.96,"y":20.32}])
    );
}

#[test]
fn label_fields_follow_only_global_label_offsets() {
    let offset = Ok((1.5, 0.3));
    assert_eq!(label_field_offset(LabelKind::GlobalLabel, offset), offset);
    assert_eq!(
        label_field_offset(LabelKind::NetLabel, offset),
        Ok((0.0, 0.0))
    );
    assert_eq!(
        label_field_offset(LabelKind::HierarchicalLabel, offset),
        Ok((0.0, 0.0))
    );
}

#[tokio::test]
async fn a_missing_schematic_is_a_file_error_not_a_clear_verdict() {
    let sheet = Sheet::new(COVERAGE).await;
    let result = sheet
        .call(&sheet.dir.path().join("missing.kicad_sch"))
        .await;
    let body = body_of(&result);
    assert!(result.is_error);
    assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    assert!(body.get("fits").is_none());
}

#[tokio::test]
async fn saved_channel_references_are_not_assumed_to_match_every_placement() {
    let mut sheet = Sheet::new("multichannel_channel_strip.kicad_sch").await;
    let channel = sheet.dir.path().join("channel_strip.kicad_sch");
    fs::rename(&sheet.path, &channel).unwrap();
    sheet.path = channel;
    copy_fixture(sheet.dir.path(), "multichannel_mixer.kicad_sch");
    copy_fixture(sheet.dir.path(), "multichannel_mixer.kicad_pro");
    let body = sheet.report().await;
    assert_eq!(body["drawing_sheet"]["kind"], "builtin", "{body}");
    assert_eq!(body["coverage_complete"], false);
    assert_ne!(body["fits"], true);
    assert!(
        body["unmeasured"].as_array().unwrap().iter().any(|row| {
            row["kind"] == "symbol"
                && (row["reason"] == "per_instance_reference_not_measured"
                    || row["reasons"].as_array().is_some_and(|reasons| {
                        reasons.contains(&json!("per_instance_reference_not_measured"))
                    }))
        }),
        "{body}"
    );
}

#[tokio::test]
async fn no_connect_ink_can_overflow_even_when_its_anchor_fits() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup("add_no_connect", json!({"x": 12.5, "y": 30.48}))
        .await;
    let body = sheet.report().await;
    assert_complete(&body, 5, false);
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["kind"], "no_connect");
    near(&rows[0]["frame_overflow_mm"]["left"], 0.1858);
}

#[tokio::test]
async fn sheet_name_can_cross_the_frame_without_the_sheet_box_crossing() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup(
            "add_hierarchical_sheet",
            json!({
                "sheet_name": "Controller", "sheet_file": "child.kicad_sch",
                "x": 50.8, "y": 12.7, "width": 25.4, "height": 15.24
            }),
        )
        .await;
    let body = sheet.report().await;
    assert_eq!(body["fits"], false, "{body}");
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["kind"], "sheet");
    near(&rows[0]["frame_overflow_mm"]["top"], 1.4447);
    sheet
        .setup(
            "edit_sheet",
            json!({"sheet_name": "Controller", "hide_name": true}),
        )
        .await;
    assert_complete(&sheet.report().await, 5, true);
}

#[tokio::test]
async fn a_bus_entry_stroke_can_overflow_while_its_segment_fits() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup("add_bus_entry", json!({"x": 12.05, "y": 30.48}))
        .await;
    let body = sheet.report().await;
    assert_complete(&body, 5, false);
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["kind"], "bus_entry");
    near(&rows[0]["frame_overflow_mm"]["left"], 0.0262);
}

#[tokio::test]
async fn a_circles_bounding_box_can_reach_the_title_block_without_any_ink_there() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup(
            "add_schematic_shapes",
            json!({"shapes": [{
                "type": "circle", "center": {"x": 175.0022, "y": 164.0072},
                "radius_mm": 2.5,
            }]}),
        )
        .await;
    let body = sheet.report().await;
    assert_complete(&body, 5, true);
    assert_eq!(body["findings"], json!([]));
}

#[tokio::test]
async fn a_dashed_outline_does_not_prove_an_intrusion_at_the_title_block() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup(
            "add_schematic_shapes",
            json!({"shapes":[{
                "type":"line", "stroke_type":"dash",
                "start":{"x":170.0,"y":170.0}, "end":{"x":182.0,"y":170.0}
            }]}),
        )
        .await;
    let body = sheet.report().await;
    assert_eq!(body["fits"], Value::Null, "{body}");
    assert_eq!(body["findings"], json!([]), "{body}");
    assert_eq!(
        body["unmeasured"][0]["reason"],
        "patterned_geometry_not_measured"
    );
}

#[test]
fn a_mismatched_unit_keeps_violations_from_common_graphics() {
    let lib = parse_sexp(
        r#"(symbol "Test:U"
        (symbol "U_0_1" (rectangle (start -1 1) (end 1 -1)))
        (symbol "U_1_1" (rectangle (start 200 1) (end 500 -1))))"#,
    )
    .unwrap();
    let symbol = parse_sexp(
        r#"(symbol (lib_id "Test:U") (at 12.5 25 0) (unit 1)
        (property "Reference" "U1" (hide yes))
        (instances (project "p" (path "/root" (reference "U1") (unit 2)))))"#,
    )
    .unwrap();
    let area = DrawingArea::builtin((297.0, 210.0)).unwrap();
    let mut measured = Measurement {
        frame: area.frame,
        ..Default::default()
    };
    measure_symbol(
        &symbol,
        &[&lib],
        from_bbox(area.title_block),
        &PlotSettings::default(),
        &mut measured,
    )
    .unwrap();
    assert_eq!(measured.missing, ["per_instance_unit_not_measured"]);
    let [left, _, right, _] = frame_overflow(measured.bounds.unwrap(), area.frame);
    assert!((left - 0.5762).abs() < 1e-6);
    assert_eq!(right, 0.0, "must exclude the wrong unit's artwork");
}

#[tokio::test]
async fn a_hierarchical_flag_can_overflow_even_when_its_text_is_unmeasured() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup(
            "add_schematic_net_label",
            json!({
                "net": "${ALIAS}", "label_type": "hierarchical_label",
                "x": 12.7, "y": 30.48, "rotation": 180.0
            }),
        )
        .await;
    let body = sheet.report().await;
    assert_eq!(body["fits"], false, "{body}");
    assert_eq!(body["coverage_complete"], false);
    let missing = body["unmeasured"].as_array().unwrap();
    let findings = body["findings"].as_array().unwrap();
    assert_eq!(missing.len(), 1, "{body}");
    assert_eq!(findings.len(), 1, "{body}");
    assert_eq!(missing[0]["reason"], "unresolved_text_variable");
    assert_eq!(missing[0]["uuid"], findings[0]["uuid"]);
    near(&findings[0]["frame_overflow_mm"]["left"], 0.6462);
}

#[tokio::test]
async fn a_nonnative_overbar_offset_is_not_silently_checked_with_default_metrics() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup(
            "add_schematic_net_label",
            json!({
                "net": "~{SIGNAL}", "x": 50.8, "y": 30.48
            }),
        )
        .await;
    assert_complete(&sheet.report().await, 5, true);
    fs::write(
        sheet.path.with_extension("kicad_pro"),
        json!({"schematic":{"drawing":{"overbar_offset_ratio": 2.0}}}).to_string(),
    )
    .unwrap();
    let body = sheet.report().await;
    assert_eq!(body["fits"], Value::Null);
    let rows = body["unmeasured"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["reason"], "project_overbar_offset_not_measured");
}

#[tokio::test]
async fn project_junction_size_can_make_a_dot_cross_the_frame() {
    let sheet = Sheet::new(COVERAGE).await;
    sheet
        .setup("add_junction", json!({"x": 12.7, "y": 30.48}))
        .await;
    assert_complete(&sheet.report().await, 5, true);
    fs::write(
        sheet.path.with_extension("kicad_pro"),
        json!({"schematic":{"drawing":{"junction_size_choice": 5}}}).to_string(),
    )
    .unwrap();
    let body = sheet.report().await;
    assert_complete(&body, 5, false);
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["kind"], "junction");
    near(&rows[0]["frame_overflow_mm"]["left"], 0.2144);
}

#[tokio::test]
async fn unsupported_notes_prevent_an_unqualified_clear_verdict() {
    let sheet = Sheet::new("schematic_note_bounds_kicad10.kicad_sch").await;
    let body = sheet.report().await;
    assert_eq!(body["findings"], json!([]), "{body}");
    assert_eq!(body["coverage_complete"], false);
    assert_eq!(body["fits"], Value::Null);
    let missed = body["unmeasured"].as_array().unwrap();
    assert_eq!(missed.len(), 4, "{body}");
    assert!(missed
        .iter()
        .any(|item| item["reason"] == "unresolved_text_variable"));
}

#[tokio::test]
async fn a_confirmed_overflow_still_fails_with_unmeasured_items() {
    let sheet = Sheet::new("schematic_note_bounds_kicad10.kicad_sch").await;
    let added = served_call(
        &sheet.handler,
        "add_schematic_shapes",
        json!({
            "schematic": sheet.path,
            "shapes": [
                {"type":"rect", "start":{"x":10.16,"y":30.48}, "end":{"x":20.32,"y":40.64}}
            ]
        }),
    )
    .await;
    assert!(!added.is_error, "{}", body_of(&added));
    let body = sheet.report().await;
    assert_eq!(body["fits"], false, "{body}");
    assert_eq!(body["coverage_complete"], false);
    assert_eq!(body["unmeasured"].as_array().unwrap().len(), 4);
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["kind"], "rectangle");
    near(&rows[0]["frame_overflow_mm"]["left"], 1.9162);
}

#[tokio::test]
async fn rectangle_and_polyline_from_the_shape_writer_can_cross_the_frame() {
    let sheet = Sheet::new(COVERAGE).await;
    let added = served_call(&sheet.handler, "add_schematic_shapes", json!({
        "schematic": sheet.path,
        "shapes": [
            {"type":"rect", "start":{"x":10.16,"y":30.48}, "end":{"x":20.32,"y":40.64}},
            {"type":"poly", "points":[{"x":50.8,"y":10.16},{"x":50.8,"y":20.32},{"x":60.96,"y":20.32}]}
        ]
    })).await;
    assert!(!added.is_error, "{}", body_of(&added));
    let body = sheet.report().await;
    assert_complete(&body, 6, false);
    let rows = body["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{body}");
    for (tag, side) in [("rectangle", "left"), ("polyline", "top")] {
        let item = rows
            .iter()
            .find(|item| item["kind"] == tag)
            .unwrap_or_else(|| panic!("{tag}: {body}"));
        near(&item["frame_overflow_mm"][side], 1.9162);
        assert_eq!(item["title_block_intrusion"], false);
    }
}

#[tokio::test]
async fn a_non_schematic_file_does_not_get_a_verdict() {
    let sheet = Sheet::new(CUSTOM).await;
    let result = sheet.call(&copy_fixture(sheet.dir.path(), MARKED)).await;
    let body = body_of(&result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "schematic");
    assert!(body.get("fits").is_none());
}

#[tokio::test]
async fn a_malformed_s_expression_does_not_get_a_verdict() {
    let sheet = Sheet::new(CUSTOM).await;
    let invalid = sheet.dir.path().join("truncated.txt");
    fs::write(&invalid, "(kicad_sch").unwrap();
    let result = sheet.call(&invalid).await;
    let body = body_of(&result);
    assert!(result.is_error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "schematic");
    assert!(body.get("fits").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn a_schematic_alias_does_not_hide_its_project_worksheet() {
    let sheet = Sheet::new(CUSTOM).await;
    copy_fixture(sheet.dir.path(), MARKED);
    sheet.configure(MARKED);
    let other = tempfile::tempdir().unwrap();
    let alias = other.path().join("alias.kicad_sch");
    std::os::unix::fs::symlink(&sheet.path, &alias).unwrap();
    let result = sheet.call(&alias).await;
    let body = body_of(&result);
    assert!(!result.is_error, "{body}");
    assert_eq!(body["drawing_sheet"]["kind"], "custom");
    assert_complete(&body, 3, false);
    assert_eq!(body["findings"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(fs::read_link(&alias).unwrap(), sheet.path);
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 1);
}
