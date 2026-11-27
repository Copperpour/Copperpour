use super::*;

const DUPLICATE_PAD_FOOTPRINT: &str = r#"(footprint "DualSocket"
  (version 20240108)
  (generator "pcbnew")
  (layer "F.Cu")
  (descr "preserve me")
  (fp_line (start 0 0) (end 1 1) (stroke (width 0.1) (type default)) (layer "F.SilkS"))
  (pad "3" thru_hole circle (at 1 2) (size 2 2) (drill 1) (layers "*.Cu" "*.Mask"))
  (pad "3" thru_hole oval (at 3 4 90) (size 3 2) (drill oval 1 2) (layers "*.Cu" "*.Mask"))
  (pad "1" thru_hole circle (at 5 6) (size 2 2) (drill 1) (layers "*.Cu" "*.Mask"))
  (model "../models/keep.step"
    (offset (xyz 0 0 0))
    (scale (xyz 1 1 1))
    (rotate (xyz 0 0 0)))
)
"#;

#[tokio::test]
async fn edit_footprint_pad_renumbers_only_the_first_duplicate_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("DualSocket.kicad_mod");
    std::fs::write(&path, DUPLICATE_PAD_FOOTPRINT).unwrap();

    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": path,
            "pad_number": "3",
            "new_number": "1"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["matched_count"], 1);
    assert_eq!(output["updated_count"], 1);

    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated.matches("(pad \"3\"").count(), 1);
    assert_eq!(updated.matches("(pad \"1\"").count(), 2);
    assert!(
        updated.contains("(pad \"1\" thru_hole circle (at 1 2) (size 2 2) (drill 1)"),
        "the first duplicate should be renumbered: {updated}"
    );
    assert!(
        updated.contains("(pad \"3\" thru_hole oval (at 3 4 90) (size 3 2) (drill oval 1 2)"),
        "the second duplicate should remain unchanged: {updated}"
    );
}

#[tokio::test]
async fn edit_footprint_pad_persists_a_valid_shape_change() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("DualSocket.kicad_mod");
    std::fs::write(&path, DUPLICATE_PAD_FOOTPRINT).unwrap();

    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": path,
            "pad_number": "3",
            "shape": "roundrect"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{:?}", result.content);

    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated.matches("thru_hole roundrect").count(), 1);
    assert_eq!(updated.matches("(roundrect_rratio 0.25)").count(), 1);
    parse_sexp(&updated).expect("edited footprint stays parseable");
}

#[tokio::test]
async fn edit_footprint_pad_renumbers_and_resizes_every_duplicate_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("DualSocket.kicad_mod");
    std::fs::write(&path, DUPLICATE_PAD_FOOTPRINT).unwrap();

    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": path,
            "pad_number": "3",
            "new_number": "1",
            "match_all": true,
            "x": 9.5,
            "width": 4.0,
            "height": 5.0
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["matched_count"], 2);
    assert_eq!(output["updated_count"], 2);

    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated.matches("(pad \"3\"").count(), 0);
    assert_eq!(updated.matches("(pad \"1\"").count(), 3);
    assert_eq!(updated.matches("(at 9.5 ").count(), 2);
    assert_eq!(updated.matches("(size 4 5)").count(), 2);
    assert!(updated.contains("(descr \"preserve me\")"));
    assert!(updated.contains("(fp_line (start 0 0) (end 1 1)"));
    assert!(updated.contains("(model \"../models/keep.step\""));
    parse_sexp(&updated).expect("the edited footprint must remain parseable");
}

#[tokio::test]
async fn edit_footprint_pad_missing_match_returns_structured_error_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("DualSocket.kicad_mod");
    std::fs::write(&path, DUPLICATE_PAD_FOOTPRINT).unwrap();

    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": path,
            "pad_number": "404",
            "new_number": "1",
            "match_all": true
        }),
        &test_ctx(),
    )
    .await
    .expect("a missing pad is a tool error, not a transport error");

    assert!(result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["error"]["kind"], "invalid_argument");
    assert_eq!(output["error"]["field"], "pad_number");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        DUPLICATE_PAD_FOOTPRINT
    );
}

#[tokio::test]
async fn edit_footprint_pad_rejects_non_string_new_number_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("DualSocket.kicad_mod");
    std::fs::write(&path, DUPLICATE_PAD_FOOTPRINT).unwrap();

    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": path,
            "pad_number": "3",
            "new_number": 1
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["error"]["kind"], "invalid_argument");
    assert_eq!(output["error"]["field"], "new_number");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        DUPLICATE_PAD_FOOTPRINT
    );
}

#[test]
fn edit_footprint_pad_schema_exposes_batch_renumbering_compatibly() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == "edit_footprint_pad")
        .expect("library must expose edit_footprint_pad");
    let properties = &tool.input_schema["properties"];

    assert_eq!(properties["new_number"]["type"], "string");
    assert_eq!(properties["match_all"]["type"], "boolean");
    assert_eq!(properties["match_all"]["default"], false);
    assert_eq!(
        properties["shape"]["enum"],
        json!(["circle", "rect", "oval", "roundrect", "custom"])
    );
    assert_eq!(
        properties["zone_connect"]["enum"],
        json!(["inherited", "solid", "thermal", "none"])
    );
    assert_eq!(
        properties["fabrication_property"]["enum"],
        json!(KICAD_PAD_FABRICATION_PROPERTIES)
    );
    assert_eq!(
        tool.input_schema["required"],
        json!(["footprint_path", "pad_number"])
    );
}

#[test]
fn pad_shape_change_adds_and_removes_shape_specific_children() {
    let circle = r#"(pad "1" smd circle (at 0 0) (size 2 1) (layers "F.Cu" "F.Mask"))"#;
    let rounded = edit_footprint_pad_block(circle, &json!({}), None, Some("roundrect")).unwrap();
    assert!(rounded.contains(r#"(pad "1" smd roundrect"#), "{rounded}");
    assert!(rounded.contains("(roundrect_rratio 0.25)"), "{rounded}");
    parse_sexp(&rounded).unwrap();

    let chamfered = r#"(pad "1" smd roundrect
  (at 0 0)
  (size 2 1)
  (layers "F.Cu" "F.Mask")
  (roundrect_rratio 0)
  (chamfer_ratio 0.2)
  (chamfer top_left)
)"#;
    let oval = edit_footprint_pad_block(chamfered, &json!({}), None, Some("oval")).unwrap();
    assert!(oval.contains(r#"(pad "1" smd oval"#), "{oval}");
    for removed in ["roundrect_rratio", "chamfer_ratio", "(chamfer "] {
        assert!(!oval.contains(removed), "left {removed} in {oval}");
    }
    parse_sexp(&oval).unwrap();
}

#[test]
fn changing_one_pad_dimension_preserves_the_other() {
    let pad = r#"(pad "1" smd rect (at 0 0) (size 2 1) (layers "F.Cu"))"#;
    let wider = edit_footprint_pad_block(pad, &json!({ "width": 3.5 }), None, None).unwrap();
    assert!(wider.contains("(size 3.5 1)"), "{wider}");

    let taller = edit_footprint_pad_block(pad, &json!({ "height": 4.5 }), None, None).unwrap();
    assert!(taller.contains("(size 2 4.5)"), "{taller}");
}

#[tokio::test]
async fn unsupported_pad_shape_is_rejected_before_reading_or_writing() {
    let result = handle_edit_footprint_pad(
        &json!({
            "footprint_path": "/does/not/exist.kicad_mod",
            "pad_number": "1",
            "shape": "trapezoid"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["error"]["field"], "shape");
}

// KiCad 10.0.6 wrote every one of these; provenance in
// tests/fixtures/pad_zone_connect_kicad10.README.md.
const ZONE_CONNECT_SOLID: &str =
    include_str!("../../../tests/fixtures/pad_zone_connect_kicad10.kicad_mod");
const ZONE_CONNECT_THERMAL: &str =
    include_str!("../../../tests/fixtures/pad_zone_connect_thermal_kicad10.kicad_mod");
const ZONE_CONNECT_INHERITED: &str =
    include_str!("../../../tests/fixtures/pad_zone_connect_inherited_kicad10.kicad_mod");
const ZONE_CONNECT_FIRST_NONE: &str =
    include_str!("../../../tests/fixtures/pad_zone_connect_first_none_kicad10.kicad_mod");

fn footprint_pad_ranges(content: &str) -> Vec<(usize, usize)> {
    find_direct_child_blocks(content, "footprint")
        .into_iter()
        .filter(|&(start, end)| parse_sexp(&content[start..end]).unwrap().head() == Some("pad"))
        .collect()
}

/// `source` with each pad replaced by the pad KiCad wrote in `kicad`. KiCad
/// regenerates the mandatory fields' UUIDs on every save, so the rest of its
/// file cannot be compared; the rest of `source` must survive byte for byte.
fn with_kicad_pads(source: &str, kicad: &str) -> String {
    let source_pads = footprint_pad_ranges(source);
    let kicad_pads = footprint_pad_ranges(kicad);
    assert_eq!(source_pads.len(), kicad_pads.len());
    let edits = source_pads
        .into_iter()
        .zip(kicad_pads)
        .map(|((start, end), (kicad_start, kicad_end))| {
            SexpEdit::replace(start, end, &kicad[kicad_start..kicad_end])
        })
        .collect();
    apply_edits(source.to_string(), edits)
}

#[tokio::test]
async fn edit_footprint_pad_writes_each_zone_connection_as_kicad_does() {
    let cases = [
        (
            "replace solid with thermal",
            ZONE_CONNECT_SOLID,
            "thermal",
            true,
            ZONE_CONNECT_THERMAL,
            3,
        ),
        (
            "replace thermal with solid",
            ZONE_CONNECT_THERMAL,
            "solid",
            true,
            ZONE_CONNECT_SOLID,
            3,
        ),
        (
            "remove the override",
            ZONE_CONNECT_SOLID,
            "inherited",
            true,
            ZONE_CONNECT_INHERITED,
            3,
        ),
        (
            "add none to the first pad only",
            ZONE_CONNECT_INHERITED,
            "none",
            false,
            ZONE_CONNECT_FIRST_NONE,
            1,
        ),
    ];
    for (case, source, zone_connect, match_all, kicad, matched) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp
            .path()
            .join("MountingHole_3.2mm_M3_Pad_TopBottom.kicad_mod");
        std::fs::write(&path, source).unwrap();

        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path": path,
                "pad_number": "1",
                "match_all": match_all,
                "zone_connect": zone_connect
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{case}: {:?}", result.content);
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(output["updated_count"], matched, "{case}");
        assert_eq!(output["zone_connect"], zone_connect, "{case}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            with_kicad_pads(source, kicad),
            "{case}: pads must match KiCad's and nothing else may change"
        );
    }
}

#[tokio::test]
async fn edit_footprint_pad_refuses_an_unsupported_zone_connect_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp
        .path()
        .join("MountingHole_3.2mm_M3_Pad_TopBottom.kicad_mod");
    std::fs::write(&path, ZONE_CONNECT_SOLID).unwrap();

    for zone_connect in [json!("thermal_pth"), json!(2), serde_json::Value::Null] {
        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path": path,
                "pad_number": "1",
                "zone_connect": zone_connect
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error, "{zone_connect} must be refused");
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(output["error"]["kind"], "invalid_argument");
        assert_eq!(output["error"]["field"], "zone_connect");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ZONE_CONNECT_SOLID);
    }
}

#[test]
fn zone_connect_follows_the_indentation_of_a_single_line_pad() {
    let pad = r#"(pad "1" smd rect (at 0 0) (size 2 1) (layers "F.Cu"))"#;
    let solid = set_pad_zone_connect(pad.to_string(), PadZoneConnect::Solid);
    assert_eq!(
        solid,
        r#"(pad "1" smd rect (at 0 0) (size 2 1) (layers "F.Cu") (zone_connect 2))"#
    );
    assert_eq!(set_pad_zone_connect(solid, PadZoneConnect::Inherited), pad);
}

// KiCad 10.0.8 wrote each of these from the stock Winbond USON-8; provenance
// in tests/fixtures/solder_margins_kicad10.README.md.
const MARGINS_NONE: &str =
    include_str!("../../../tests/fixtures/solder_margins_none_kicad10.kicad_mod");
const MARGINS_EP: &str =
    include_str!("../../../tests/fixtures/solder_margins_ep_kicad10.kicad_mod");
const MARGINS_EP_ZERO: &str =
    include_str!("../../../tests/fixtures/solder_margins_ep_zero_kicad10.kicad_mod");
const MARGINS_FOOTPRINT_NAME: &str = "Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm.kicad_mod";

async fn edit_margins(source: &str, arguments: serde_json::Value) -> (serde_json::Value, String) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(MARGINS_FOOTPRINT_NAME);
    std::fs::write(&path, source).unwrap();
    let mut arguments = arguments;
    arguments["footprint_path"] = json!(path);
    let result = handle_edit_footprint_pad(&arguments, &test_ctx())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    (body, std::fs::read_to_string(&path).unwrap())
}

// KiCad 10.0.8 wrote each of these from the stock Winbond USON-8, one
// PAD::SetLayerSet each; provenance in tests/fixtures/pad_layers_kicad10.README.md.
const LAYERS_SOURCE: &str =
    include_str!("../../../tests/fixtures/pad_layers_source_kicad10.kicad_mod");
const LAYERS_NO_PASTE: &str =
    include_str!("../../../tests/fixtures/pad_layers_no_paste_kicad10.kicad_mod");
const LAYERS_BACK: &str = include_str!("../../../tests/fixtures/pad_layers_back_kicad10.kicad_mod");
const LAYERS_EP_PASTE: &str =
    include_str!("../../../tests/fixtures/pad_layers_ep_paste_kicad10.kicad_mod");
const LAYERS_BOTH_SIDES: &str =
    include_str!("../../../tests/fixtures/pad_layers_both_sides_kicad10.kicad_mod");

#[tokio::test]
async fn edit_footprint_pad_writes_layers_as_kicad_does() {
    let cases = [
        (
            "paste removed",
            LAYERS_SOURCE,
            "1",
            json!(["F.Mask", "F.Cu"]),
            LAYERS_NO_PASTE,
        ),
        (
            "paste given back",
            LAYERS_NO_PASTE,
            "1",
            json!(["F.Paste", "F.Cu", "F.Mask"]),
            LAYERS_SOURCE,
        ),
        (
            "moved to the back",
            LAYERS_SOURCE,
            "1",
            json!(["B.Paste", "B.Mask", "B.Cu"]),
            LAYERS_BACK,
        ),
        (
            "paste on the exposed pad",
            LAYERS_SOURCE,
            "9",
            json!(["F.Cu", "F.Paste", "F.Mask"]),
            LAYERS_EP_PASTE,
        ),
        (
            "both sides as wildcards",
            LAYERS_SOURCE,
            "9",
            json!(["B.Mask", "F.Cu", "B.Cu", "F.Mask"]),
            LAYERS_BOTH_SIDES,
        ),
        (
            "wildcards given",
            LAYERS_SOURCE,
            "9",
            json!(["*.Mask", "*.Cu"]),
            LAYERS_BOTH_SIDES,
        ),
        (
            "unchanged",
            LAYERS_SOURCE,
            "1",
            json!(["F.Cu", "F.Mask", "F.Paste"]),
            LAYERS_SOURCE,
        ),
    ];
    for (case, source, pad, layers, kicad) in cases {
        let (body, written) =
            edit_margins(source, json!({ "pad_number": pad, "layers": layers })).await;
        assert_eq!(body["updated_count"], 1, "{case}: {body}");
        assert_eq!(
            solder_margins::without_uuids(&written),
            solder_margins::without_uuids(kicad),
            "{case}: the pad must match KiCad's and nothing else may change"
        );
    }

    let (body, written) = edit_margins(
        LAYERS_SOURCE,
        json!({ "pad_number": "1", "layers": ["F.Cu", "F.Mask"] }),
    )
    .await;
    assert_eq!(
        body["layers"],
        json!({ "previous": ["F.Cu", "F.Mask", "F.Paste"], "current": ["F.Cu", "F.Mask"] })
    );
    let pad_uuid = |text: &str| {
        let pad = &text[text.find("(pad \"1\"").unwrap()..];
        pad[pad.find("(uuid").unwrap()..][..44].to_string()
    };
    assert_eq!(
        pad_uuid(&written),
        pad_uuid(LAYERS_SOURCE),
        "the pad keeps its UUID"
    );
}

#[tokio::test]
async fn edit_footprint_pad_layers_with_match_all_report_each_distinct_change() {
    let source = "(footprint \"P\"\n  (version 20260206)\n  (layer \"F.Cu\")\n  \
        (pad \"1\" smd rect (at -1 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\") (uuid \"a\"))\n  \
        (pad \"1\" smd rect (at 1 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (uuid \"b\"))\n  \
        (pad \"2\" smd rect (at 3 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\") (uuid \"c\"))\n)\n";
    let (body, written) = edit_margins(
        source,
        json!({ "pad_number": "1", "match_all": true, "layers": ["F.Cu", "F.Paste", "F.Mask"] }),
    )
    .await;
    assert_eq!(body["updated_count"], 2, "{body}");
    assert_eq!(
        body["layers"],
        json!([
            { "previous": ["F.Cu", "F.Mask"], "current": ["F.Cu", "F.Mask", "F.Paste"] },
            { "previous": ["F.Cu", "F.Mask", "F.Paste"], "current": ["F.Cu", "F.Mask", "F.Paste"] }
        ])
    );
    assert_eq!(
        written,
        source.replacen(
            "(layers \"F.Cu\" \"F.Mask\") (uuid \"a\")",
            "(layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (uuid \"a\")",
            1
        ),
        "both pads 1 are on the new layers, pad 2 is untouched"
    );
}

#[tokio::test]
async fn edit_footprint_pad_refuses_bad_layers_without_writing() {
    for (layers, reason) in [
        (
            json!("F.Cu"),
            "must be an array of layer names when supplied",
        ),
        (json!(null), "must be an array of layer names when supplied"),
        (json!([]), "pad layers must not be empty"),
        (json!(["F.Cu", 3]), "pad layers must contain strings"),
        (json!(["F.Cu", "In1.Cu"]), "invalid pad layer: In1.Cu"),
        (json!(["F.SilkS"]), "invalid pad layer: F.SilkS"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("P.kicad_mod");
        std::fs::write(&path, LAYERS_SOURCE).unwrap();
        let result = handle_edit_footprint_pad(
            &json!({ "footprint_path": path, "pad_number": "1", "layers": layers }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{layers}");
        let body: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(body["error"]["field"], "layers", "{body}");
        assert!(
            body["error"]["reason"].as_str().unwrap().contains(reason),
            "{layers}: {body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), LAYERS_SOURCE);
    }
}

#[test]
fn pad_layers_are_written_in_kicads_order_and_wildcards() {
    for (names, written) in [
        (
            vec!["F.Paste", "F.Mask", "F.Cu"],
            r#"(layers "F.Cu" "F.Mask" "F.Paste")"#,
        ),
        (vec!["B.Mask", "B.Cu"], r#"(layers "B.Cu" "B.Mask")"#),
        (vec!["F.Cu", "B.Cu"], r#"(layers "*.Cu")"#),
        (
            vec!["*.Cu", "F.Cu", "F.Mask"],
            r#"(layers "*.Cu" "F.Mask")"#,
        ),
        (
            vec!["B.Paste", "F.Paste", "B.Mask", "F.Mask", "F.Cu"],
            r#"(layers "*.Paste" "*.Mask" "F.Cu")"#,
        ),
        (
            vec!["F.Mask", "B.Cu", "F.Mask"],
            r#"(layers "F.Mask" "B.Cu")"#,
        ),
        (vec!["F.Paste"], r#"(layers "F.Paste")"#),
        // KiCad 10.0.8 wrote these two (#825's review): both pastes fold to
        // `*.Paste` after `*.Cu`, and B.Mask (layer 3) precedes F.Paste (13).
        (
            vec!["F.Paste", "B.Paste", "B.Cu", "F.Cu"],
            r#"(layers "*.Cu" "*.Paste")"#,
        ),
        (vec!["F.Paste", "B.Mask"], r#"(layers "B.Mask" "F.Paste")"#),
    ] {
        assert_eq!(kicad_pad_layers(&names), written, "{names:?}");
    }
}

#[test]
fn edit_footprint_pad_schema_offers_the_create_footprint_layer_names() {
    let tools = tools();
    let edit = tools
        .iter()
        .find(|tool| tool.name == "edit_footprint_pad")
        .unwrap();
    assert_eq!(
        edit.input_schema["properties"]["layers"]["items"]["enum"],
        json!(PAD_LAYER_NAMES)
    );
}

#[tokio::test]
async fn edit_footprint_pad_writes_solder_margins_as_kicad_does() {
    let set = json!({
        "solder_mask_margin": -0.05,
        "solder_paste_margin": -0.1,
        "solder_paste_margin_ratio": -0.2
    });
    let zero = json!({
        "solder_mask_margin": 0,
        "solder_paste_margin": 0,
        "solder_paste_margin_ratio": 0
    });
    let inherited = json!({
        "solder_mask_margin": "inherited",
        "solder_paste_margin": "inherited",
        "solder_paste_margin_ratio": "inherited"
    });
    let cases = [
        ("set on the exposed pad", MARGINS_NONE, &set, MARGINS_EP),
        ("an override of 0", MARGINS_NONE, &zero, MARGINS_EP_ZERO),
        ("a value replaced by 0", MARGINS_EP, &zero, MARGINS_EP_ZERO),
        ("values removed", MARGINS_EP, &inherited, MARGINS_NONE),
        ("zeros removed", MARGINS_EP_ZERO, &inherited, MARGINS_NONE),
        ("nothing to remove", MARGINS_NONE, &inherited, MARGINS_NONE),
    ];
    for (case, source, margins, kicad) in cases {
        let mut arguments = margins.clone();
        arguments["pad_number"] = json!("9");
        let (body, written) = edit_margins(source, arguments).await;
        assert_eq!(body["updated_count"], 1, "{case}: {body}");
        assert_eq!(
            solder_margins::without_uuids(&written),
            solder_margins::without_uuids(kicad),
            "{case}: the pad must match KiCad's and nothing else may change"
        );
    }

    let mut arguments = set.clone();
    arguments["pad_number"] = json!("9");
    let (body, _) = edit_margins(MARGINS_EP_ZERO, arguments).await;
    assert_eq!(
        body["solder_mask_margin"],
        json!({ "previous": 0.0, "current": -0.05 })
    );
    assert_eq!(
        body["solder_paste_margin"],
        json!({ "previous": 0.0, "current": -0.1 })
    );
    assert_eq!(
        body["solder_paste_margin_ratio"],
        json!({ "previous": 0.0, "current": -0.2 })
    );
    assert_eq!(
        body.get("zone_connect"),
        None,
        "only what was asked is reported"
    );
}

#[tokio::test]
async fn pads_that_disagree_report_each_previous_margin() {
    let (_, first) = edit_margins(
        ZONE_CONNECT_SOLID,
        json!({ "pad_number": "1", "solder_mask_margin": 0.2 }),
    )
    .await;
    let (body, written) = edit_margins(
        &first,
        json!({ "pad_number": "1", "match_all": true, "solder_mask_margin": 0.1 }),
    )
    .await;
    assert_eq!(body["updated_count"], 3, "{body}");
    assert_eq!(
        body["solder_mask_margin"],
        json!([
            { "previous": 0.2, "current": 0.1 },
            { "previous": "inherited", "current": 0.1 }
        ])
    );
    assert_eq!(written.matches("(solder_mask_margin 0.1)").count(), 3);
    assert!(!written.contains("(solder_mask_margin 0.2)"));
}

/// Up to format 20240201 KiCad reads a margin of 0 as "inherited": an
/// override of 0 is refused there, and an existing 0 reads as inherited.
#[tokio::test]
async fn a_pad_margin_of_zero_is_refused_in_an_older_file() {
    let older = MARGINS_EP_ZERO.replace("(version 20260206)", "(version 20240108)");
    for field in [
        "solder_mask_margin",
        "solder_paste_margin",
        "solder_paste_margin_ratio",
    ] {
        let (body, written) = edit_margins(&older, json!({ "pad_number": "9", field: 0 })).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
        assert_eq!(written, older);
    }
    let (body, _) = edit_margins(
        &older,
        json!({ "pad_number": "9", "solder_mask_margin": -0.05 }),
    )
    .await;
    assert_eq!(
        body["solder_mask_margin"],
        json!({ "previous": "inherited", "current": -0.05 })
    );
}

#[tokio::test]
async fn a_pad_margin_that_is_not_a_number_is_refused_on_its_argument() {
    for (field, value) in [
        ("solder_mask_margin", json!(null)),
        ("solder_paste_margin", json!("-0.1")),
        ("solder_paste_margin_ratio", json!(false)),
    ] {
        let (body, written) =
            edit_margins(MARGINS_NONE, json!({ "pad_number": "9", field: value })).await;
        assert_eq!(body["error"]["field"], field, "{body}");
        assert_eq!(written, MARGINS_NONE);
    }
}

// KiCad 10.0.6 wrote every one of these; provenance in
// tests/fixtures/pad_fabrication_property_kicad10.README.md.
const PAD_PROP_MECHANICAL: &str =
    include_str!("../../../tests/fixtures/pad_fabrication_property_mechanical_kicad10.kicad_mod");
const PAD_PROP_CASTELLATED: &str =
    include_str!("../../../tests/fixtures/pad_fabrication_property_castellated_kicad10.kicad_mod");
const PAD_PROP_FIRST_CASTELLATED: &str = include_str!(
    "../../../tests/fixtures/pad_fabrication_property_first_castellated_kicad10.kicad_mod"
);
const PAD_PROP_NONE: &str =
    include_str!("../../../tests/fixtures/pad_fabrication_property_none_kicad10.kicad_mod");
const PAD_PROP_TESTPOINT: &str =
    include_str!("../../../tests/fixtures/pad_fabrication_property_testpoint_kicad10.kicad_mod");
const PAD_PROP_FOOTPRINT: &str = "USB_C_Receptacle_GCT_USB4125-xx-x_6P_TopMnt_Horizontal.kicad_mod";

/// KiCad 10's `PAD_PROP` names (`pcb_io_kicad_sexpr.cpp`), the promise the
/// schema makes; not read from the code under test.
const KICAD_PAD_FABRICATION_PROPERTIES: [&str; 9] = [
    "none",
    "bga",
    "fiducial_glob",
    "fiducial_loc",
    "testpoint",
    "heatsink",
    "castellated",
    "mechanical",
    "pressfit",
];

#[tokio::test]
async fn edit_footprint_pad_writes_each_fabrication_property_as_kicad_does() {
    let cases = [
        (
            "mechanical to castellated on every SH pad",
            PAD_PROP_MECHANICAL,
            "SH",
            true,
            "castellated",
            PAD_PROP_CASTELLATED,
            4,
        ),
        (
            "castellated back to mechanical",
            PAD_PROP_CASTELLATED,
            "SH",
            true,
            "mechanical",
            PAD_PROP_MECHANICAL,
            4,
        ),
        (
            "castellated on the first SH pad only",
            PAD_PROP_MECHANICAL,
            "SH",
            false,
            "castellated",
            PAD_PROP_FIRST_CASTELLATED,
            1,
        ),
        (
            "remove the property",
            PAD_PROP_MECHANICAL,
            "SH",
            true,
            "none",
            PAD_PROP_NONE,
            4,
        ),
        (
            "add one where there was none",
            PAD_PROP_MECHANICAL,
            "A5",
            false,
            "testpoint",
            PAD_PROP_TESTPOINT,
            1,
        ),
        (
            "none on a pad without one",
            PAD_PROP_NONE,
            "A5",
            false,
            "none",
            PAD_PROP_NONE,
            1,
        ),
    ];
    for (case, source, pad, match_all, property, kicad, matched) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(PAD_PROP_FOOTPRINT);
        std::fs::write(&path, source).unwrap();

        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path": path,
                "pad_number": pad,
                "match_all": match_all,
                "fabrication_property": property
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{case}: {:?}", result.content);
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(output["updated_count"], matched, "{case}");
        assert_eq!(output["fabrication_property"], property, "{case}");
        assert!(output.get("warnings").is_none(), "{case}: {output}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            with_kicad_pads(source, kicad),
            "{case}: pads must match KiCad's and nothing else may change"
        );
    }
}

#[tokio::test]
async fn edit_footprint_pad_passes_on_kicads_padstack_warning() {
    // KiCad 10.0.6 DRC on castellated A5 (SMD) reports "Padstack is
    // questionable ('castellated' pads are normally PTH)"; see the README.
    let cases = [
        ("A5", "castellated", Some("pad 'A5': Padstack is questionable ('castellated' pads are normally PTH)")),
        ("SH", "pressfit", Some("pad 'SH': Padstack is questionable ('press-fit' pads are normally PTH with round holes)")),
        ("SH", "bga", Some("pad 'SH': Padstack is questionable ('BGA' property is for SMD pads)")),
        ("A5", "bga", None),
        ("SH", "heatsink", None),
    ];
    for (pad, property, warning) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(PAD_PROP_FOOTPRINT);
        std::fs::write(&path, PAD_PROP_MECHANICAL).unwrap();
        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path": path,
                "pad_number": pad,
                "match_all": true,
                "fabrication_property": property
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{pad} {property}: {:?}", result.content);
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(output["fabrication_property"], property);
        match warning {
            Some(warning) => {
                assert_eq!(output["warnings"], json!([warning]), "{pad} {property}")
            }
            None => assert!(
                output.get("warnings").is_none(),
                "{pad} {property}: {output}"
            ),
        }
    }
}

#[tokio::test]
async fn padstack_warning_judges_the_pad_as_written() {
    // The SH pads' oval drill draws KiCad's press-fit warning; a round
    // drill written in the same call does not, and a rename is reported.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PAD_PROP_FOOTPRINT);
    for (extra, warning) in [
        (json!({}), Some("pad 'SH': Padstack is questionable ('press-fit' pads are normally PTH with round holes)")),
        (json!({ "drill": 1.0 }), None),
        (json!({ "new_number": "S1" }), Some("pad 'S1': Padstack is questionable ('press-fit' pads are normally PTH with round holes)")),
    ] {
        std::fs::write(&path, PAD_PROP_MECHANICAL).unwrap();
        let mut args = json!({
            "footprint_path": path,
            "pad_number": "SH",
            "fabrication_property": "pressfit"
        });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let result = handle_edit_footprint_pad(&args, &test_ctx()).await.unwrap();
        assert!(!result.is_error, "{extra}: {:?}", result.content);
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        match warning {
            Some(warning) => assert_eq!(output["warnings"], json!([warning]), "{extra}"),
            None => assert!(output.get("warnings").is_none(), "{extra}: {output}"),
        }
    }
}

#[tokio::test]
async fn served_edit_footprint_pad_sets_castellated() {
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;
    let handler = McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PAD_PROP_FOOTPRINT);
    std::fs::write(&path, PAD_PROP_MECHANICAL).unwrap();

    let call = |property: serde_json::Value| {
        json!({
            "jsonrpc": "2.0", "id": 961, "method": "tools/call",
            "params": {"name": "edit_footprint_pad", "arguments": {
                "footprint_path": path, "pad_number": "SH", "match_all": true,
                "fabrication_property": property
            }}
        })
    };
    let refused = handler
        .handle_message(call(json!("castellated_pad")))
        .await
        .unwrap()
        .result
        .unwrap();
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PAD_PROP_MECHANICAL);

    let response = handler
        .handle_message(call(json!("castellated")))
        .await
        .unwrap()
        .result
        .unwrap();
    assert_ne!(response["isError"], true, "{response}");
    let report: serde_json::Value =
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(report["fabrication_property"], "castellated");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        with_kicad_pads(PAD_PROP_MECHANICAL, PAD_PROP_CASTELLATED)
    );
}

#[tokio::test]
async fn edit_footprint_pad_refuses_an_unsupported_fabrication_property_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PAD_PROP_FOOTPRINT);
    std::fs::write(&path, PAD_PROP_MECHANICAL).unwrap();

    for property in [
        json!("pad_prop_castellated"),
        json!("Castellated"),
        json!(6),
        serde_json::Value::Null,
    ] {
        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path": path,
                "pad_number": "SH",
                "fabrication_property": property
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error, "{property} must be refused");
        let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(output["error"]["kind"], "invalid_argument");
        assert_eq!(output["error"]["field"], "fabrication_property");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PAD_PROP_MECHANICAL);
    }
}

#[test]
fn fabrication_property_is_read_as_kicad_loads_it() {
    // KiCad's parser keeps the last token it knows and skips the rest.
    for (pad, expected) in [
        (r#"(pad "1" smd rect (layers "F.Cu"))"#, "none"),
        (
            r#"(pad "1" smd rect (property pad_prop_bga) (layers "F.Cu"))"#,
            "bga",
        ),
        (
            r#"(pad "1" smd rect (property pad_prop_bga pad_prop_heatsink))"#,
            "heatsink",
        ),
        (
            r#"(pad "1" smd rect (property pad_prop_bga) (property none))"#,
            "none",
        ),
        (
            r#"(pad "1" smd rect (property pad_prop_castellated pad_prop_future))"#,
            "castellated",
        ),
    ] {
        assert_eq!(
            PadFabricationProperty::of_pad(&parse_sexp(pad).unwrap()).as_str(),
            expected,
            "{pad}"
        );
    }
}

#[test]
fn fabrication_property_replaces_duplicates_and_follows_a_single_line_pad() {
    let pad = r#"(pad "1" smd rect (at 0 0) (size 2 1) (layers "F.Cu") (uuid "u"))"#;
    let castellated =
        set_pad_fabrication_property(pad.to_string(), PadFabricationProperty::Castellated);
    assert_eq!(
        castellated,
        r#"(pad "1" smd rect (at 0 0) (size 2 1) (property pad_prop_castellated) (layers "F.Cu") (uuid "u"))"#
    );
    let doubled = r#"(pad "1" smd rect (property pad_prop_bga) (property pad_prop_heatsink) (layers "F.Cu"))"#;
    assert_eq!(
        set_pad_fabrication_property(doubled.to_string(), PadFabricationProperty::Testpoint),
        r#"(pad "1" smd rect (property pad_prop_testpoint) (layers "F.Cu"))"#
    );
    assert_eq!(
        set_pad_fabrication_property(doubled.to_string(), PadFabricationProperty::None),
        r#"(pad "1" smd rect (layers "F.Cu"))"#
    );
}

#[test]
fn zone_connect_goes_after_the_pth_layer_children_as_kicad_writes_it() {
    // KiCad writes zone_layer_connections beside remove_unused_layers and
    // keep_end_layers, before net and zone_connect (format( const PAD* )).
    let pad = r#"(pad "1" thru_hole circle (layers "*.Cu") (remove_unused_layers yes) (keep_end_layers no) (zone_layer_connections) (net 1 "GND") (uuid "u"))"#;
    assert_eq!(
        set_pad_zone_connect(pad.to_string(), PadZoneConnect::Solid),
        r#"(pad "1" thru_hole circle (layers "*.Cu") (remove_unused_layers yes) (keep_end_layers no) (zone_layer_connections) (net 1 "GND") (zone_connect 2) (uuid "u"))"#
    );
}

#[test]
fn observed_values_are_reported_once_each_in_first_seen_order() {
    let mut response = json!({});
    report_observed(
        &mut response,
        "k",
        vec!["thermal", "unrecognized", "thermal"],
    );
    assert_eq!(response["k"], json!(["thermal", "unrecognized"]));
    report_observed(&mut response, "one", vec!["solid", "solid"]);
    assert_eq!(response["one"], "solid");
}

#[tokio::test]
async fn create_footprint_writes_a_pad_fabrication_property() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("CASTELLATED.kicad_mod");
    let pad = |number: &str, x: f64, property: serde_json::Value| {
        json!({
            "number": number, "type": "thru_hole", "shape": "circle",
            "x": x, "y": 0.0, "width": 1.7, "height": 1.7, "drill": 1.0,
            "fabrication_property": property
        })
    };
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "CASTELLATED",
        "pads": [
            pad("1", 0.0, json!("castellated")),
            pad("2", 2.54, json!("none")),
            pad("3", 5.08, serde_json::Value::Null)
        ]
    });

    let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert!(output.get("warnings").is_none(), "{output}");
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(content.contains("(generator \"copperpour\")"), "{content}");
    let pads: Vec<&str> = footprint_pad_ranges(&content)
        .into_iter()
        .map(|(start, end)| &content[start..end])
        .collect();
    assert_eq!(pads.len(), 3);
    assert!(
        pads[0].contains("(property pad_prop_castellated)\n\t\t(layers \"*.Cu\" \"*.Mask\")"),
        "{}",
        pads[0]
    );
    for (pad, expected) in pads.iter().zip(["castellated", "none", "none"]) {
        assert_eq!(
            PadFabricationProperty::of_pad(&parse_sexp(pad).unwrap()).as_str(),
            expected,
            "{pad}"
        );
    }
    assert!(!pads[1].contains("(property") && !pads[2].contains("(property"));
}

#[tokio::test]
async fn create_footprint_passes_on_kicads_padstack_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("SMD_CASTELLATED.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "SMD_CASTELLATED",
        "pads": [{
            "number": "1", "type": "smd", "shape": "rect",
            "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0,
            "fabrication_property": "castellated"
        }]
    });

    let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(
        output["warnings"],
        json!(["pad '1': Padstack is questionable ('castellated' pads are normally PTH)"])
    );
    assert!(std::fs::read_to_string(&out)
        .unwrap()
        .contains("(property pad_prop_castellated)"));
}

#[tokio::test]
async fn create_footprint_rejects_an_unsupported_fabrication_property_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("INVALID_PROPERTY.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "INVALID_PROPERTY",
        "pads": [{
            "number": "1", "type": "smd", "shape": "rect",
            "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0,
            "fabrication_property": "half_hole"
        }]
    });

    let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["error"]["field"], "pads[0].fabrication_property");
    assert!(!out.exists());
}

fn pad(number: &str, t: &str, x: f64, y: f64, w: f64, h: f64) -> PadGeom {
    PadGeom {
        number: number.into(),
        pad_type: t.into(),
        x,
        y,
        w,
        h,
        rotation: 0.0,
        custom_bounds: None,
    }
}

#[test]
fn pads_bbox_covers_pad_extents() {
    let pads = vec![
        pad("1", "smd", -1.0, 0.0, 0.4, 0.6),
        pad("2", "smd", 1.0, 0.0, 0.4, 0.6),
    ];
    let (min_x, min_y, max_x, max_y) = pads_bbox(&pads);
    assert!((min_x - -1.2).abs() < 1e-9); // -1.0 - 0.4/2
    assert!((max_x - 1.2).abs() < 1e-9);
    assert!((min_y - -0.3).abs() < 1e-9);
    assert!((max_y - 0.3).abs() < 1e-9);
}

#[test]
fn courtyard_clearance_follows_the_rule() {
    let smd = vec![pad("1", "smd", 0.0, 0.0, 0.4, 0.6)];
    let th = vec![pad("1", "thru_hole", 0.0, 0.0, 1.5, 1.5)];
    // Explicit wins over everything.
    assert_eq!(
        courtyard_clearance(Some(0.42), Some("bga"), &smd, None),
        0.42
    );
    // package_type mapping.
    assert_eq!(courtyard_clearance(None, Some("bga"), &smd, None), 1.0);
    assert_eq!(courtyard_clearance(None, Some("small"), &smd, None), 0.15);
    assert_eq!(
        courtyard_clearance(None, Some("through_hole"), &smd, None),
        0.5
    );
    assert_eq!(courtyard_clearance(None, Some("smd"), &smd, None), 0.25);
    // Auto: through-hole pad present.
    assert_eq!(courtyard_clearance(None, None, &th, None), 0.5);
    // Auto: sub-0603 body (1.0 x 0.5 mm).
    assert_eq!(
        courtyard_clearance(None, None, &smd, Some((1.0, 0.5))),
        0.15
    );
    // Auto: 0603 itself and larger stay at the SMT default.
    assert_eq!(
        courtyard_clearance(None, None, &smd, Some((1.6, 0.8))),
        0.25
    );
    assert_eq!(courtyard_clearance(None, None, &smd, None), 0.25);
}

#[test]
fn pin1_index_prefers_pad_numbered_one() {
    let pads = vec![
        pad("2", "smd", 0.0, 0.0, 1.0, 1.0),
        pad("1", "smd", 2.0, 0.0, 1.0, 1.0),
    ];
    assert_eq!(pin1_index(&pads), Some(1));
    // No pad numbered "1" falls back to the first pad.
    let pads2 = vec![pad("A1", "smd", 0.0, 0.0, 1.0, 1.0)];
    assert_eq!(pin1_index(&pads2), Some(0));
    assert_eq!(pin1_index(&[]), None);
}

#[test]
fn chamfered_rect_cuts_the_pin1_corner() {
    // Rectangle (0,0)-(10,10), pin 1 nearest the top-left corner.
    let pts = chamfered_rect_points(0.0, 0.0, 10.0, 10.0, 0.0, 0.0, 1.0);
    assert_eq!(pts.len(), 5, "one corner chamfered adds a vertex: {pts:?}");
    // The sharp corner is gone, replaced by two edge points.
    assert!(!pts.iter().any(|&(x, y)| x.abs() < 1e-9 && y.abs() < 1e-9));
    assert!(pts
        .iter()
        .any(|&(x, y)| (x - 0.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9));
    assert!(pts
        .iter()
        .any(|&(x, y)| (x - 1.0).abs() < 1e-9 && (y - 0.0).abs() < 1e-9));
}

#[test]

fn model_sexp_only_with_path() {
    assert_eq!(build_model_sexp(&json!({})), "");
    assert_eq!(build_model_sexp(&json!({ "model": {} })), "");
    let s = build_model_sexp(&json!({ "model": { "path": "x.wrl", "rotate": { "z": 90.0 } } }));
    assert!(s.contains("(model \"x.wrl\""));
    assert!(s.contains("(rotate (xyz 0 0 90)"));
    assert!(s.contains("(scale (xyz 1 1 1)"));
}

#[tokio::test]
async fn oval_drill_edit_preserves_kicad_authored_footprint() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Socket.kicad_mod");
    let original = include_str!("../../../tests/fixtures/socket_kicad10.kicad_mod");
    std::fs::write(&path, original).unwrap();
    let result = handle_edit_footprint_pad(
        &json!({"footprint_path":path.to_string_lossy(), "pad_number":"2",
                "drill":{"shape":"oval","width":0.6,"height":1.6}}),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original.replacen("(drill 1)", "(drill oval 0.6 1.6)", 1)
    );
}

#[tokio::test]
async fn oval_drill_create_edit_readback_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("SLOTS.kicad_mod");
    let created = handle_create_footprint(
        &json!({
            "output": path.to_string_lossy(), "name": "SLOTS",
            "pads": [
                {"number":"1", "type":"thru_hole", "shape":"oval", "x":0.0,"y":0.0,
                 "width":1.0,"height":2.0,"rotation":90.0,
                 "drill":{"shape":"oval","width":0.6,"height":1.6}},
                {"number":"2", "type":"thru_hole", "shape":"circle", "x":3.0,"y":0.0,
                 "width":1.5,"height":1.5,"drill":0.8}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!created.is_error);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("(drill oval 0.6 1.6)"));
    assert!(text.contains("(drill 0.8)"));
    assert!(text.contains("(at 0 0 90)"));
    assert!(text.contains("(layers \"*.Cu\" \"*.Mask\")"));

    let edited = handle_edit_footprint_pad(
        &json!({
            "footprint_path":path.to_string_lossy(),"pad_number":"2",
            "height":2.0,"drill":{"shape":"oval","width":0.6,"height":1.6}
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!edited.is_error);
    let info = handle_get_footprint_info(
        &json!({
            "footprint_path":path.to_string_lossy(),"include_pads":true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &info.content[0] else {
        panic!("expected text");
    };
    let info: serde_json::Value = serde_json::from_str(text).unwrap();
    for pad in info["pads"].as_array().unwrap() {
        assert_eq!(pad["drill"]["shape"], "oval");
        assert_eq!(pad["drill"]["size"], json!({"x":0.6,"y":1.6}));
        assert_eq!(pad["pad_type"], "thru_hole");
    }
    let edited = handle_edit_footprint_pad(
        &json!({
            "footprint_path":path.to_string_lossy(),"pad_number":"1","drill":0.5
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!edited.is_error);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("(at 0 0 90)"));
    assert!(text.contains("(drill 0.5)"));
    assert_eq!(text.matches("(drill oval 0.6 1.6)").count(), 1);
    assert!(parse_sexp(&text).is_ok());
}

#[tokio::test]
async fn oval_drill_invalid_payloads_leave_files_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("INVALID.kicad_mod");
    let initial = handle_create_footprint(
        &json!({
            "output":path.to_string_lossy(),"name":"INVALID",
            "pads":[{"number":"1","type":"thru_hole","shape":"oval",
                     "x":0.0,"y":0.0,"width":1.0,"height":2.0,"drill":0.6}]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!initial.is_error);
    let before = std::fs::read(&path).unwrap();
    for drill in [
        json!(0),
        json!(-0.1),
        json!(null),
        json!("0.6"),
        json!({"shape":"oval","width":0.6}),
        json!({"shape":"circle","width":0.6,"height":1.6}),
        json!({"shape":"oval","width":0.6,"height":2.1}),
        json!({"shape":"oval","width":0.6,"height":1.6,"rotation":90}),
    ] {
        let result = handle_edit_footprint_pad(
            &json!({
                "footprint_path":path.to_string_lossy(),"pad_number":"1","drill":drill
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "accepted invalid drill {drill}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let new_path = tmp.path().join("MUST_NOT_EXIST.kicad_mod");
        let result = handle_create_footprint(
            &json!({
                "output":new_path.to_string_lossy(),"name":"INVALID",
                "pads":[{"number":"1","type":"thru_hole","shape":"oval",
                         "x":0.0,"y":0.0,"width":1.0,"height":2.0,"drill":drill}]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error);
        assert!(!new_path.exists());
    }
    let result = handle_create_footprint(
        &json!({
            "output":tmp.path().join("SMD.kicad_mod").to_string_lossy(),"name":"SMD",
            "pads":[{"number":"1","type":"smd","shape":"rect","x":0.0,"y":0.0,
                     "width":1.0,"height":2.0,"drill":{"shape":"oval","width":0.6,"height":1.6}}]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(!tmp.path().join("SMD.kicad_mod").exists());
}

#[test]
fn oval_drill_schema_accepts_legacy_and_slot_rejects_invalid() {
    let schema = footprint_drill_schema();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    assert!(validator.is_valid(&json!(0.6)));
    assert!(validator.is_valid(&json!({"shape":"oval","width":0.6,"height":1.6})));
    for invalid in [
        json!(0),
        json!(-1),
        json!({"shape":"oval","width":0.6}),
        json!({"shape":"oval","width":0.6,"height":1.6,"offset":0.1}),
    ] {
        assert!(!validator.is_valid(&invalid));
    }
    let defs = tools();
    let create = defs.iter().find(|t| t.name == "create_footprint").unwrap();
    let edit = defs
        .iter()
        .find(|t| t.name == "edit_footprint_pad")
        .unwrap();
    assert_eq!(
        create.input_schema["properties"]["pads"]["items"]["properties"]["drill"],
        schema
    );
    assert_eq!(edit.input_schema["properties"]["drill"], schema);
}

#[test]
fn oval_drill_edit_refuses_existing_offset() {
    let pad = "(pad \"1\" thru_hole oval (at 0 0) (size 1 2) (drill oval 0.6 1.6 (offset 0.1 0)) (layers \"*.Cu\" \"*.Mask\"))";
    let result = edit_footprint_pad_block(pad, &json!({"drill":0.5}), None, None);
    assert!(result.unwrap_err().contains("offset"));
}

#[tokio::test]
async fn create_footprint_emits_courtyard_pin1_and_model() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("TEST.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "TEST_QFN",
        "pads": [
            {"number":"1","type":"smd","shape":"roundrect","x":-1.0,"y":-1.0,"width":0.3,"height":0.6},
            {"number":"2","type":"smd","shape":"roundrect","x":-1.0,"y":1.0,"width":0.3,"height":0.6},
            {"number":"3","type":"smd","shape":"roundrect","x":1.0,"y":0.0,"width":0.3,"height":0.6}
        ],
        "body_width": 2.0, "body_height": 2.0,
        "model": { "path": "${KICAD9_3DMODEL_DIR}/Package.3dshapes/TEST_QFN.wrl" }
    });
    let res = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error);
    let c = std::fs::read_to_string(&out).unwrap();
    assert!(c.contains("F.CrtYd"), "missing courtyard:\n{c}");
    assert!(c.contains("F.SilkS"));
    assert!(c.contains("(fp_poly"), "missing fab chamfer outline");
    assert!(c.contains("(fp_circle"), "missing pin-1 silk dot");
    assert!(c.contains("(property \"Reference\" \"REF**\""));
    assert!(c.contains("(property \"Value\" \"TEST_QFN\""));
    assert!(c.contains("(model \"${KICAD9_3DMODEL_DIR}/Package.3dshapes/TEST_QFN.wrl\""));
    // Round-trips through the S-expression parser.
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "generated footprint doesn't parse"
    );
}

#[test]
fn create_footprint_schema_exposes_pad_layers_rotation_and_roundrect_ratio() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == "create_footprint")
        .expect("library must expose create_footprint");
    let pad_properties = &tool.input_schema["properties"]["pads"]["items"]["properties"];
    assert_eq!(pad_properties["layers"]["items"]["type"], json!("string"));
    assert_eq!(pad_properties["rotation"]["type"], json!("number"));
    assert_eq!(pad_properties["roundrect_rratio"]["maximum"], json!(0.5));
    assert_eq!(
        pad_properties["fabrication_property"]["enum"],
        json!(KICAD_PAD_FABRICATION_PROPERTIES)
    );
}

#[tokio::test]
async fn create_footprint_emits_bottom_layer_rotated_roundrect_pad() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("BOTTOM.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "BOTTOM",
        "pads": [{
            "number": "1",
            "type": "smd",
            "shape": "roundrect",
            "x": -8.075,
            "y": 4.7,
            "width": 2.5,
            "height": 2.55,
            "layers": ["B.Cu", "B.Paste", "B.Mask"],
            "rotation": 180.0,
            "roundrect_rratio": 0.2
        }]
    });

    let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error);
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.contains("(at -8.075 4.7 180)"),
        "missing pad rotation:\n{content}"
    );
    assert!(
        content.contains("(layers \"B.Cu\" \"B.Mask\" \"B.Paste\")"),
        "missing bottom pad layers, in KiCad's order:\n{content}"
    );
    assert!(
        content.contains("(roundrect_rratio 0.2)"),
        "missing roundrect ratio:\n{content}"
    );
}

#[tokio::test]
async fn create_footprint_legacy_pad_payload_remains_front_side() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("LEGACY.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "LEGACY",
        "pads": [{
            "number": "1",
            "type": "smd",
            "shape": "roundrect",
            "x": 0.0,
            "y": 0.0,
            "width": 1.0,
            "height": 1.0
        }]
    });

    let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error);
    let content = std::fs::read_to_string(&out).unwrap();
    let pad = &content[content.find("(pad \"1\"").unwrap()..];
    assert!(pad.contains("(at 0 0)\n"), "{content}");
    assert!(
        pad.contains("(layers \"F.Cu\" \"F.Mask\" \"F.Paste\")"),
        "{content}"
    );
}

#[tokio::test]
async fn create_footprint_rejects_invalid_pad_layer_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("INVALID_LAYER.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "INVALID_LAYER",
        "pads": [{
            "number": "1",
            "type": "smd",
            "shape": "rect",
            "x": 0.0,
            "y": 0.0,
            "width": 1.0,
            "height": 1.0,
            "layers": ["User.MadeUp"]
        }]
    });

    let error = handle_create_footprint(&args, &test_ctx())
        .await
        .expect_err("invalid layer must be rejected");
    assert!(error.to_string().contains("invalid pad layer"));
    assert!(!out.exists());
}

#[tokio::test]
async fn create_footprint_rejects_invalid_roundrect_ratio_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("INVALID_RATIO.kicad_mod");
    let args = json!({
        "output": out.to_string_lossy(),
        "name": "INVALID_RATIO",
        "pads": [{
            "number": "1",
            "type": "smd",
            "shape": "roundrect",
            "x": 0.0,
            "y": 0.0,
            "width": 1.0,
            "height": 1.0,
            "roundrect_rratio": 0.75
        }]
    });

    let error = handle_create_footprint(&args, &test_ctx())
        .await
        .expect_err("invalid roundrect ratio must be rejected");
    assert!(error.to_string().contains("roundrect_rratio"));
    assert!(!out.exists());
}

#[tokio::test]
async fn get_footprint_info_includes_graphics_only_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("inspect.kicad_mod");
    std::fs::write(
        &path,
        r#"(footprint "Inspect" (version 20240108) (generator "pcbnew")
  (layer "F.Cu")
  (descr "Inspection fixture")
  (fp_line (start 0 0) (end 1 0)
    (stroke (width 0.1) (type solid)) (layer "F.SilkS"))
  (fp_poly
    (pts (xy 0 0) (xy 2 0) (xy 2 1))
    (stroke (width 0.05) (type solid)) (fill none) (layer "B.CrtYd"))
  (pad "1" thru_hole circle (at 0 0) (size 1.8 1.8) (drill 1)
    (layers "*.Cu" "*.Mask"))
)
"#,
    )
    .unwrap();

    let default = handle_get_footprint_info(
        &json!({"footprint_path": path.to_string_lossy()}),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &default.content[0] else {
        panic!("expected text");
    };
    let default: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(default["name"], "Inspect");
    assert_eq!(default["pad_count"], 1);
    assert!(
        default.get("graphics").is_none(),
        "default response must stay compact: {default}"
    );
    assert!(default.get("graphic_count").is_none());
    assert!(default.get("pads").is_none());

    let detailed = handle_get_footprint_info(
        &json!({
            "footprint_path": path.to_string_lossy(),
            "include_pads": true,
            "include_graphics": true,
            "graphics_layer": "B.CrtYd"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &detailed.content[0] else {
        panic!("expected text");
    };
    let detailed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(detailed["graphic_count"], 1);
    assert_eq!(detailed["graphics"][0]["type"], "poly");
    assert_eq!(detailed["graphics"][0]["layer"], "B.CrtYd");
    assert_eq!(detailed["graphics"][0]["point_count"], 3);
    assert_eq!(detailed["graphics"][0]["closed"], true);
    assert_eq!(detailed["graphics"][0]["stroke_width_mm"], 0.05);
    assert_eq!(detailed["graphics"][0]["fill"], "none");
    assert_eq!(detailed["coordinate_space"], "footprint_local");
    assert_eq!(detailed["pads"][0]["number"], "1");
    assert_eq!(detailed["pads"][0]["pad_type"], "thru_hole");
    assert_eq!(detailed["pads"][0]["shape"], "circle");
    assert_eq!(detailed["pads"][0]["size"], json!({"x": 1.8, "y": 1.8}));
    assert_eq!(detailed["pads"][0]["drill"]["shape"], "circle");
    assert_eq!(
        detailed["pads"][0]["drill"]["size"],
        json!({"x": 1.0, "y": 1.0})
    );
    assert_eq!(detailed["pads"][0]["copper_layers"][0]["layer"], "*.Cu");

    let invalid = handle_get_footprint_info(
        &json!({
            "footprint_path": path.to_string_lossy(),
            "graphics_layer": "BottomCourtyard"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(invalid.is_error);
    let crate::mcp::protocol::ToolContent::Text { text } = &invalid.content[0] else {
        panic!("expected text");
    };
    let invalid: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(invalid["error"]["kind"], "invalid_argument");
    assert_eq!(invalid["error"]["field"], "graphics_layer");

    let schema = tools()
        .into_iter()
        .find(|tool| tool.name == "get_footprint_info")
        .unwrap()
        .input_schema;
    assert_eq!(schema["properties"]["include_graphics"]["type"], "boolean");
    assert_eq!(schema["properties"]["include_pads"]["type"], "boolean");
    assert_eq!(schema["properties"]["graphics_layer"]["type"], "string");
}

#[tokio::test]
async fn get_footprint_info_refuses_to_silently_drop_an_unreadable_pad() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("malformed-pad.kicad_mod");
    std::fs::write(
        &path,
        r#"(footprint "MalformedPad"
  (layer "F.Cu")
  (pad "1" smd rect (size 1 1) (layers "F.Cu"))
)"#,
    )
    .unwrap();

    let result = handle_get_footprint_info(
        &json!({
            "footprint_path": path.to_string_lossy(),
            "include_pads": true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(result.is_error);
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text");
    };
    assert!(text.contains("Pad '1' has no readable footprint-local position"));
}

#[tokio::test]
async fn get_footprint_info_reads_kicad_style_layout_structurally() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp
        .path()
        .join("USB_C_Receptacle_HRO_TYPE-C-31-M-12.kicad_mod");
    // Reduced from KiCad 10's stock Connector_USB footprint without
    // rewriting the retained nodes. CRLF is intentional: together with
    // KiCad's tabs it reproduces both reported formatting assumptions.
    const KICAD_STYLE_FOOTPRINT: &str = concat!(
        "(footprint \"USB_C_Receptacle_HRO_TYPE-C-31-M-12\"\r\n",
        "\t(version 20260206)\r\n",
        "\t(generator \"pcbnew\")\r\n",
        "\t(generator_version \"10.0\")\r\n",
        "\t(layer \"F.Cu\")\r\n",
        "\t(descr \"USB Type-C receptacle for USB 2.0 and PD, http://www.krhro.com/uploads/soft/180320/1-1P320120243.pdf\")\r\n",
        "\t(tags \"usb usb-c 2.0 pd\")\r\n",
        "\t(fp_line\r\n",
        "\t\t(start -5.32 -5.27)\r\n",
        "\t\t(end -5.32 4.15)\r\n",
        "\t\t(stroke\r\n",
        "\t\t\t(width 0.05)\r\n",
        "\t\t\t(type solid)\r\n",
        "\t\t)\r\n",
        "\t\t(layer \"F.CrtYd\")\r\n",
        "\t\t(uuid \"d939342c-cab3-429e-ad71-738d34173267\")\r\n",
        "\t)\r\n",
        "\t(pad \"\" np_thru_hole circle\r\n",
        "\t\t(at -2.89 -2.6)\r\n",
        "\t\t(size 0.65 0.65)\r\n",
        "\t\t(drill 0.65)\r\n",
        "\t\t(layers \"*.Cu\" \"*.Mask\")\r\n",
        "\t\t(uuid \"e13b4b37-788a-41d5-87ca-c66be43ee32d\")\r\n",
        "\t)\r\n",
        "\t(pad \"\" np_thru_hole circle\r\n",
        "\t\t(at 2.89 -2.6)\r\n",
        "\t\t(size 0.65 0.65)\r\n",
        "\t\t(drill 0.65)\r\n",
        "\t\t(layers \"*.Cu\" \"*.Mask\")\r\n",
        "\t\t(uuid \"4558f1f0-2fa3-46e1-a220-284fa2707bb5\")\r\n",
        "\t)\r\n",
        "\t(pad \"A1\" smd roundrect\r\n",
        "\t\t(at -3.25 -4.045)\r\n",
        "\t\t(size 0.6 1.45)\r\n",
        "\t\t(layers \"F.Cu\" \"F.Mask\" \"F.Paste\")\r\n",
        "\t\t(roundrect_rratio 0.25)\r\n",
        "\t\t(uuid \"245fae56-8b58-4bd8-bbf1-36624dc3fa3e\")\r\n",
        "\t)\r\n",
        "\t(pad \"B12\" smd roundrect\r\n",
        "\t\t(at -3.25 -4.045)\r\n",
        "\t\t(size 0.6 1.45)\r\n",
        "\t\t(layers \"F.Cu\" \"F.Mask\" \"F.Paste\")\r\n",
        "\t\t(roundrect_rratio 0.25)\r\n",
        "\t\t(uuid \"eda87c04-3a59-4e2c-a9bd-f59ed31672aa\")\r\n",
        "\t)\r\n",
        "\t(pad \"SH\" thru_hole oval\r\n",
        "\t\t(at -4.32 -3.13)\r\n",
        "\t\t(size 1 2.1)\r\n",
        "\t\t(drill oval 0.6 1.7)\r\n",
        "\t\t(property pad_prop_mechanical)\r\n",
        "\t\t(layers \"*.Cu\" \"*.Mask\")\r\n",
        "\t\t(remove_unused_layers no)\r\n",
        "\t\t(uuid \"69c3dc88-a37e-49ef-ae4d-497d3191ad5b\")\r\n",
        "\t)\r\n",
        "\t(pad \"SH\" thru_hole oval\r\n",
        "\t\t(at -4.32 1.05)\r\n",
        "\t\t(size 1 1.6)\r\n",
        "\t\t(drill oval 0.6 1.2)\r\n",
        "\t\t(property pad_prop_mechanical)\r\n",
        "\t\t(layers \"*.Cu\" \"*.Mask\")\r\n",
        "\t\t(remove_unused_layers no)\r\n",
        "\t\t(uuid \"5991ca6f-c5c1-4692-8fe9-cf4b7ca50c4b\")\r\n",
        "\t)\r\n",
        "\t(embedded_fonts no)\r\n",
        "\t(model \"${KICAD10_3DMODEL_DIR}/Connector_USB.3dshapes/USB_C_Receptacle_HRO_TYPE-C-31-M-12.step\"\r\n",
        "\t\t(offset\r\n",
        "\t\t\t(xyz 0 0 0)\r\n",
        "\t\t)\r\n",
        "\t\t(scale\r\n",
        "\t\t\t(xyz 1 1 1)\r\n",
        "\t\t)\r\n",
        "\t\t(rotate\r\n",
        "\t\t\t(xyz 0 0 0)\r\n",
        "\t\t)\r\n",
        "\t)\r\n",
        ")\r\n",
    );
    std::fs::write(&path, KICAD_STYLE_FOOTPRINT).unwrap();

    let result = handle_get_footprint_info(
        &json!({
            "footprint_path": path.to_string_lossy(),
            "include_pads": true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text");
    };
    let result: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(result["name"], "USB_C_Receptacle_HRO_TYPE-C-31-M-12");
    assert_eq!(result["pad_count"], 6);
    assert_eq!(result["has_courtyard"], true);
    assert_eq!(result["has_3d_model"], true);
    assert_eq!(result["coordinate_space"], "footprint_local");
    assert_eq!(result["pads"].as_array().unwrap().len(), 6);
    assert_eq!(result["pads"][0]["pad_type"], "np_thru_hole");
    assert_eq!(result["pads"][0]["number"], "");
    assert_eq!(result["pads"][0]["drill"]["shape"], "circle");
    assert_eq!(result["pads"][2]["shape"], "roundrect");
    assert_eq!(result["pads"][3]["number"], "B12");
    assert_eq!(result["pads"][4]["number"], "SH");
    assert_eq!(result["pads"][5]["number"], "SH");
    assert_eq!(result["pads"][4]["drill"]["shape"], "oval");
    assert_eq!(
        result["pads"][4]["drill"]["size"],
        json!({"x": 0.6, "y": 1.7})
    );
}

#[tokio::test]
async fn get_footprint_info_ignores_metadata_that_looks_like_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata-only.kicad_mod");
    std::fs::write(
        &path,
        concat!(
            "(footprint \"MetadataOnly\"\r\n",
            "\t(version 20240108)\r\n",
            "\t(generator \"pcbnew\")\r\n",
            "\t(layer \"F.Cu\")\r\n",
            "\t(descr \"mentions (pad fake), F.CrtYd, and (model fake)\")\r\n",
            ")\r\n",
        ),
    )
    .unwrap();

    let result = handle_get_footprint_info(
        &json!({"footprint_path": path.to_string_lossy()}),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text");
    };
    let result: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(result["pad_count"], 0);
    assert_eq!(result["has_courtyard"], false);
    assert_eq!(result["has_3d_model"], false);
}

/// A missing argument must stay a structured `invalid_argument` naming the
/// field, rather than collapsing into a generic `handler_error`.
///
/// Seven handlers here did `map_err(|e| anyhow!("{:?}", e))?` on the
/// `CallToolResult` that `require_str` returns, debug-formatting a typed error
/// into a string and losing the taxonomy on the way out. `pcb_components`
/// already returned the result directly. This is what lets a caller tell "you
/// forgot an argument" from "the tool tried and failed".
mod argument_error_kind_tests {
    use super::super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use serde_json::json;
    use std::sync::Arc;

    fn ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        ))
    }

    #[tokio::test]
    async fn missing_required_arguments_report_invalid_argument_with_the_field() {
        // (tool, args that satisfy everything *except* the field under test,
        //  the field it should name)
        //
        // The path arguments are still supplied deliberately: this test is
        // about `require_str`, and a handler that reads a path first would
        // otherwise fail on that instead, making the assertion vacuous. Since
        // #194 those path failures are themselves `invalid_argument`, so the
        // wrong one would still *pass* the kind check while naming a different
        // field — which is exactly why the field is asserted too. Path
        // arguments get their own coverage in
        // `mcp::handler::path_argument_taxonomy_tests`.
        let cases = [
            ("get_symbol_info", json!({}), "lib_id"),
            ("get_footprint_info", json!({}), "footprint_path"),
            (
                "register_footprint_library",
                json!({ "library_path": "/tmp/x.pretty", "scope": "global" }),
                "nickname",
            ),
            (
                "register_symbol_library",
                json!({ "library_path": "/tmp/x.kicad_sym", "scope": "global" }),
                "nickname",
            ),
        ];

        for (tool_name, args, field) in cases {
            let def = tools()
                .into_iter()
                .find(|t| t.name == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} is registered"));

            // The old pattern turned this into an Err, so the unwrap itself is
            // part of the assertion.
            let result = (def.handler)(&args, ctx())
                .await
                .unwrap_or_else(|e| panic!("{tool_name} must not bubble an anyhow error: {e}"));

            assert!(result.is_error, "{tool_name}: missing argument must fail");

            let text = match result.content.first() {
                Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
                other => panic!("{tool_name}: expected text, got {other:?}"),
            };
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool_name}: {e}: {text}"));

            assert_eq!(
                parsed["error"]["kind"], "invalid_argument",
                "{tool_name} must report a typed argument error, not a \
                 debug-formatted handler_error: {text}"
            );
            assert_eq!(
                parsed["error"]["field"], field,
                "{tool_name} must name the missing field: {text}"
            );
        }
    }
}

/// A tool that declares an argument required must refuse the call when it is
/// absent — not substitute a value, do the work, and report success.
///
/// `create_footprint` is the reason this module exists. With `pads` omitted it
/// produced a footprint with no pads, no courtyard, no silkscreen and no fab
/// outline, named `"Footprint"` because `name` defaulted too, and committed it
/// with `write_atomic` — an unconditional replace with no if-unchanged check.
/// Measured against a real 0402 resistor footprint: **805 bytes and 2 pads
/// became 121 bytes and 0 pads**, and the call returned
/// `{"success": true, "pad_count": 0}` (#218).
mod required_argument_tests {
    use super::super::*;
    use crate::tools::ServerConfig;
    use serde_json::json;
    use std::sync::Arc;

    fn ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
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
        ))
    }

    fn error_field(result: &CallToolResult) -> String {
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        assert_eq!(
            parsed["error"]["kind"], "invalid_argument",
            "must be a typed argument error: {text}"
        );
        parsed["error"]["field"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn call(tool_name: &str, args: serde_json::Value) -> CallToolResult {
        let def = tools()
            .into_iter()
            .find(|t| t.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is registered"));
        (def.handler)(&args, ctx())
            .await
            .unwrap_or_else(|e| panic!("{tool_name} must not bubble anyhow: {e}"))
    }

    /// The property that matters: a refused call leaves the target file exactly
    /// as it was. Asserting only the error would still pass if the write
    /// happened first.
    #[tokio::test]
    async fn create_footprint_without_pads_refuses_and_leaves_the_file_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("R_0402_1005Metric.kicad_mod");
        let original = "(footprint \"R_0402_1005Metric\"\n\t(layer \"F.Cu\")\n\t\
             (pad \"1\" smd roundrect (at -0.51 0) (size 0.54 0.64) \
             (layers \"F.Cu\" \"F.Paste\" \"F.Mask\"))\n\t\
             (pad \"2\" smd roundrect (at 0.51 0) (size 0.54 0.64) \
             (layers \"F.Cu\" \"F.Paste\" \"F.Mask\"))\n)\n";
        std::fs::write(&path, original).unwrap();

        let result = call(
            "create_footprint",
            json!({ "output": path.display().to_string(), "name": "R_0402_1005Metric" }),
        )
        .await;

        assert!(result.is_error, "a footprint with no pads must be refused");
        assert_eq!(error_field(&result), "pads");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "the existing footprint must survive a refused call untouched"
        );
    }

    #[tokio::test]
    async fn create_footprint_rejects_invalid_silkscreen_before_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.kicad_mod");
        let original = include_str!("../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        std::fs::write(&path, original).unwrap();

        for mode in [json!("fill"), json!(true), json!(null)] {
            let result = call(
                "create_footprint",
                json!({
                    "output": path, "name": "Replacement", "replace_existing": true,
                    "pads": [], "silkscreen": mode
                }),
            )
            .await;
            assert!(result.is_error, "{mode}");
            assert_eq!(error_field(&result), "silkscreen");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn create_footprint_without_a_name_refuses_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.kicad_mod");
        let result = call(
            "create_footprint",
            json!({ "output": path.display().to_string(), "pads": [] }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(error_field(&result), "name");
        assert!(
            !path.exists(),
            "nothing should be created for a refused call"
        );
    }

    /// An empty `pads` array is a different thing from an absent one: the
    /// caller said "no pads", which is a coherent request for a mechanical
    /// footprint. Only the absent case is a mistake.
    #[tokio::test]
    async fn an_explicitly_empty_pads_array_is_still_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mech.kicad_mod");
        let result = call(
            "create_footprint",
            json!({ "output": path.display().to_string(), "name": "Mech", "pads": [] }),
        )
        .await;
        assert!(!result.is_error, "an explicit empty pad list is a request");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn create_footprint_rejects_a_malformed_pad_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.kicad_mod");
        let original = b"(footprint \"Existing\"\r\n\t(layer \"F.Cu\")\r\n)\r\n";
        std::fs::write(&path, original).unwrap();

        let result = call(
            "create_footprint",
            json!({
                "output": path.display().to_string(),
                "name": "Replacement",
                "pads": [{}]
            }),
        )
        .await;

        assert!(result.is_error, "an invented origin pad must be refused");
        assert_eq!(error_field(&result), "pads[0].number");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the existing footprint must remain byte-identical"
        );
    }

    #[tokio::test]
    async fn create_symbol_refuses_a_missing_name_or_reference_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.kicad_sym");
        std::fs::write(&path, "(kicad_symbol_lib\n)\n").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        for (args, field) in [
            (json!({ "reference_prefix": "R" }), "name"),
            (json!({ "name": "MyPart" }), "reference_prefix"),
        ] {
            let mut args = args;
            args["library_path"] = json!(path.display().to_string());
            let result = call("create_symbol", args).await;
            assert!(result.is_error, "must refuse when {field} is absent");
            assert_eq!(error_field(&result), field);
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "no symbol should have been appended"
        );
    }

    #[tokio::test]
    async fn create_symbol_rejects_a_unit_without_pins_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.kicad_sym");
        let original = b"(kicad_symbol_lib\r\n\t(version 20240108)\r\n)\r\n";
        std::fs::write(&path, original).unwrap();

        let result = call(
            "create_symbol",
            json!({
                "library_path": path.display().to_string(),
                "name": "BrokenMulti",
                "reference_prefix": "U",
                "units": [{}]
            }),
        )
        .await;

        assert!(result.is_error, "a pin-less invented unit must be refused");
        assert_eq!(error_field(&result), "units[0].pins");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the existing symbol library must remain byte-identical"
        );
    }

    #[tokio::test]
    async fn create_symbol_names_the_malformed_nested_pin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.kicad_sym");
        let result = call(
            "create_symbol",
            json!({
                "library_path": path.display().to_string(),
                "name": "BrokenPin",
                "reference_prefix": "U",
                "units": [{ "pins": [
                    { "number": "1", "name": "A", "type": "input" },
                    { "number": "2", "type": "output" }
                ] }]
            }),
        )
        .await;

        assert!(result.is_error);
        assert_eq!(error_field(&result), "units[0].pins[1].name");
        assert!(!path.exists(), "no library may be created on rejection");
    }

    #[test]
    fn model_search_matches_kicad_plugin_formats() {
        for file in [
            "a.step",
            "a.STP",
            "a.stpZ",
            "a.stp.gz",
            "a.step.gz",
            "a.igs",
            "a.iges",
            "a.wrl",
            "a.wrz",
            "a.x3d",
            "a.idf",
            "a.emn",
        ] {
            assert!(is_3d_model_file(Path::new(file)), "{file}");
        }
        for file in ["a.obj", "a.step.bak", "a.gz", "notes.txt"] {
            assert!(!is_3d_model_file(Path::new(file)), "{file}");
        }
    }

    #[test]
    fn model_search_paths_round_trip_for_unmapped_install_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model.step.gz"), b"model").unwrap();
        let mut results = Vec::new();
        collect_3d_models_in_dir(
            dir.path(),
            "kicad",
            "${UNMAPPED_3DMODEL_DIR}",
            "",
            10,
            &mut results,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["format"], "step.gz");
        for result in results {
            assert_eq!(
                expand_lib_uri(result["kicad_path"].as_str().unwrap(), None).unwrap(),
                PathBuf::from(result["path"].as_str().unwrap())
            );
        }
    }

    #[tokio::test]
    async fn create_symbol_escapes_quotes_and_backslashes_in_symbol_and_unit_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escaped.kicad_sym");
        let lib_str = path.to_string_lossy().to_string();
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let dispatch = |tool: &str, arguments: serde_json::Value| {
            let h = &handler;
            let req = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            });
            async move {
                let resp = h.handle_message(req).await.unwrap();
                let res = serde_json::to_value(resp.result.unwrap()).unwrap();
                let is_error = res["isError"].as_bool().unwrap_or(false);
                let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
                (is_error, text)
            }
        };

        // 1. Quote in name (single-unit) must produce valid S-expression and round-trip.
        let (is_err, text) = dispatch(
            "create_symbol",
            json!({
                "library_path": lib_str,
                "name": "A\"B",
                "reference_prefix": "U",
                "pins": [{"number": "1", "name": "P1", "type": "passive"}]
            }),
        )
        .await;
        assert!(!is_err, "create_symbol A\"B failed: {text}");

        // 2. Backslash + 'n' in name (single-unit) must stay literal `\n`, not newline.
        let (is_err, text) = dispatch(
            "create_symbol",
            json!({
                "library_path": lib_str,
                "name": "A\\nB",
                "reference_prefix": "U",
                "pins": [{"number": "1", "name": "P1", "type": "passive"}]
            }),
        )
        .await;
        assert!(!is_err, "create_symbol A\\nB failed: {text}");

        // Second identical call must be refused as a duplicate.
        let (is_err, text) = dispatch(
            "create_symbol",
            json!({
                "library_path": lib_str,
                "name": "A\\nB",
                "reference_prefix": "U",
                "pins": [{"number": "1", "name": "P1", "type": "passive"}]
            }),
        )
        .await;
        assert!(is_err, "duplicate A\\nB should be refused: {text}");

        // 3. Multi-unit with quotes and backslashes.
        let (is_err, text) = dispatch(
            "create_symbol",
            json!({
                "library_path": lib_str,
                "name": "M\"U\\n1",
                "reference_prefix": "U",
                "units": [{"pins": [{"number": "1", "name": "IN", "type": "input"}]}],
                "power_pins": [{"number": "2", "name": "VCC", "type": "power_in", "x": 0.0, "y": 2.54, "angle": 270.0, "length": 2.54}]
            }),
        )
        .await;
        assert!(!is_err, "create_symbol multi-unit failed: {text}");

        // 4. Triangular glyph with power split (unit 1 + unit 2).
        let (is_err, text) = dispatch(
            "create_symbol",
            json!({
                "library_path": lib_str,
                "name": "T\"P\\n1",
                "reference_prefix": "U",
                "glyph": "opamp",
                "pins": [
                    {"number": "1", "name": "IN+", "type": "input"},
                    {"number": "2", "name": "IN-", "type": "input"},
                    {"number": "3", "name": "OUT", "type": "output"},
                    {"number": "4", "name": "VCC", "type": "power_in"},
                    {"number": "5", "name": "GND", "type": "power_in"}
                ]
            }),
        )
        .await;
        assert!(!is_err, "create_symbol split-power failed: {text}");

        let content = std::fs::read_to_string(&path).unwrap();
        let root = parse_sexp(&content).expect("written library must parse");
        let (is_err, list_text) = dispatch(
            "list_symbols_in_library",
            json!({ "library_path": lib_str }),
        )
        .await;
        assert!(!is_err, "list_symbols_in_library failed: {list_text}");
        let list_json: serde_json::Value = serde_json::from_str(&list_text).unwrap();
        assert_eq!(
            list_json["symbols"],
            json!(["A\"B", "A\\nB", "M\"U\\n1", "T\"P\\n1"])
        );

        // Verify unit sub-symbol names also unescape to `<name>_U_1`.
        let unit_names: Vec<&str> = root
            .find_all("symbol")
            .into_iter()
            .flat_map(|s| s.find_all("symbol"))
            .filter_map(|u| u.get(1).and_then(|n| n.as_str()))
            .collect();
        assert!(unit_names.contains(&"A\"B_0_1"), "got {unit_names:?}");
        assert!(unit_names.contains(&"A\\nB_0_1"), "got {unit_names:?}");
        assert!(unit_names.contains(&"M\"U\\n1_1_1"), "got {unit_names:?}");
        assert!(unit_names.contains(&"M\"U\\n1_2_1"), "got {unit_names:?}");
        assert!(unit_names.contains(&"T\"P\\n1_1_1"), "got {unit_names:?}");
        assert!(unit_names.contains(&"T\"P\\n1_2_1"), "got {unit_names:?}");

        // delete_symbol must find and delete symbols with quotes and backslashes.
        for sym in ["A\"B", "A\\nB"] {
            let (is_err, del_text) = dispatch(
                "delete_symbol",
                json!({ "library_path": lib_str, "symbol_name": sym }),
            )
            .await;
            assert!(!is_err, "delete_symbol {sym} failed: {del_text}");
        }
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            top_level_symbol_names(&after).unwrap(),
            vec!["M\"U\\n1", "T\"P\\n1"]
        );
    }

    #[tokio::test]
    async fn search_3d_models_finds_project_models_and_respects_scope_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("3dmodels");
        std::fs::create_dir_all(models.join("Resistor_SMD.3dshapes")).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/models-3d");
        std::fs::copy(
            fixtures.join("resistor.step"),
            models.join("Resistor_SMD.3dshapes/R_0805.step"),
        )
        .unwrap();
        std::fs::copy(
            fixtures.join("adjustable_rx2v4.wrl"),
            models.join("Connector.wrl"),
        )
        .unwrap();
        std::fs::write(models.join("notes.txt"), "ignored").unwrap();

        let json_of = |r: &CallToolResult| -> serde_json::Value {
            let crate::mcp::protocol::ToolContent::Text { text } = &r.content[0] else {
                panic!("expected text result");
            };
            serde_json::from_str(text).unwrap()
        };

        let res = call(
            "search_3d_models",
            json!({
                "query": "",
                "project_dir": dir.path(),
                "scope": "project"
            }),
        )
        .await;
        assert!(!res.is_error, "{:?}", res);
        let parsed = json_of(&res);
        assert_eq!(parsed["count"], 2);
        assert_eq!(parsed["truncated"], false);
        for result in parsed["results"].as_array().unwrap() {
            assert_eq!(
                expand_lib_uri(result["kicad_path"].as_str().unwrap(), Some(dir.path())).unwrap(),
                PathBuf::from(result["path"].as_str().unwrap())
            );
        }
        let paths: Vec<&str> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kicad_path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"${KIPRJMOD}/3dmodels/Connector.wrl"));
        assert!(paths.contains(&"${KIPRJMOD}/3dmodels/Resistor_SMD.3dshapes/R_0805.step"));

        // Case-insensitive substring match + limit truncation
        let filtered = call(
            "search_3d_models",
            json!({
                "query": "r_0805",
                "project_dir": dir.path(),
                "scope": "project"
            }),
        )
        .await;
        let filtered_json = json_of(&filtered);
        assert_eq!(filtered_json["count"], 1);
        assert_eq!(filtered_json["results"][0]["format"], "step");

        let limited = call(
            "search_3d_models",
            json!({
                "query": "",
                "project_dir": dir.path(),
                "scope": "project",
                "limit": 1
            }),
        )
        .await;
        let limited_json = json_of(&limited);
        assert_eq!(limited_json["count"], 1);
        assert_eq!(limited_json["truncated"], true);

        // Missing project_dir with scope=project and invalid scope are refused.
        let missing_proj = call(
            "search_3d_models",
            json!({ "query": "R", "scope": "project" }),
        )
        .await;
        assert!(missing_proj.is_error);
        assert_eq!(error_field(&missing_proj), "project_dir");

        let bad_scope = call(
            "search_3d_models",
            json!({ "query": "R", "scope": "invalid" }),
        )
        .await;
        assert!(bad_scope.is_error);
        assert_eq!(error_field(&bad_scope), "scope");

        let bad_proj_type = call(
            "search_3d_models",
            json!({ "query": "R", "project_dir": 123 }),
        )
        .await;
        assert!(bad_proj_type.is_error);
        assert_eq!(error_field(&bad_proj_type), "project_dir");
    }
}

mod create_footprint_replace_tests {
    use super::super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use serde_json::json;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
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
            Arc::new(ToolRouter::new()),
        )
    }

    const KICAD10_FIXTURE: &str =
        include_str!("../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");

    fn one_pad(name: &str) -> serde_json::Value {
        json!({
            "output": "",
            "name": name,
            "pads": [{
                "number": "1",
                "type": "smd",
                "shape": "rect",
                "x": 0.0,
                "y": 0.0,
                "width": 1.0,
                "height": 1.0
            }]
        })
    }

    fn error_json(result: &CallToolResult) -> serde_json::Value {
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text,
            other => panic!("expected text content, got {other:?}"),
        };
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    #[tokio::test]
    async fn create_footprint_refuses_existing_file_by_default_and_leaves_it_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("C_0603_1608Metric.kicad_mod");
        std::fs::write(&path, KICAD10_FIXTURE).unwrap();

        let mut args = one_pad("Other");
        args["output"] = json!(path.display().to_string());
        let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();

        assert!(
            result.is_error,
            "existing footprint must be refused by default"
        );
        let parsed = error_json(&result);
        assert_eq!(parsed["error"]["kind"], "conflict");
        assert!(
            parsed["message"]
                .as_str()
                .unwrap_or("")
                .contains("already exists"),
            "{parsed}"
        );
        assert_eq!(
            parsed["error"]["paths"],
            json!([path.display().to_string()])
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            KICAD10_FIXTURE.as_bytes(),
            "existing fixture must stay byte-identical"
        );
    }

    #[tokio::test]
    async fn create_footprint_replaces_existing_file_when_replace_existing_is_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("C_0603_1608Metric.kicad_mod");
        std::fs::write(&path, KICAD10_FIXTURE).unwrap();

        let mut args = one_pad("Replaced");
        args["output"] = json!(path.display().to_string());
        args["replace_existing"] = json!(true);
        let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();

        assert!(
            !result.is_error,
            "replace_existing=true must succeed: {result:?}"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("(footprint \"Replaced\""), "{written}");
    }

    #[tokio::test]
    async fn create_footprint_rejects_non_boolean_replace_existing_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("C_0603_1608Metric.kicad_mod");
        std::fs::write(&path, KICAD10_FIXTURE).unwrap();

        let mut args = one_pad("Other");
        args["output"] = json!(path.display().to_string());
        args["replace_existing"] = json!("yes");
        let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();

        assert!(result.is_error);
        let parsed = error_json(&result);
        assert_eq!(parsed["error"]["kind"], "invalid_argument");
        assert_eq!(parsed["error"]["field"], "replace_existing");
        assert_eq!(std::fs::read(&path).unwrap(), KICAD10_FIXTURE.as_bytes());
    }

    #[tokio::test]
    async fn create_footprint_replace_existing_refuses_concurrent_external_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("C_0603_1608Metric.kicad_mod");
        std::fs::write(&path, KICAD10_FIXTURE).unwrap();

        let editor_save = "(footprint \"FromFootprintEditor\"\n  (layer \"F.Cu\")\n)\n";
        crate::tools::write_hook::set(editor_save.to_string());

        let mut args = one_pad("Replaced");
        args["output"] = json!(path.display().to_string());
        args["replace_existing"] = json!(true);
        let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();

        assert!(!crate::tools::write_hook::armed(), "write_hook must fire");
        assert!(result.is_error, "concurrent external save must be refused");
        let parsed = error_json(&result);
        assert_eq!(parsed["error"]["kind"], "conflict");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            editor_save,
            "the concurrent editor save must be preserved"
        );
    }

    #[tokio::test]
    async fn create_footprint_refuses_concurrent_creation_before_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appeared.kicad_mod");

        let external = "(footprint \"Appeared\"\n  (layer \"F.Cu\")\n)\n";
        crate::tools::write_hook::set(external.to_string());

        let mut args = one_pad("New");
        args["output"] = json!(path.display().to_string());
        let result = handle_create_footprint(&args, &test_ctx()).await.unwrap();

        assert!(!crate::tools::write_hook::armed(), "write_hook must fire");
        assert!(result.is_error, "concurrent file creation must be refused");
        let parsed = error_json(&result);
        assert_eq!(parsed["error"]["kind"], "conflict");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            external,
            "the concurrently created file must be preserved"
        );
    }
}
