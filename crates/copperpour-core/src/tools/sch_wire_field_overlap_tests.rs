use super::{cse, served_overlaps};
use serde_json::{json, Value};

const DIRECT_LANES: &str =
    include_str!("../../tests/fixtures/direct_lanes_kicad10/direct_lanes.kicad_sch");
const FIELD_PLACEMENT: &str =
    include_str!("../../tests/fixtures/field_placement_kicad10/field_placement.kicad_sch");
const POWER_TEXT: &str =
    include_str!("../../tests/fixtures/wire_over_power_text_kicad10.kicad_sch");

fn assert_field_crossing(
    findings: &[Value],
    wire_uuid: &str,
    [x1, y1, x2, y2]: [f64; 4],
    [reference, symbol_uuid, field, text]: [&str; 4],
    ink_bounds: [f64; 4],
) {
    let wire = json!({"kind": "wire", "uuid": wire_uuid, "x1": x1, "y1": y1, "x2": x2, "y2": y2});
    let field = json!({
        "kind": "field", "reference": reference, "unit": 1,
        "symbol_uuid": symbol_uuid, "field": field, "text": text
    });
    let finding = findings
        .iter()
        .find(|o| o["a"] == wire && o["b"] == field)
        .unwrap_or_else(|| panic!("missing {wire} / {field}: {findings:?}"));
    assert_eq!(finding["type"], "wire_field_overlap");
    // Independent bounds from the KiCad 10.0.8 SVG strokes, including half the pen width.
    for (key, ink) in ["x_min", "y_min", "x_max", "y_max"]
        .into_iter()
        .zip(ink_bounds)
    {
        let reported = finding["bounds_b"][key].as_f64().unwrap();
        assert!((reported - ink).abs() < 0.01, "{key}: {finding}");
    }
}

#[tokio::test]
async fn straight_lanes_cross_the_rotated_reference_once_and_the_value_twice() {
    let body = served_overlaps(DIRECT_LANES.as_bytes()).await;
    assert_eq!(body["overlap_count"], 3, "{body}");
    for (wire, y, field, text, ink) in [
        (
            "c9ac38c6-043c-4795-93f9-b053e790821e",
            78.74,
            "Reference",
            "C1",
            [72.8932, 77.1117, 74.3156, 79.2598],
        ),
        (
            "c9ac38c6-043c-4795-93f9-b053e790821e",
            78.74,
            "Value",
            "100nF",
            [67.8132, 77.0513, 69.2356, 82.6464],
        ),
        (
            "676cb32a-f015-4eb4-8a3f-5b9364d0e345",
            81.28,
            "Value",
            "100nF",
            [67.8132, 77.0513, 69.2356, 82.6464],
        ),
    ] {
        assert_field_crossing(
            body["overlaps"].as_array().unwrap(),
            wire,
            [58.42, y, 83.82, y],
            ["C1", "2d93183c-1dda-4390-8f0b-b713d830dc03", field, text],
            ink,
        );
    }
}

#[tokio::test]
async fn only_the_regulator_value_and_mcu_reference_are_crossed_on_the_field_fixture() {
    let body = served_overlaps(FIELD_PLACEMENT.as_bytes()).await;
    assert_eq!(body["overlap_count"], 2, "{body}");
    let findings = body["overlaps"].as_array().unwrap();
    assert_field_crossing(
        findings,
        "9594ab3c-1fae-444d-9ecd-a951cae3a68e",
        [71.12, 88.9, 71.12, 76.2],
        [
            "U1",
            "c70ce4c1-435c-4f9b-8e41-bf7db7e67d50",
            "Value",
            "AMS1117-3.3",
        ],
        [61.1159, 84.9583, 74.0892, 86.3807],
    );
    assert_field_crossing(
        findings,
        "ad6d0fe7-e0c6-4c32-b3e8-7c3f49a82a6f",
        [50.8, 124.46, 76.2, 124.46],
        [
            "U2",
            "ca31c817-f042-4c38-833f-95bf358d2126",
            "Reference",
            "U2",
        ],
        [63.8373, 124.2385, 65.9855, 125.6609],
    );
}

#[tokio::test]
async fn the_worked_example_rail_identifies_the_unannotated_symbols_by_uuid() {
    let source = include_bytes!(
        "../../tests/fixtures/worked_example_kicad10/before/worked_example.kicad_sch"
    );
    let body = served_overlaps(source).await;
    let rail = "e9fb1dd6-b11f-409f-869a-a8512bcb0fe3";
    let fields: Vec<_> = body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["a"]["uuid"] == rail && o["type"] == "wire_field_overlap")
        .map(|o| &o["b"])
        .collect();
    assert_eq!(fields.len(), 2, "{body}");
    for (reference, symbol_uuid, field, text) in [
        ("D?", "688ff7ba-29de-42b1-98ac-0479f96cf728", "Value", "LED"),
        (
            "R?",
            "cf203f95-5abf-4024-bfd6-b95957da19af",
            "Reference",
            "R?",
        ),
    ] {
        assert!(
            fields.contains(&&json!({
                "kind": "field", "reference": reference, "unit": 1,
                "symbol_uuid": symbol_uuid, "field": field, "text": text
            })),
            "{body}"
        );
    }
}

fn sheet(source: &str) -> cse::Schematic {
    cse::Schematic::from_source("source.kicad_sch", source).unwrap()
}

fn field<'a>(sheet: &'a mut cse::Schematic, reference: &str, name: &str) -> &'a mut cse::Property {
    sheet
        .symbols
        .by_reference_mut(reference)
        .unwrap()
        .properties
        .iter_mut()
        .find(|p| p.name == name)
        .unwrap()
}

#[tokio::test]
async fn moving_only_the_value_clears_its_crossings() {
    let mut sheet = sheet(DIRECT_LANES);
    field(&mut sheet, "C1", "Value").translate(2.54, -10.16);
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    assert_eq!(body["overlap_count"], 1, "{body}");
    assert_eq!(body["overlaps"][0]["b"]["field"], "Reference", "{body}");
}

#[tokio::test]
async fn a_wire_crosses_the_mirrored_value_on_its_drawn_side() {
    let mut sheet = sheet(FIELD_PLACEMENT);
    let wire = sheet.add_wire(212.0, 49.53, 212.0, 52.07).uuid.clone();
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    let findings: Vec<_> = body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["a"]["uuid"] == wire)
        .cloned()
        .collect();
    assert_eq!(findings.len(), 1, "{body}");
    assert_field_crossing(
        &findings,
        &wire,
        [212.0, 49.53, 212.0, 52.07],
        [
            "Q1",
            "cd182a65-b122-4229-84d4-e31cf451cebf",
            "Value",
            "BC547",
        ],
        [209.8464, 49.9729, 215.6835, 51.4557],
    );
}

#[tokio::test]
async fn ending_at_a_field_anchor_does_not_exempt_the_crossing() {
    let mut sheet = sheet(FIELD_PLACEMENT);
    let wire = sheet.add_wire(145.0, 83.82, 147.32, 83.82).uuid.clone();
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    let finding = body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["a"]["uuid"] == wire)
        .unwrap_or_else(|| panic!("missing crossing: {body}"));
    assert_eq!(finding["type"], "wire_field_overlap");
    assert_eq!(finding["b"]["reference"], "J1");
    assert_eq!(finding["b"]["field"], "Reference");
}

#[tokio::test]
async fn a_wire_along_the_edge_of_a_field_run_is_not_reported() {
    let mut sheet = sheet(FIELD_PLACEMENT);
    let reference = field(&mut sheet, "J1", "Reference");
    // Set a size whose stroke bounds at this anchor are exact at KiCad's precision.
    let effects = reference
        .sub_nodes
        .iter_mut()
        .find(|n| n.tag() == Some("effects"))
        .unwrap();
    *effects =
        cse::sexp::parser::parse("(effects (font (size 1.26 1.26) (thickness 0.15)))").unwrap();
    let edge = sheet.add_wire(145.0, 83.0601, 150.0, 83.0601).uuid.clone();
    let inside = sheet.add_wire(145.0, 83.0611, 150.0, 83.0611).uuid.clone();
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    let findings = body["overlaps"].as_array().unwrap();
    assert!(findings.iter().all(|o| o["a"]["uuid"] != edge), "{body}");
    let finding = findings
        .iter()
        .find(|o| o["a"]["uuid"] == inside)
        .unwrap_or_else(|| panic!("missing crossing just inside the edge: {body}"));
    assert_eq!(finding["type"], "wire_field_overlap");
    assert_eq!(finding["b"]["reference"], "J1");
    assert_eq!(finding["bounds_b"]["y_min"], 83.0601);
}

#[tokio::test]
async fn a_visible_custom_field_is_not_reported_as_a_wire_field_crossing() {
    let mut sheet = sheet(DIRECT_LANES);
    // Keep the Value's drawn text and placement, but change its role.
    field(&mut sheet, "C1", "Value").name = "Ordering code".into();
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    assert_eq!(body["overlap_count"], 1, "{body}");
    assert_eq!(body["overlaps"][0]["b"]["field"], "Reference", "{body}");
}

#[tokio::test]
async fn a_hidden_value_is_not_reported_as_a_wire_field_crossing() {
    let mut sheet = sheet(DIRECT_LANES);
    field(&mut sheet, "C1", "Value").set_hidden(true);
    let body = served_overlaps(sheet.to_source().as_bytes()).await;
    assert_eq!(body["overlap_count"], 1, "{body}");
    assert_eq!(body["overlaps"][0]["b"]["field"], "Reference", "{body}");
}

#[tokio::test]
async fn a_visible_power_reference_is_not_checked_for_wire_crossings() {
    let mut sheet = sheet(POWER_TEXT);
    let reference = field(&mut sheet, "#PWR01", "Reference");
    reference.set_hidden(false);
    reference.translate(0.0, 5.08);
    let wire = sheet.add_wire(45.72, 59.69, 55.88, 59.69).uuid.clone();
    let source = sheet.to_source();
    let body = served_overlaps(source.as_bytes()).await;
    assert_eq!(body["overlap_count"], 2, "{body}");
    assert!(
        body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["a"]["uuid"] != wire),
        "{body}"
    );
    let tree = copperpour_sexp::parse_sexp(&source).unwrap();
    let wires = copperpour_sexp::schematic::extract_wires(&tree);
    let (clearance, _, _) = crate::tools::sch_analysis::overlap_findings_with_field_clearance(
        &tree,
        &wires,
        crate::tools::sch_analysis::OVERLAP_TOLERANCE,
        true,
    );
    assert!(clearance.iter().any(|o| {
        o["a"]["uuid"] == wire
            && o["type"] == "wire_field_overlap"
            && o["b"]["reference"] == "#PWR01"
            && o["b"]["field"] == "Reference"
    }));
    let mut without_definition = cse::sexp::parser::parse(&source).unwrap();
    remove_power_definition(&mut without_definition);
    let control = served_overlaps(cse::sexp::writer::write(&without_definition).as_bytes()).await;
    assert!(
        control["overlaps"].as_array().unwrap().iter().any(|o| {
            o["a"]["uuid"] == wire
                && o["type"] == "wire_field_overlap"
                && o["b"]["reference"] == "#PWR01"
                && o["b"]["field"] == "Reference"
        }),
        "{control}"
    );
}

#[tokio::test]
async fn an_unresolved_power_instance_can_still_report_a_wire_field_crossing() {
    assert_power_crossing_without_geometry(None, "wire_field_overlap").await;
}

#[tokio::test]
async fn an_embedded_power_marker_still_classifies_a_value_without_body_or_pin_geometry() {
    assert_power_crossing_without_geometry(
        Some(r#"(symbol "power:+3V3" (power))"#),
        "wire_label_overlap",
    )
    .await;
}

async fn assert_power_crossing_without_geometry(replacement: Option<&str>, kind: &str) {
    let mut source = cse::sexp::parser::parse(POWER_TEXT).unwrap();
    let entries = remove_power_definition(&mut source);
    if let Some(definition) = replacement {
        entries.push(cse::sexp::parser::parse(definition).unwrap());
    }

    let body = served_overlaps(cse::sexp::writer::write(&source).as_bytes()).await;
    assert_eq!(body["bounds_unresolved"], json!(["#PWR01"]), "{body}");
    let crossings: Vec<_> = body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["b"]["reference"] == "#PWR01")
        .collect();
    assert_eq!(crossings.len(), 1, "{body}");
    assert_eq!(crossings[0]["type"], kind, "{body}");
    assert_eq!(
        crossings[0]["a"]["uuid"],
        "a1dd2242-097c-42f2-81f7-2f296aeebff6"
    );
    assert_eq!(
        crossings[0]["b"],
        json!({
            "kind": "field", "reference": "#PWR01", "unit": 1,
            "symbol_uuid": "bb8234ac-3887-4cae-8062-7d496f3f5909",
            "field": "Value", "text": "+3V3"
        })
    );
}

fn remove_power_definition(source: &mut cse::sexp::SexpNode) -> &mut Vec<cse::sexp::SexpNode> {
    let Some(cse::sexp::SexpNode::List(entries)) = source.find_mut("lib_symbols") else {
        panic!("fixture must embed a power definition");
    };
    let before = entries.len();
    entries.retain(|entry| entry.tag() != Some("symbol") || entry.value() != Some("power:+3V3"));
    assert_eq!(entries.len(), before - 1);
    entries
}
