use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::sch_wiring::insert_before_close;
use crate::tools::ServerConfig;
use copperpour_sexp::schematic::{format_wire, symbol_instance};
use std::path::PathBuf;

#[path = "sch_cleanup_tests/native.rs"]
mod native;

const CROWDED: &str =
    include_str!("../../tests/fixtures/region_cleanup_kicad10/crowded/crowded.kicad_sch");
const CONSTRAINED: &str =
    include_str!("../../tests/fixtures/region_cleanup_kicad10/constrained/constrained.kicad_sch");
const SPARSE: &str =
    include_str!("../../tests/fixtures/region_cleanup_kicad10/sparse/sparse.kicad_sch");
const FIELDS: &str =
    include_str!("../../tests/fixtures/field_placement_kicad10/field_placement.kicad_sch");
const TOOL: &str = "plan_schematic_cleanup";

struct Preview {
    _dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
    original: String,
}

impl Preview {
    async fn new(source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sheet.kicad_sch");
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
            original: source.to_owned(),
        }
    }

    async fn call(&self, name: &str, mut args: Value) -> CallToolResult {
        args["schematic"] = json!(self.path);
        let result = served_call(&self.handler, name, args).await;
        assert_eq!(std::fs::read_to_string(&self.path).unwrap(), self.original);
        assert_eq!(
            std::fs::read_dir(self.path.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        result
    }

    async fn ok(&self, name: &str, args: Value) -> Value {
        let result = self.call(name, args).await;
        assert!(!result.is_error, "{name}: {}", text_of(&result));
        serde_json::from_str(text_of(&result)).unwrap()
    }

    async fn plan(&self, movable: &[&str]) -> Value {
        self.ok(
            TOOL,
            json!({
                "x1": 0, "y1": 0, "x2": 200, "y2": 200, "movable": movable
            }),
        )
        .await
    }
}

fn array<'a>(response: &'a Value, key: &str) -> &'a [Value] {
    response[key].as_array().unwrap()
}

fn skipped_reason<'a>(response: &'a Value, reference: &str) -> &'a str {
    array(response, "skipped")
        .iter()
        .find(|item| item["reference"] == reference)
        .unwrap_or_else(|| panic!("{reference} missing from {response}"))["reason"]
        .as_str()
        .unwrap()
}

fn step<'a>(response: &'a Value, kind: &str) -> &'a Value {
    array(response, "steps")
        .iter()
        .find(|step| step["kind"] == kind)
        .unwrap_or_else(|| panic!("{kind} missing from {response}"))
}

#[tokio::test]
async fn crowded_reroutes_only_and_reports_the_saved_revision() {
    let preview = Preview::new(CROWDED).await;
    let result = preview
        .ok(TOOL, json!({"x1": 200, "y1": 200, "x2": 0, "y2": 0}))
        .await;
    assert_eq!(result, preview.plan(&[]).await);
    assert_eq!(
        result["revision"],
        preview.ok("list_schematic_texts", json!({})).await["revision"]
    );
    assert_eq!(result["source"], "saved_schematic");
    assert_eq!(array(&result, "steps").len(), 1, "{result}");
    let reroute = step(&result, "reroute");
    assert_eq!(
        reroute["route"],
        json!([[63.5, 105.41], [119.38, 105.41], [119.38, 76.2]])
    );
    assert_eq!(reroute["length_before_mm"], 110.49);
    assert_eq!(reroute["length_after_mm"], 85.09);
    assert_eq!(array(reroute, "wires_removed").len(), 6);
    assert_eq!(reroute["symbols_moved"], json!([]));
    assert!(
        !array(&result, "skipped")
            .iter()
            .any(|item| item["run"] == "C1 pin 1 to U1 pin 3"),
        "{result}"
    );
}

fn crowded_with_near_miss_wire() -> String {
    assert_eq!(CROWDED.matches("(xy 63.5 105.41)").count(), 1);
    CROWDED.replacen("(xy 63.5 105.41)", "(xy 63.5004 105.41)", 1)
}

#[tokio::test]
async fn a_near_miss_wire_endpoint_does_not_connect_a_pin() {
    let source = crowded_with_near_miss_wire();
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");

    // The route attachment guard may also catch this. Exercise the net gate
    // independently by replaying the valid plan from the unperturbed sheet.
    let normal_plan = Preview::new(CROWDED).await.plan(&[]).await;
    let connected = native::replay(&source, &normal_plan);
    let before = parse_sexp(&source).unwrap();
    let after = parse_sexp(&connected).unwrap();
    let findings = sch_layout_score::cleanup_gate(&before, &after, &[]);
    assert!(
        findings.iter().any(|reason| reason.contains("C1 pin 1")),
        "{findings:?}"
    );
}

#[tokio::test]
async fn a_reroute_cannot_add_a_tee_into_a_bare_wire() {
    let source = insert_before_close(CROWDED, &format_wire(101.6, 105.41, 101.6, 111.76));
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_ne!(
        array(&result, "steps").first().map(|s| &s["route"]),
        Some(&json!([[63.5, 105.41], [119.38, 105.41], [119.38, 76.2]])),
        "{result}"
    );
}

#[tokio::test]
async fn a_moved_route_tip_cannot_land_on_a_bare_wire() {
    let source = insert_before_close(CROWDED, &format_wire(114.3, 74.93, 114.3, 77.47));
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let reroute = step(&result, "reroute");
    assert_ne!(
        reroute["route"],
        json!([[114.3, 76.2], [119.38, 76.2]]),
        "{result}"
    );
    assert!(
        !array(reroute, "junctions_added").contains(&json!([114.3, 76.2])),
        "{result}"
    );
}

#[tokio::test]
async fn a_carried_power_pin_cannot_land_on_a_bare_wire_off_the_route() {
    let source = insert_before_close(CROWDED, &format_wire(111.76, 83.82, 114.3, 83.82));
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let reroute = step(&result, "reroute");
    assert!(
        !array(reroute, "symbols_moved")
            .iter()
            .any(|m| { m["reference"] == "#PWR05" && m["to"] == json!([114.3, 83.82]) }),
        "{result}"
    );
}

#[test]
fn a_same_net_label_is_still_a_new_mid_run_attachment() {
    let source = insert_before_close(
        CROWDED,
        r#"(global_label "+5V" (shape input) (at 101.6 105.41 0)
            (effects (font (size 1.27 1.27))) (uuid "00000000-0000-4000-8000-000000000108"))"#,
    );
    let tree = parse_sexp(&source).unwrap();
    let sheet = Sheet::new(&tree).unwrap();
    let index = sheet.index();
    let run = sheet
        .chains(&index)
        .into_iter()
        .find(|run| run.indices.len() == 6)
        .unwrap();
    let edit = Edit::new(&sheet, &run.indices);
    let (stripped, _, _) = edit.prepare(&source, &tree).unwrap();
    let base = parse_sexp(&stripped).unwrap();
    let remaining = Sheet::new(&base).unwrap();
    assert_eq!(
        new_route_attachment(
            &remaining,
            &[(63.5, 105.41), (119.38, 105.41), (119.38, 76.2)],
            &run.ends,
            &edit,
        ),
        Some("a global_label".into()),
    );
}

#[tokio::test]
async fn crowded_placement_beats_the_reroute_and_carries_ground() {
    let result = Preview::new(CROWDED).await.plan(&["C1"]).await;
    assert_eq!(array(&result, "steps").len(), 1, "{result}");
    let reroute = step(&result, "reroute");
    assert_eq!(reroute["route"], json!([[114.3, 76.2], [119.38, 76.2]]));
    assert_eq!(reroute["length_after_mm"], 5.08);
    assert_eq!(reroute["junctions_added"], json!([[119.38, 76.2]]));
    let moves = array(reroute, "symbols_moved");
    assert_eq!(moves.len(), 2);
    for (reference, to) in [("C1", [114.3, 80.01]), ("#PWR05", [114.3, 83.82])] {
        let m = moves.iter().find(|m| m["reference"] == reference).unwrap();
        assert_eq!(m["to"], json!(to), "{m}");
        assert_eq!(m["dx"], 50.8);
        assert_eq!(m["dy"], -29.21);
    }
}

#[tokio::test]
async fn a_connected_pin_cannot_take_ownership_of_a_dangling_no_connect() {
    let source = insert_before_close(
        CROWDED,
        r#"(no_connect (at 114.3 76.2) (uuid "00000000-0000-4000-8000-000000000100"))"#,
    );
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let replayed = native::replay(&source, &result);
    let tree = parse_sexp(&replayed).unwrap();
    let sheet = Sheet::new(&tree).unwrap();
    assert!(!sheet.index().has_pin(114.3, 76.2), "{result}");

    // Independently exercise the ownership check with the otherwise-valid
    // placement; the route guards can also refuse this marker.
    let normal_plan = Preview::new(CROWDED).await.plan(&["C1"]).await;
    let connected = native::replay(&source, &normal_plan);
    let before = parse_sexp(&source).unwrap();
    let after = parse_sexp(&connected).unwrap();
    assert!(
        no_connect_change(&Sheet::new(&before).unwrap(), &Sheet::new(&after).unwrap()).is_some()
    );
}

#[tokio::test]
async fn a_reroute_cannot_wire_through_an_unowned_no_connect() {
    // Neither elbow can be accepted: one crosses the marker and the other
    // crosses a symbol field. The marker lies off the original run.
    let source = insert_before_close(
        CROWDED,
        r#"(no_connect (at 101.6 105.41) (uuid "00000000-0000-4000-8000-000000000107"))"#,
    );
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        array(&result, "skipped").iter().any(|item| item["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("would change what no-connect"))),
        "{result}"
    );
}

#[tokio::test]
async fn a_carried_contact_cannot_pick_up_a_nearby_pin() {
    // C1.2 and its carried ground already touch. The third pin is visually
    // coincident with their closest landing, but is electrically separate.
    let source = with_library_symbol(
        CROWDED,
        r#"(symbol "Test:Tip" (pin passive line (at 0 0 0) (length 0.5)
            (name "") (number "1")))"#,
    );
    let source = insert_before_close(
        &source,
        r#"(symbol (lib_id "Test:Tip") (at 114.304 83.82 0) (unit 1)
            (property "Reference" "TP99" (at 114.304 83.82 0) (hide yes))
            (uuid "00000000-0000-4000-8000-000000000107"))"#,
    );
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let reroute = step(&result, "reroute");
    assert_ne!(
        reroute["route"],
        json!([[114.3, 76.2], [119.38, 76.2]]),
        "{result}"
    );
}

#[tokio::test]
async fn a_carried_power_contact_eligible_after_a_move_gets_a_stub_in_the_same_plan() {
    let args = json!({"x1":60, "y1":60, "x2":120, "y2":106, "movable":["C1", "#PWR05"]});
    let preview = Preview::new(CROWDED).await;
    let result = preview.ok(TOOL, args.clone()).await;
    let steps = array(&result, "steps");
    assert_eq!(steps.len(), 2, "{result}");
    assert_eq!(steps[0]["kind"], "reroute");
    assert_eq!(steps[0]["extends_outside_region"], true);
    let carry = array(&steps[0], "symbols_moved")
        .iter()
        .find(|m| m["reference"] == "#PWR05")
        .unwrap();
    assert_eq!(carry["from"], json!([63.5, 113.03]));
    assert_eq!(carry["to"], json!([114.3, 83.82]));
    assert_eq!(steps[1]["kind"], "power_stub");
    assert_eq!(steps[1]["route"], json!([[114.3, 83.82], [114.3, 86.36]]));
    assert_eq!(steps[1]["symbols_moved"][0]["uuid"], carry["uuid"]);
    assert_eq!(steps[1]["symbols_moved"][0]["from"], carry["to"]);

    let replayed = native::replay(CROWDED, &result);
    assert_eq!(
        Preview::new(&replayed).await.ok(TOOL, args).await["steps"],
        json!([])
    );
}

#[tokio::test]
async fn the_step_limit_reports_unfinished_cleanup_without_a_partial_plan() {
    let preview = Preview::new(CROWDED).await;
    let args = json!({"schematic":preview.path, "x1":60, "y1":60, "x2":120, "y2":106,
        "movable":["C1", "#PWR05"]});
    let refused = plan_cleanup(&args, 1).unwrap();
    assert!(refused.is_error);
    let body: Value = serde_json::from_str(text_of(&refused)).unwrap();
    assert_eq!(body["error"]["kind"], "plan_blocked");
    assert_eq!(
        body["error"]["reasons"],
        json!(["the region still has cleanup steps after 1 edits"])
    );
    assert!(body.get("steps").is_none());
    assert_eq!(std::fs::read_to_string(&preview.path).unwrap(), CROWDED);
    assert_eq!(std::fs::read_dir(preview._dir.path()).unwrap().count(), 1);

    let converged = plan_cleanup(&args, 2).unwrap();
    assert!(!converged.is_error, "{}", text_of(&converged));
    let body: Value = serde_json::from_str(text_of(&converged)).unwrap();
    assert_eq!(array(&body, "steps").len(), 2);
}

#[tokio::test]
async fn served_plans_are_repeatable_and_replay_to_a_no_op() {
    let redundant = with_redundant_power();
    let jogged = jogged_power_lead(FIELDS);
    let flag = flag_lead_with_blocked_horizontal_elbow();
    let marker = crowded_with_no_connect();
    for (source, movable) in [
        (CROWDED, &[][..]),
        (CROWDED, &["C1"][..]),
        (CROWDED, &["#PWR05"][..]),
        (FIELDS, &["#PWR03"][..]),
        (redundant.as_str(), &["#PWR99"][..]),
        (jogged.as_str(), &[][..]),
        (flag.as_str(), &["#FLG99"][..]),
        (marker.as_str(), &["C1"][..]),
    ] {
        let preview = Preview::new(source).await;
        let plan = preview.plan(movable).await;
        assert!(!array(&plan, "steps").is_empty());
        assert_eq!(preview.plan(movable).await, plan);
        let candidate = native::replay(source, &plan);
        assert_eq!(
            Preview::new(&candidate).await.plan(movable).await["steps"],
            json!([])
        );
    }
}

#[tokio::test]
async fn cleanup_prefers_the_better_score_to_the_shorter_run() {
    let source = insert_before_close(
        CROWDED,
        r#"(symbol (lib_id "Device:C") (at 113.03 80.01 90) (unit 1)
            (property "Reference" "C99" (at 113.03 80.01 0) (hide yes))
            (uuid "00000000-0000-4000-8000-000000000099"))"#,
    );
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let step = step(&result, "reroute");
    assert_eq!(
        step["route"],
        json!([[106.68, 76.2], [119.38, 76.2]]),
        "{result}"
    );
}

#[tokio::test]
async fn a_run_that_leaves_the_selection_is_not_a_candidate() {
    let result = Preview::new(CROWDED)
        .await
        .ok(
            TOOL,
            json!({
                "x1": 63.5, "y1": 64, "x2": 119.38, "y2": 118, "movable": ["C1"]
            }),
        )
        .await;
    assert_eq!(result["steps"], json!([]), "{result}");
}

#[test]
fn the_preservation_check_sees_a_pinless_label_join_even_if_the_power_name_wins() {
    let source = insert_before_close(
        CROWDED,
        r#"
        (label "AUX" (at 101.6 76.2 90)
            (effects (font (size 1.27 1.27)) (justify left bottom))
            (uuid "00000000-0000-4000-8000-000000000105"))"#,
    );
    let before = parse_sexp(&source).unwrap();
    let sheet = Sheet::new(&before).unwrap();
    let index = sheet.index();
    let run = sheet
        .chains(&index)
        .into_iter()
        .find(|r| r.indices.len() == 6)
        .unwrap();
    let mut edit = Edit::new(&sheet, &run.indices);
    for (reference, to) in [("C1", (101.6, 80.01)), ("#PWR05", (101.6, 83.82))] {
        let inst = sheet
            .placed
            .iter()
            .map(|(s, _)| s)
            .find(|s| s.reference == reference)
            .unwrap();
        edit.symbols
            .push((inst.uuid.clone().unwrap(), to, inst.rotation));
    }
    let (moved, _, _) = edit.prepare(&source, &before).unwrap();
    let base = parse_sexp(&moved).unwrap();
    let from = (101.6, 76.2);
    let to = (119.38, 76.2);
    let candidate =
        super::super::sch_wiring::draw_route_with_drawn_pins(&moved, &[from, to]).unwrap();
    assert!(RouteGuard::for_drawn_pins(&base, from, &[to])
        .unwrap()
        .refusal("straight route", &candidate)
        .unwrap()
        .is_none());
    let after = parse_sexp(&candidate).unwrap();
    assert!(super::super::sch_block_move::pin_net_changes(&before, &after).is_empty());
    let findings = sch_layout_score::cleanup_gate(&before, &after, &[]);
    assert!(
        findings.iter().any(|reason| reason.contains("label AUX")),
        "{findings:?}"
    );
}

#[tokio::test]
async fn shortest_elbow_below_two_grid_steps_is_skipped() {
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(SPARSE, "kicad_sch") {
        let block = &SPARSE[start..end];
        let node = parse_sexp(block).unwrap();
        match node.head() {
            Some("symbol") => match symbol_instance(&node).unwrap().reference.as_str() {
                "R1" => {}
                "R2" => {
                    let mut symbol =
                        cse::Symbol::from_sexp(&cse::sexp::parser::parse(block).unwrap()).unwrap();
                    symbol.move_to(38.735, 46.355);
                    edits.push(SexpEdit::replace(
                        start,
                        end,
                        cse::sexp::writer::write(&symbol.to_sexp()),
                    ));
                }
                _ => edits.push(SexpEdit::delete(start, end)),
            },
            Some("label" | "global_label" | "junction" | "wire") => {
                edits.push(SexpEdit::delete(start, end));
            }
            _ => {}
        }
    }
    let mut source = apply_edits(SPARSE.to_owned(), edits);
    for [a, b] in [
        [(38.1, 41.91), (38.1, 42.1)],
        [(38.1, 42.1), (35.56, 42.1)],
        [(35.56, 42.1), (35.56, 42.545)],
        [(35.56, 42.545), (38.735, 42.545)],
    ] {
        source = insert_before_close(&source, &format_wire(a.0, a.1, b.0, b.1));
    }
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        array(&result, "skipped").iter().any(|item| {
            item["reason"]
                .as_str()
                .is_some_and(|r| r.contains("less than 2.54 mm"))
        }),
        "{result}"
    );
}

#[tokio::test]
async fn the_region_selects_the_old_lead_not_the_stub_destination() {
    let power = Preview::new(FIELDS)
        .await
        .ok(
            TOOL,
            json!({
                "x1": 50.8, "y1": 124.46, "x2": 76.2, "y2": 124.46, "movable": ["#PWR03"]
            }),
        )
        .await;
    let stub = step(&power, "power_stub");
    assert_eq!(stub["route"], json!([[76.2, 124.46], [76.2, 121.92]]));
    assert_eq!(stub["extends_outside_region"], true);
    assert_eq!(stub["symbols_moved"][0]["to"], json!([76.2, 121.92]));
}

#[tokio::test]
async fn another_connection_prevents_moving_the_regulator() {
    let result = Preview::new(CROWDED).await.plan(&["U1"]).await;
    assert!(
        array(&result, "skipped").iter().any(|item| {
            item["reference"] == "U1"
                && item["reason"] == "U1 pin 2 also carries the run to C2 pin 1"
        }),
        "{result}"
    );
    assert_eq!(step(&result, "reroute")["symbols_moved"], json!([]));
}

#[tokio::test]
async fn constrained_elbows_name_both_foreign_joins() {
    let result = Preview::new(CONSTRAINED).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    let why = array(&result, "skipped")[0]["reason"].as_str().unwrap();
    assert!(
        why.contains("R2 pin 1") && why.contains("R3 pin 1") && why.contains("SIG_B"),
        "{why}"
    );
}

#[tokio::test]
async fn already_straight_sparse_sheet_does_not_get_a_step() {
    assert_eq!(
        Preview::new(SPARSE).await.plan(&[]).await["steps"],
        json!([])
    );
}

#[tokio::test]
async fn power_lead_shortens_and_the_cleaned_sheet_has_no_second_step() {
    let result = Preview::new(FIELDS).await.plan(&["#PWR03"]).await;
    let power = step(&result, "power_stub");
    assert_eq!(
        power["route"],
        json!([[76.2, 124.46], [76.2, 121.92]]),
        "{power}"
    );
    assert_eq!(power["length_before_mm"], 25.4);
    assert_eq!(power["length_after_mm"], 2.54);
    let moved = &power["symbols_moved"][0];
    let tree = parse_sexp(FIELDS).unwrap();
    let inst = tree
        .find_all("symbol")
        .into_iter()
        .filter_map(symbol_instance)
        .find(|s| s.reference == "#PWR03")
        .unwrap();
    assert_eq!(moved["uuid"], json!(inst.uuid));
    assert_eq!(moved["to"], json!([76.2, 121.92]));
    assert!(
        !array(&result, "skipped")
            .iter()
            .any(|item| item["reference"] == "#PWR03"),
        "{result}"
    );

    let (steps, _, candidate) = plan_with_candidate(
        FIELDS,
        Region::new(0.0, 0.0, 200.0, 200.0),
        &BTreeSet::from(["#PWR03".into()]),
        MAX_CLEANUP_STEPS,
    )
    .unwrap()
    .unwrap();
    assert_eq!(steps, array(&result, "steps"));
    assert_eq!(
        Preview::new(&candidate).await.plan(&["#PWR03"]).await["steps"],
        json!([])
    );
}

fn jogged_power_lead(source: &str) -> String {
    // Give the fixed-symbol reroute a clear path so it can compete with the
    // stub. The unmodified fixture deliberately puts U2's Reference in the way.
    let reference = "(property \"Reference\" \"U2\"\n\t\t\t(at 63.5 125.73 0)";
    assert_eq!(source.matches(reference).count(), 1);
    let clear = source.replacen(
        reference,
        "(property \"Reference\" \"U2\"\n\t\t\t(at 60.96 129.54 0)",
        1,
    );
    jog_power_wire(&clear)
}

fn jog_power_wire(source: &str) -> String {
    let (start, end) = find_direct_child_blocks(source, "kicad_sch")
        .into_iter()
        .find(|&(start, end)| source[start..end].contains("(xy 50.8 124.46) (xy 76.2 124.46)"))
        .unwrap();
    let mut source = apply_edits(source.to_owned(), vec![SexpEdit::delete(start, end)]);
    for (i, (a, b)) in [
        ((50.8, 124.46), (50.8, 119.38)),
        ((50.8, 119.38), (76.2, 119.38)),
        ((76.2, 119.38), (76.2, 124.46)),
    ]
    .into_iter()
    .enumerate()
    {
        source = insert_before_close(&source, &fixture_wire(a, b, 200 + i));
    }
    source
}

#[tokio::test]
async fn a_jogged_power_lead_can_reroute_with_the_symbol_fixed() {
    let source = jogged_power_lead(FIELDS);
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(array(&result, "steps").len(), 1, "{result}");
    let reroute = step(&result, "reroute");
    assert_eq!(reroute["route"], json!([[50.8, 124.46], [76.2, 124.46]]));
    assert_eq!(reroute["length_before_mm"], 35.56);
    assert_eq!(reroute["length_after_mm"], 25.4);
    assert_eq!(reroute["symbols_moved"], json!([]));
}

#[tokio::test]
async fn a_fixed_power_lead_cannot_shortcut_through_a_reference_field() {
    let source = jog_power_wire(FIELDS);
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        array(&result, "skipped").iter().any(|entry| entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("wire_field_overlap"))),
        "{result}"
    );
}

#[tokio::test]
async fn a_fixed_power_lead_with_a_shared_tip_is_not_rerouted() {
    for wire in [
        format_wire(50.8, 124.46, 45.72, 124.46),
        format_wire(76.2, 124.46, 71.12, 124.46),
    ] {
        let source = insert_before_close(&jogged_power_lead(FIELDS), &wire);
        let result = Preview::new(&source).await.plan(&[]).await;
        assert_eq!(result["steps"], json!([]), "{result}");
        assert!(
            array(&result, "skipped")
                .iter()
                .any(|reason| reason["reason"]
                    .as_str()
                    .is_some_and(|s| s.contains("power lead has another attachment"))),
            "{result}"
        );
    }
}

#[test]
fn a_turned_power_stub_keeps_the_by_pin_facing_with_or_without_a_mirror() {
    let tree = parse_sexp(FIELDS).unwrap();
    let sheet = Sheet::new(&tree).unwrap();
    let index = sheet.index();
    let pin = index
        .placed_pins()
        .iter()
        .find(|pin| pin.reference == "#PWR03")
        .unwrap();
    let power = &sheet.placed[pin.symbol_index].0;
    let tip = (76.2, 124.46);
    let expected = rotation_facing(&pin.pin, 0.0);
    assert_ne!(expected, 0.0);
    assert_eq!(power_placement(power, pin, tip, 0.0), (tip, expected));

    let mut mirrored = power.clone();
    mirrored.mirror_y = true;
    let (origin, rotation) = power_placement(&mirrored, pin, tip, 0.0);
    assert_ne!(rotation, expected);
    let mut transform = mirrored.pin_transform();
    (transform.comp_x, transform.comp_y, transform.rotation_deg) = (origin.0, origin.1, rotation);
    assert_eq!(schematic_pin_endpoint(&pin.pin, transform), tip);
    assert_eq!(
        copperpour_sexp::transform_direction(pin.pin.rotation, transform),
        0.0
    );
}

#[tokio::test]
async fn the_stub_and_fixed_power_reroute_compete_for_the_same_lead() {
    let source = jogged_power_lead(FIELDS);
    let preview = Preview::new(&source).await;
    let fixed = preview.plan(&[]).await;
    let listed = preview.plan(&["#PWR03"]).await;
    let fixed = step(&fixed, "reroute");
    assert_eq!(array(&listed, "steps").len(), 1, "{listed}");
    let stub = step(&listed, "power_stub");
    assert_eq!(stub["wires_removed"], fixed["wires_removed"]);
    assert_eq!(stub["route"], json!([[76.2, 124.46], [76.2, 121.92]]));
    assert_eq!(stub["symbols_moved"][0]["reference"], "#PWR03");
}

#[tokio::test]
async fn a_fixed_power_reroute_cannot_join_an_unlabelled_wire() {
    let source = jogged_power_lead(FIELDS);
    let source = insert_before_close(&source, &format_wire(63.5, 124.46, 63.5, 127.0));
    let result = Preview::new(&source).await.plan(&[]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        array(&result, "skipped").iter().any(|item| item["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("power lead would touch another wire"))),
        "{result}"
    );
}

fn flag_lead_with_blocked_horizontal_elbow() -> String {
    let source = without_symbol(jogged_power_lead(&with_flag_library(FIELDS)), "#PWR03");
    let source = add_flag(&source, 50.8, 125.73, 180);
    let source = insert_before_close(&source, &fixture_wire((50.8, 125.73), (50.8, 124.46), 203));
    insert_before_close(&source, &fixture_wire((63.5, 125.73), (63.5, 128.27), 204))
}

// Keep the input bytes and ERC item identities the same across native runs.
fn fixture_wire((x1, y1): Point, (x2, y2): Point, id: usize) -> String {
    format!(
        "(wire (pts (xy {x1} {y1}) (xy {x2} {y2})) (stroke (width 0) (type default)) \
        (uuid \"00000000-0000-4000-8000-{id:012}\"))"
    )
}

fn with_library_symbol(source: &str, symbol: &str) -> String {
    let (start, end) = find_direct_child_blocks(source, "kicad_sch")
        .into_iter()
        .find(|&(start, end)| source[start..end].starts_with("(lib_symbols"))
        .unwrap();
    apply_edits(
        source.to_owned(),
        vec![SexpEdit::replace(
            start,
            end,
            insert_before_close(&source[start..end], symbol),
        )],
    )
}

fn with_flag_library(source: &str) -> String {
    if source.contains(r#"(symbol "power:PWR_FLAG""#) {
        return source.to_owned();
    }
    let flag = cse::sexp::parser::parse(CROWDED)
        .unwrap()
        .find("lib_symbols")
        .unwrap()
        .find_all("symbol")
        .into_iter()
        .find(|symbol| symbol.value() == Some("power:PWR_FLAG"))
        .map(cse::sexp::writer::write)
        .unwrap();
    with_library_symbol(source, &flag)
}

fn add_flag(source: &str, x: f64, y: f64, angle: i32) -> String {
    insert_before_close(
        source,
        &format!(
            r##"(symbol (lib_id "power:PWR_FLAG") (at {x} {y} {angle}) (unit 1)
            (property "Reference" "#FLG99" (at {x} {y} 0) (hide yes))
            (property "Value" "PWR_FLAG" (at {x} {y} 0) (hide yes))
            (uuid "00000000-0000-4000-8000-000000000106"))"##
        ),
    )
}

#[tokio::test]
async fn a_flag_on_a_movable_symbol_pin_is_not_carried() {
    let source = without_symbol(CROWDED.to_owned(), "#PWR05");
    let source = add_flag(&source, 63.5, 113.03, 180);
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    assert_eq!(
        step(&result, "reroute")["symbols_moved"],
        json!([]),
        "{result}"
    );
    assert!(skipped_reason(&result, "C1").contains("#FLG99"), "{result}");
}

#[tokio::test]
async fn a_listed_flag_stays_fixed_and_its_lead_tries_the_second_elbow() {
    let source = flag_lead_with_blocked_horizontal_elbow();
    let result = Preview::new(&source).await.plan(&["#FLG99"]).await;
    assert_eq!(skipped_reason(&result, "#FLG99"), "PWR_FLAG never moves");
    assert_eq!(array(&result, "steps").len(), 1, "{result}");
    let reroute = step(&result, "reroute");
    assert_eq!(
        reroute["route"],
        json!([[50.8, 125.73], [50.8, 124.46], [76.2, 124.46]])
    );
    assert_eq!(reroute["length_after_mm"], 26.67);
    assert_eq!(reroute["symbols_moved"], json!([]));
}

#[tokio::test]
async fn zero_length_lead_gets_a_stub_that_removes_one_pin_contact() {
    let result = Preview::new(CROWDED).await.plan(&["#PWR05"]).await;
    let power = step(&result, "power_stub");
    assert_eq!(
        power["route"],
        json!([[63.5, 113.03], [63.5, 115.57]]),
        "{power}"
    );
    assert_eq!(power["wires_removed"], json!([]));
    assert_eq!(power["length_before_mm"], 0.0);
    let (_, _, candidate) = plan_with_candidate(
        CROWDED,
        Region::new(0.0, 0.0, 200.0, 200.0),
        &BTreeSet::from(["#PWR05".into()]),
        MAX_CLEANUP_STEPS,
    )
    .unwrap()
    .unwrap();
    let before = Preview::new(CROWDED)
        .await
        .ok("find_pin_contacts", json!({}))
        .await;
    let after = Preview::new(&candidate)
        .await
        .ok("find_pin_contacts", json!({}))
        .await;
    assert_eq!(before["contact_count"], 7);
    assert_eq!(after["contact_count"], 6);
}

#[tokio::test]
async fn shared_tip_does_not_move_the_only_power_symbol_on_the_node() {
    let result = Preview::new(CROWDED).await.plan(&["#PWR01"]).await;
    assert!(
        skipped_reason(&result, "#PWR01").contains("U1 pin 3"),
        "{result}"
    );
    assert_eq!(array(&result, "steps").len(), 1);
}

#[tokio::test]
async fn a_listed_power_flag_stays_put() {
    let result = Preview::new(CROWDED).await.plan(&["#FLG01"]).await;
    assert_eq!(skipped_reason(&result, "#FLG01"), "PWR_FLAG never moves");
    assert!(array(&result, "steps")
        .iter()
        .all(|step| step["symbols_moved"] == json!([])));
}

#[tokio::test]
async fn a_tap_in_the_power_lead_rules_out_a_power_move() {
    let source = insert_before_close(FIELDS, &format_wire(63.5, 124.46, 63.5, 121.92));
    let result = Preview::new(&source).await.plan(&["#PWR03"]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        skipped_reason(&result, "#PWR03").contains("tapped"),
        "{result}"
    );
}

#[tokio::test]
async fn an_entry_in_the_old_leads_interior_is_not_removed() {
    let source = insert_before_close(
        FIELDS,
        r#"
        (bus (pts (xy 55.88 121.92) (xy 66.04 121.92))
            (uuid "00000000-0000-4000-8000-000000000101"))
        (bus_entry (at 60.96 121.92) (size 2.54 2.54)
            (uuid "00000000-0000-4000-8000-000000000102"))"#,
    );
    let result = Preview::new(&source).await.plan(&["#PWR03"]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        skipped_reason(&result, "#PWR03").contains("tapped"),
        "{result}"
    );
}

#[tokio::test]
async fn a_stub_will_not_attach_to_the_wire_end_of_a_bus_entry() {
    let source = insert_before_close(
        FIELDS,
        r#"
        (bus (pts (xy 71.12 119.38) (xy 75.565 119.38))
            (uuid "00000000-0000-4000-8000-000000000101"))
        (bus_entry (at 73.66 119.38) (size 2.54 2.54)
            (uuid "00000000-0000-4000-8000-000000000102"))"#,
    );
    let result = Preview::new(&source).await.plan(&["#PWR03"]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert!(
        skipped_reason(&result, "#PWR03").contains("bus entry"),
        "{result}"
    );
}

#[tokio::test]
async fn a_power_stub_cannot_end_on_a_bare_wire() {
    let source = insert_before_close(FIELDS, &format_wire(74.93, 121.92, 77.47, 121.92));
    let result = Preview::new(&source).await.plan(&["#PWR03"]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert_eq!(
        skipped_reason(&result, "#PWR03"),
        "the power stub would touch another wire or bus"
    );
}

#[tokio::test]
async fn a_power_stub_refuses_a_second_symbol_on_the_fixed_tip() {
    let source = add_flag(&with_flag_library(FIELDS), 76.2, 124.46, 180);
    let result = Preview::new(&source).await.plan(&["#PWR03"]).await;
    assert_eq!(result["steps"], json!([]), "{result}");
    assert_eq!(
        skipped_reason(&result, "#PWR03"),
        "U2 pin 8 also touches another symbol"
    );
}

#[tokio::test]
async fn an_elbow_does_not_attach_to_a_bus_entry() {
    let source = insert_before_close(
        CROWDED,
        r#"
        (bus (pts (xy 76.2 102.87) (xy 81.28 102.87))
            (uuid "00000000-0000-4000-8000-000000000101"))
        (bus_entry (at 78.74 102.87) (size 2.54 2.54)
            (uuid "00000000-0000-4000-8000-000000000102"))"#,
    );
    let result = Preview::new(&source).await.plan(&[]).await;
    // The other elbow crosses a symbol field in the crowded fixture.
    assert_eq!(result["steps"], json!([]), "{result}");
    let run = array(&result, "skipped")
        .iter()
        .find(|entry| entry["run"] == "C1 pin 1 to U1 pin 3")
        .unwrap();
    assert!(
        run["reason"].as_str().unwrap().contains("bus entry"),
        "{run}"
    );

    // The generic attachment check also sees the wire end of the bus entry.
    // Check the bus guard on its own, before the generic check can refuse.
    let tree = parse_sexp(&source).unwrap();
    let sheet = Sheet::new(&tree).unwrap();
    let horizontal = [(63.5, 105.41), (119.38, 105.41), (119.38, 76.2)];
    let vertical = [(63.5, 105.41), (63.5, 76.2), (119.38, 76.2)];
    assert!(route_bus_obstruction(&sheet, &horizontal).is_some());
    assert!(route_bus_obstruction(&sheet, &vertical).is_none());
}

#[tokio::test]
async fn a_reroute_unblocks_an_earlier_run_in_the_same_preview() {
    let edits = find_direct_child_blocks(SPARSE, "kicad_sch")
        .into_iter()
        .filter(|&(start, end)| {
            matches!(
                parse_sexp(&SPARSE[start..end]).unwrap().head(),
                Some("symbol" | "wire" | "junction" | "label" | "global_label")
            )
        })
        .map(|(start, end)| SexpEdit::delete(start, end))
        .collect();
    let mut source = apply_edits(SPARSE.into(), edits);
    for (i, (x, y)) in [
        (40.0, 36.19),
        (65.0, 63.81),
        (50.0, 26.19),
        (55.0, 53.81),
        (40.0, 49.19),
    ]
    .into_iter()
    .enumerate()
    {
        source = insert_before_close(
            &source,
            &format!(
                "(symbol (lib_id \"Device:R\") (at {x} {y} 0) (unit 1) \
                 (property \"Reference\" \"R{}\" (at {x} {y} 0) (hide yes)) \
                 (uuid \"00000000-0000-4000-8000-{i:012}\"))",
                i + 1
            ),
        );
    }
    for points in [
        &[
            (40.0, 40.0),
            (40.0, 43.0),
            (48.0, 43.0),
            (48.0, 62.0),
            (62.0, 62.0),
            (62.0, 60.0),
            (65.0, 60.0),
        ][..],
        &[
            (50.0, 30.0),
            (58.0, 30.0),
            (58.0, 40.0),
            (55.0, 40.0),
            (55.0, 50.0),
        ][..],
    ] {
        for leg in points.windows(2) {
            source = insert_before_close(
                &source,
                &format_wire(leg[0].0, leg[0].1, leg[1].0, leg[1].1),
            );
        }
    }
    let preview = Preview::new(&source).await;
    let result = preview.plan(&[]).await;
    let steps = array(&result, "steps");
    assert_eq!(steps.len(), 2, "{result}");
    assert_eq!(steps[0]["route"][0], json!([50.0, 30.0]), "{result}");
    assert_eq!(
        steps[1]["route"],
        json!([[40.0, 40.0], [65.0, 40.0], [65.0, 60.0]])
    );
    assert_eq!(result, preview.plan(&[]).await);
    let replayed = native::replay(&source, &result);
    assert_eq!(
        Preview::new(&replayed).await.plan(&[]).await["steps"],
        json!([])
    );
}

#[test]
fn clearance_counts_a_removed_collinear_overlap_but_an_overlapped_run_is_not_isolated() {
    let overlapped = insert_before_close(CROWDED, &format_wire(65.0, 96.52, 80.0, 96.52));
    let before_tree = parse_sexp(&overlapped).unwrap();
    let after_tree = parse_sexp(CROWDED).unwrap();
    let before = Sheet::new(&before_tree).unwrap();
    let after = Sheet::new(&after_tree).unwrap();
    let index = before.index();
    let jog = before
        .chains(&index)
        .into_iter()
        .find(|r| r.indices.len() == 6)
        .unwrap();
    assert!(before.tapped(&jog, &index));
    let id = before
        .wires
        .iter()
        .find(|wire| ends(wire) == [(65.0, 96.52), (80.0, 96.52)])
        .unwrap()
        .uuid
        .as_deref()
        .unwrap();
    assert_eq!(check_clearance(before.tree, after.tree, &[id]), Ok(()));
    assert_eq!(
        check_clearance(after.tree, before.tree, &[id]),
        Err("would add a collinear_overlap finding".into())
    );
}

#[test]
fn clearance_refuses_a_new_wire_through_a_symbol_body() {
    let edit = insert_before_close(SPARSE, &format_wire(34.29, 38.1, 41.91, 38.1));
    let before_tree = parse_sexp(SPARSE).unwrap();
    let after_tree = parse_sexp(&edit).unwrap();
    let after = Sheet::new(&after_tree).unwrap();
    let added = after.wires.last().unwrap().uuid.as_deref().unwrap();
    assert!(sch_layout_score::cleanup_gate(&before_tree, &after_tree, &[]).is_empty());
    assert_eq!(
        check_clearance(&before_tree, &after_tree, &[added]),
        Err("would add a wire_overlap finding".into())
    );
}

fn with_redundant_power() -> String {
    let source = insert_before_close(
        CROWDED,
        r##"(symbol (lib_id "power:+3V3") (at 139.7 68.58 0) (unit 1)
            (property "Reference" "#PWR99" (at 139.7 68.58 0) (hide yes))
            (property "Value" "+3V3" (at 139.7 64.77 0))
            (uuid "00000000-0000-4000-8000-000000000099"))"##,
    );
    let source = insert_before_close(&source, &fixture_wire((139.7, 68.58), (139.7, 76.2), 205));
    insert_before_close(
        &source,
        r#"
        (junction (at 139.7 76.2) (diameter 0) (color 0 0 0 0)
            (uuid "00000000-0000-4000-8000-000000000103"))"#,
    )
}

#[tokio::test]
async fn redundant_power_is_removed_only_with_a_surviving_driver_on_its_drawn_node() {
    let source = with_redundant_power();
    let result = Preview::new(&source).await.plan(&["#PWR99"]).await;
    let removed = step(&result, "remove_redundant_power");
    assert_eq!(
        removed["power_symbols_removed"],
        json!(["00000000-0000-4000-8000-000000000099"])
    );
    assert_eq!(array(removed, "wires_removed").len(), 1);
    assert_eq!(removed["route"], json!([]));
    assert_eq!(removed["length_before_mm"], 7.62);
    assert_eq!(
        removed["junctions_removed"],
        json!(["00000000-0000-4000-8000-000000000103"])
    );
}

#[tokio::test]
async fn a_same_name_global_driver_cannot_replace_the_only_local_driver() {
    let source = with_redundant_power();
    let original = r#"(lib_id "power:+3V3") (at 139.7 68.58 0)"#;
    assert_eq!(source.matches(original).count(), 1);
    let source = source.replace(original, r#"(lib_id "power:Local3V3") (at 139.7 68.58 0)"#);
    let source = with_library_symbol(
        &source,
        r#"(symbol "power:Local3V3" (power local)
            (pin power_in line (at 0 0 90) (length 0) (name "") (number "1")))"#,
    );
    let result = Preview::new(&source).await.plan(&["#PWR99"]).await;
    assert!(
        array(&result, "steps")
            .iter()
            .all(|step| step["power_symbols_removed"] == json!([])),
        "{result}"
    );
    assert!(
        skipped_reason(&result, "#PWR99").contains("name and scope"),
        "{result}"
    );
}

#[tokio::test]
async fn another_copy_of_the_rail_elsewhere_is_not_redundancy() {
    let source = insert_before_close(
        CROWDED,
        r##"(symbol (lib_id "power:GND") (at 63.5 115.57 0) (unit 1)
            (property "Reference" "#PWR99" (at 63.5 115.57 0) (hide yes))
            (property "Value" "GND" (at 63.5 119.38 0))
            (uuid "00000000-0000-4000-8000-000000000099"))"##,
    );
    let source = without_symbol(source, "#PWR05");
    let source = insert_before_close(&source, &format_wire(63.5, 115.57, 63.5, 113.03));
    let source = insert_before_close(&source, &format_wire(63.5, 113.03, 59.69, 113.03));
    let source = insert_before_close(
        &source,
        r#"
        (global_label "GND" (shape input) (at 59.69 113.03 180)
            (effects (font (size 1.27 1.27)) (justify right))
            (uuid "00000000-0000-4000-8000-000000000104"))"#,
    );
    let result = Preview::new(&source).await.plan(&["#PWR99"]).await;
    assert!(
        skipped_reason(&result, "#PWR99").contains("C1 pin 2"),
        "{result}"
    );
    assert!(array(&result, "steps")
        .iter()
        .all(|step| step["power_symbols_removed"] == json!([])));
}

fn crowded_with_no_connect() -> String {
    let source = insert_before_close(
        CROWDED,
        r#"(no_connect (at 63.5 113.03) (uuid "00000000-0000-4000-8000-000000000100"))"#,
    );
    without_symbol(source, "#PWR05")
}

#[tokio::test]
async fn a_carried_no_connect_follows_its_drawn_pin() {
    // C1 selects style 1; style 2's copy of pin 2 must not count as a
    // second arrival for the marker. Keep the normal pin geometry unchanged.
    let source = crowded_with_no_connect().replacen(
        r#"(symbol "C_1_1""#,
        r#"(symbol "C_1_2"
            (pin passive line (at 0 -3.81 90) (length 2.794)
                (name "") (number "2")))
        (symbol "C_1_1""#,
        1,
    );
    let result = Preview::new(&source).await.plan(&["C1"]).await;
    let moves = array(step(&result, "reroute"), "no_connects_moved");
    assert_eq!(
        moves,
        &[json!({
            "uuid": "00000000-0000-4000-8000-000000000100", "from": [63.5, 113.03], "to": [114.3, 83.82]
        })],
        "{result}"
    );
}

fn without_symbol(source: String, reference: &str) -> String {
    let (start, end) = find_direct_child_blocks(&source, "kicad_sch")
        .into_iter()
        .find(|&(s, e)| {
            parse_sexp(&source[s..e])
                .ok()
                .as_ref()
                .and_then(symbol_instance)
                .is_some_and(|inst| inst.reference == reference)
        })
        .unwrap();
    apply_edits(source, vec![SexpEdit::delete(start, end)])
}

#[tokio::test]
async fn malformed_arguments_do_not_silently_default() {
    let preview = Preview::new(CROWDED).await;
    let result = preview
        .call(TOOL, json!({"x1": "0", "y1": 0, "x2": 200, "y2": 200}))
        .await;
    assert!(result.is_error);
    let result: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(result["error"]["kind"], "invalid_argument");
    assert_eq!(result["error"]["field"], "x1");
}

#[tokio::test]
async fn an_unreadable_sheet_names_the_target_and_writes_nothing() {
    let preview = Preview::new(CROWDED).await;
    for path in [
        preview.path.with_file_name("missing.kicad_sch"),
        preview.path.parent().unwrap().to_path_buf(),
    ] {
        let result = served_call(
            &preview.handler,
            TOOL,
            json!({"schematic": path, "x1": 0, "y1": 0, "x2": 200, "y2": 200}),
        )
        .await;
        assert!(result.is_error);
        let result: Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(result["error"]["kind"], "file_not_found");
        assert_eq!(result["error"]["path"], json!(path));
    }
    assert_eq!(std::fs::read(preview.path).unwrap(), CROWDED.as_bytes());
    assert_eq!(std::fs::read_dir(preview._dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn a_malformed_sheet_names_the_target_and_writes_nothing() {
    let preview = Preview::new(CROWDED).await;
    for source in [
        b"\xff".as_slice(),
        b"(kicad_sch",
        b"(kicad_pcb (version 1))",
    ] {
        std::fs::write(&preview.path, source).unwrap();
        let result = served_call(
            &preview.handler,
            TOOL,
            json!({"schematic": preview.path, "x1": 0, "y1": 0, "x2": 200, "y2": 200}),
        )
        .await;
        assert!(result.is_error);
        let result: Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(result["error"]["kind"], "malformed_file");
        assert_eq!(result["error"]["path"], json!(preview.path));
        assert!(result["error"]["reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        assert_eq!(std::fs::read(&preview.path).unwrap(), source);
        assert_eq!(std::fs::read_dir(preview._dir.path()).unwrap().count(), 1);
    }
}
