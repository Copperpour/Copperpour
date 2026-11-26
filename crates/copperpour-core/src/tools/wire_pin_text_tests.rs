//! `wire_pin_text_overlap` (#842) against
//! `tests/fixtures/wire_pin_text_kicad10.kicad_sch`, whose README names each
//! wire, and the issue's X1 case in `wire_through_symbol_kicad10.kicad_sch`.

use super::request;
use crate::{mcp::handler::McpHandler, tools::ServerConfig};
use copperpour_sexp::schematic::{extract_wires, Wire};
use serde_json::{json, Value};

const SHEET: &str = include_str!("../../tests/fixtures/wire_pin_text_kicad10.kicad_sch");
const X1_SHEET: &str = include_str!("../../tests/fixtures/wire_through_symbol_kicad10.kicad_sch");

/// Call a `sch_readability` tool on a temporary copy of `source` through the
/// served `tools/call` path, and check that the call left the file as it was.
async fn served(tool: &str, source: &str, mut arguments: Value) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sheet.kicad_sch");
    std::fs::write(&path, source).unwrap();
    arguments["schematic"] = json!(path);
    let handler = McpHandler::new(ServerConfig::default()).await.unwrap();
    let loaded = request(
        &handler,
        "tools/call",
        json!({"name": "load_toolset", "arguments": {"name": "sch_readability"}}),
    )
    .await;
    assert_ne!(loaded["isError"], true, "{loaded}");
    let result = request(
        &handler,
        "tools/call",
        json!({"name": tool, "arguments": arguments}),
    )
    .await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// The pin text findings as (wire uuid, reference, pin number, role, text).
fn pin_text_findings(body: &Value) -> Vec<(String, String, String, String, String)> {
    let mut found = body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["type"] == "wire_pin_text_overlap")
        .map(|o| {
            assert_eq!(o["a"]["kind"], "wire", "{o}");
            assert_eq!(o["b"]["kind"], "pin_text", "{o}");
            assert!(o["b"]["symbol_uuid"].is_string(), "{o}");
            let text = |side: &str, key: &str| o[side][key].as_str().unwrap().to_string();
            (
                text("a", "uuid"),
                text("b", "reference"),
                text("b", "pin_number"),
                text("b", "role"),
                text("b", "text"),
            )
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

fn finding<'a>(body: &'a Value, wire_uuid: &str) -> &'a Value {
    body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["type"] == "wire_pin_text_overlap" && o["a"]["uuid"] == wire_uuid)
        .unwrap_or_else(|| panic!("no pin text finding for {wire_uuid}: {body}"))
}

fn row(
    wire: &str,
    reference: &str,
    pin: &str,
    role: &str,
    text: &str,
) -> (String, String, String, String, String) {
    (
        wire.to_string(),
        reference.to_string(),
        pin.to_string(),
        role.to_string(),
        text.to_string(),
    )
}

/// The four wires that enter a drawn pin name or number are reported, and
/// only those: not the wire ending on `J2`'s pin tip, not the wire through
/// where `U2`'s library name `SWDIO` would be drawn, since the sheet selects
/// the shorter alternate `TMS`, and not the hidden names of `J1`-`J3`.
#[tokio::test]
async fn wires_entering_drawn_pin_text_are_reported() {
    let body = served("check_schematic_overlaps", SHEET, json!({})).await;
    assert_eq!(
        pin_text_findings(&body),
        [
            // From U1's pin 1 tip along the pin into its name, which sits
            // inside at KiCad's default 0.508 mm offset.
            row(
                "2a9d5eb2-38dc-4e5d-8f01-f8230039412a",
                "U1",
                "1",
                "name",
                "VP"
            ),
            row(
                "2f5a9655-e572-43ad-9caa-3779cf705f9c",
                "U2",
                "2",
                "name",
                "TMS"
            ),
            row(
                "6e268212-9084-40c8-8b3d-410dfd5e1ef5",
                "J1",
                "1",
                "number",
                "1"
            ),
            row(
                "c4c16499-790c-403b-aa4f-d77ec381885c",
                "J3",
                "1",
                "number",
                "1"
            ),
        ]
    );
    assert_eq!(body["pin_text_unresolved"], json!([]), "{body}");
    assert_eq!(
        body["overlap_count"],
        body["overlaps"].as_array().unwrap().len(),
        "{body}"
    );

    let j1 = finding(&body, "6e268212-9084-40c8-8b3d-410dfd5e1ef5");
    assert_eq!(
        j1["a"],
        json!({"kind": "wire", "uuid": "6e268212-9084-40c8-8b3d-410dfd5e1ef5",
               "x1": 147.32, "y1": 138.43, "x2": 149.86, "y2": 138.43})
    );
    assert_eq!(j1["b"]["unit"], 1);
    assert_eq!(
        (&j1["b"]["tip_x"], &j1["b"]["tip_y"]),
        (&json!(147.32), &json!(139.7))
    );
    assert!(j1["b"]["pin_uuid"].is_string(), "{j1}");
}

/// Each reported run matches the ink of the string in KiCad 10.0.8's SVG
/// plot of the fixture (`kicad-cli sch export svg`, path points grown by half
/// the 0.1524 mm stroke; see the README) to within 0.001 mm on every side.
#[tokio::test]
async fn reported_runs_match_kicads_plot() {
    let body = served("check_schematic_overlaps", SHEET, json!({})).await;
    for (wire, plotted) in [
        (
            "2a9d5eb2-38dc-4e5d-8f01-f8230039412a",
            [100.8333, 97.2939, 102.2557, 99.4420],
        ),
        (
            "2f5a9655-e572-43ad-9caa-3779cf705f9c",
            [178.6880, 103.3733, 182.1061, 104.7957],
        ),
        (
            "6e268212-9084-40c8-8b3d-410dfd5e1ef5",
            [148.7860, 137.8347, 149.6641, 139.2571],
        ),
        (
            "c4c16499-790c-403b-aa4f-d77ec381885c",
            [99.7347, 155.1358, 101.1571, 156.0139],
        ),
    ] {
        let bounds = &finding(&body, wire)["bounds_b"];
        let measured =
            ["x_min", "y_min", "x_max", "y_max"].map(|key| bounds[key].as_f64().unwrap());
        for (side, (got, want)) in measured.iter().zip(plotted).enumerate() {
            assert!(
                (got - want).abs() <= 0.001,
                "{wire} side {side}: {got} against KiCad's {want}: {bounds}"
            );
        }
    }
}

/// The issue's first case: the wire from X1's pin 1 tip runs through both
/// strokes of the drawn name `A`, besides crossing X1's body.
#[tokio::test]
async fn a_wire_through_a_pin_name_is_reported_beside_the_body_crossing() {
    let body = served("check_schematic_overlaps", X1_SHEET, json!({})).await;
    let wire = "ff399ce4-c445-46a7-b92c-8ec0839aee42";
    assert_eq!(
        pin_text_findings(&body),
        [row(wire, "X1", "1", "name", "A")]
    );
    assert!(body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["type"] == "wire_overlap" && o["a"]["uuid"] == wire));
}

/// A wire along a run's edge only touches it; one a hair inside enters it.
#[test]
fn an_edge_touch_is_not_an_entry() {
    use copperpour_sexp::schematic::{find_lib_symbol, symbol_instance};
    use copperpour_sexp::text_bounds::{pin_text_bounds, PinNameChoice, PinTextRole};
    let tree = copperpour_sexp::parse_sexp(SHEET).unwrap();
    // The exact run, not the reported one rounded to six decimals.
    let instance = tree
        .find_all("symbol")
        .into_iter()
        .filter_map(symbol_instance)
        .find(|instance| instance.reference == "J1")
        .unwrap();
    let library = tree.find("lib_symbols").unwrap().find_all("symbol");
    let lib = find_lib_symbol(&library, &instance).unwrap();
    let run = pin_text_bounds(
        lib,
        &instance,
        1,
        0.1524,
        0.15,
        |_| Some(PinNameChoice::Base),
        true,
    )
    .map(Result::unwrap)
    .find(|text| text.number == "1" && text.role == PinTextRole::Number)
    .unwrap()
    .run;
    let (x_min, x_max, y_max) = (run.min_x, run.max_x, run.max_y);
    assert!(!extract_wires(&tree).is_empty());
    let wire = |y: f64| Wire {
        x1: x_min - 1.0,
        y1: y,
        x2: x_max + 1.0,
        y2: y,
        uuid: Some("probe".to_string()),
    };
    let reported = |probe: Wire| {
        super::super::sch_analysis::pin_text_findings(&tree, &[probe])
            .findings
            .iter()
            .any(|o| o["b"]["reference"] == "J1" && o["b"]["role"] == "number")
    };
    assert!(!reported(wire(y_max)), "along the edge");
    assert!(reported(wire(y_max - 0.01)), "inside the run");
}

/// A saved pin uuid is keyed by number, so it names the pin only when no
/// other drawn pin shares the number; otherwise the tip tells them apart.
#[tokio::test]
async fn a_shared_pin_number_gives_no_pin_uuid() {
    let start = SHEET
        .find("(symbol \"Connector_Generic:Conn_01x02\"")
        .unwrap();
    let two = "(number \"2\"";
    let at = start + SHEET[start..].find(two).unwrap();
    let shared = format!("{}(number \"1\"{}", &SHEET[..at], &SHEET[at + two.len()..]);
    let body = served("check_schematic_overlaps", &shared, json!({})).await;
    let j1 = finding(&body, "6e268212-9084-40c8-8b3d-410dfd5e1ef5");
    assert_eq!(j1["b"]["pin_number"], "1", "{j1}");
    assert_eq!(j1["b"]["pin_uuid"], Value::Null, "{j1}");
    assert_eq!(
        (&j1["b"]["tip_x"], &j1["b"]["tip_y"]),
        (&json!(147.32), &json!(139.7))
    );
}

/// Pin text that cannot be measured names the symbol and why in
/// `pin_text_unresolved`, rather than reading as clear, and leaves
/// `bounds_unresolved` alone.
#[tokio::test]
async fn unmeasured_pin_text_is_named_not_cleared() {
    let undefined = SHEET.replace("(alternate \"TMS\")", "(alternate \"NOPE\")");
    assert_ne!(undefined, SHEET);
    let body = served("check_schematic_overlaps", &undefined, json!({})).await;
    assert_eq!(
        body["pin_text_unresolved"],
        json!([{"reference": "U2", "unit": 1,
                "symbol_uuid": "b31f35cd-86f4-4dec-85c9-71ff2471b2af",
                "reasons": ["undefined_pin_alternate"]}]),
        "{body}"
    );
    assert_eq!(body["bounds_unresolved"], json!([]), "{body}");
    assert!(!pin_text_findings(&body)
        .iter()
        .any(|(_, reference, pin, _, _)| reference == "U2" && pin == "2"));

    // With pin 4 numbered 2 as well, the saved choice for pin 2 could
    // belong to either drawn pin.
    let start = SHEET
        .find("(symbol \"Connector:Conn_ARM_SWD_TagConnect_TC2030\"")
        .unwrap();
    let four = "(number \"4\"";
    let at = start + SHEET[start..].find(four).unwrap();
    let shared = format!("{}(number \"2\"{}", &SHEET[..at], &SHEET[at + four.len()..]);
    let body = served("check_schematic_overlaps", &shared, json!({})).await;
    assert_eq!(
        body["pin_text_unresolved"][0]["reasons"],
        json!(["ambiguous_pin_alternate"]),
        "{body}"
    );

    let stacked = SHEET.replacen("(number \"1\"", "(number \"[1-2]\"", 1);
    assert_ne!(stacked, SHEET);
    let body = served("check_schematic_overlaps", &stacked, json!({})).await;
    let reasons = body["pin_text_unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["reasons"].as_array().unwrap().clone())
        .collect::<Vec<_>>();
    assert!(
        reasons.contains(&json!("unsupported_stacked_pin_number")),
        "{body}"
    );
}

/// The layout score counts the crossings in its text and pin visibility
/// tier. When pin text cannot be measured, only that term is unknown: the
/// body terms keep their counts.
#[tokio::test]
async fn the_layout_score_counts_pin_text_crossings() {
    let score = served("compare_schematic_layout", SHEET, json!({})).await;
    assert_eq!(score["score"]["wire_pin_text_overlap"], 4, "{score}");
    assert_eq!(score["score"]["pin_text_unresolved"], json!([]), "{score}");
    assert!(!score["not_compared"]
        .as_array()
        .unwrap()
        .contains(&json!("wire_pin_text_overlap")));

    let undefined = SHEET.replace("(alternate \"TMS\")", "(alternate \"NOPE\")");
    let score = served("compare_schematic_layout", &undefined, json!({})).await;
    assert_eq!(
        score["score"]["wire_pin_text_overlap"],
        Value::Null,
        "{score}"
    );
    assert!(score["not_compared"]
        .as_array()
        .unwrap()
        .contains(&json!("wire_pin_text_overlap")));
    for term in ["component_overlap", "wire_overlap", "text_overlap_body"] {
        assert!(score["score"][term].is_number(), "{term}: {score}");
    }
}

/// Moving the wire off `J1`'s number, without touching any pin's net, is an
/// improvement the score sees.
#[tokio::test]
async fn moving_a_wire_off_pin_text_improves_the_score() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = dir.path().join("baseline.kicad_sch");
    std::fs::write(&baseline, SHEET).unwrap();
    let crossed = "(xy 147.32 138.43) (xy 149.86 138.43)";
    let clear = "(xy 147.32 135.89) (xy 149.86 135.89)";
    assert_eq!(SHEET.matches(crossed).count(), 1);
    let moved = SHEET.replace(crossed, clear);
    let result = served(
        "compare_schematic_layout",
        &moved,
        json!({"baseline": baseline}),
    )
    .await;
    assert_eq!(result["delta"]["wire_pin_text_overlap"], -1, "{result}");
    assert_eq!(result["verdict"], "improved", "{result}");
}
