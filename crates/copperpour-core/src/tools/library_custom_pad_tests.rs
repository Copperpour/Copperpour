//! `edit_footprint_pad`'s `roundrect_rratio` and `custom_shape`, and custom
//! pads in `create_footprint` and `get_footprint_info` (#675).

use super::*;
use crate::router::ToolRouter;
use crate::tools::{ServerConfig, ToolContext};
use std::sync::Arc;

// KiCad 10.0.8 wrote each of these; provenance in
// tests/fixtures/custom_pad_edit_kicad10.README.md.
const BEFORE: &str = include_str!("../../tests/fixtures/custom_pad_edit_before_kicad10.kicad_mod");
const RATIO: &str = include_str!("../../tests/fixtures/custom_pad_edit_ratio_kicad10.kicad_mod");
const CUSTOM: &str = include_str!("../../tests/fixtures/custom_pad_edit_custom_kicad10.kicad_mod");
const NAME: &str = "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";

fn ctx() -> ToolContext {
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

/// A copy of `content` in a temporary library.
fn footprint(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{NAME}.kicad_mod"));
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

fn body(result: &CallToolResult) -> serde_json::Value {
    let text = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    serde_json::from_str(&text).unwrap()
}

async fn edit(path: &std::path::Path, arguments: serde_json::Value) -> (bool, serde_json::Value) {
    let mut arguments = arguments;
    arguments["footprint_path"] = json!(path);
    let result = handle_edit_footprint_pad(&arguments, &ctx()).await.unwrap();
    (result.is_error, body(&result))
}

/// KiCad gives the four standard fields new UUIDs on every load, so the
/// comparison masks UUIDs and keeps everything else byte for byte.
fn without_uuids(text: &str) -> String {
    regex::Regex::new(r#"\(uuid "[^"]*"\)"#)
        .unwrap()
        .replace_all(text, "(uuid)")
        .into_owned()
}

/// The cross and circle KiCad drew in `custom`, as `custom_shape`.
fn cross_and_circle() -> serde_json::Value {
    let cross = [
        (-1.2, -0.3),
        (-0.3, -0.3),
        (-0.3, -1.2),
        (0.3, -1.2),
        (0.3, -0.3),
        (1.2, -0.3),
        (1.2, 0.3),
        (0.3, 0.3),
        (0.3, 1.2),
        (-0.3, 1.2),
        (-0.3, 0.3),
        (-1.2, 0.3),
    ];
    json!({
        "anchor": "rect",
        "primitives": [
            {
                "type": "poly",
                "points": cross.iter().map(|(x, y)| json!({ "x": x, "y": y })).collect::<Vec<_>>(),
                "stroke_width_mm": 0.0,
                "fill": "solid"
            },
            {
                "type": "circle",
                "center": { "x": 0.9, "y": 0.9 },
                "radius_mm": 0.25,
                "stroke_width_mm": 0.0,
                "fill": "solid"
            }
        ]
    })
}

/// With `match_all`, every pad with the number gets the ratio, reported once
/// when they agree; a pad of another shape among them refuses the call before
/// anything is written (#694 review).
#[tokio::test]
async fn match_all_sets_the_ratio_on_every_matching_pad() {
    // Pad 3, a roundrect like pad 2, renumbered to 2.
    let two_roundrects = BEFORE.replacen("(pad \"3\" smd roundrect", "(pad \"2\" smd roundrect", 1);
    let (_dir, path) = footprint(&two_roundrects);
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "2", "roundrect_rratio": 0.1, "match_all": true }),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["updated_count"], 2, "{body}");
    assert_eq!(
        body["roundrect_rratio"],
        json!({ "previous": 0.25, "current": 0.1 })
    );
    let written = std::fs::read_to_string(&path).unwrap();
    let root = parse_sexp(&written).unwrap();
    let ratios = root
        .find_all("pad")
        .into_iter()
        .filter(|pad| pad.get(1).and_then(SexpNode::as_str) == Some("2"))
        .map(|pad| {
            pad.find("roundrect_rratio")
                .and_then(|ratio| ratio.get_f64(1))
        })
        .collect::<Vec<_>>();
    assert_eq!(ratios, [Some(0.1), Some(0.1)]);

    // The rect exposed pad 21, renumbered to 2 as well.
    let mixed = two_roundrects.replacen("(pad \"21\" smd rect", "(pad \"2\" smd rect", 1);
    let (_dir, path) = footprint(&mixed);
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "2", "roundrect_rratio": 0.1, "match_all": true }),
    )
    .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["field"], "roundrect_rratio", "{body}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), mixed);
}

/// Values finer than KiCad's 1 nm grid, such as a computed `0.1 + 0.2` or a
/// 7-decimal radius, are written rounded, as `create_footprint` and KiCad's
/// editor write them; the answer reads the rounded shape back off the pad
/// (#694 review).
#[tokio::test]
async fn a_custom_shape_finer_than_a_nanometre_is_written_rounded() {
    let (_dir, path) = footprint(BEFORE);
    let (is_error, body) = edit(
        &path,
        json!({
            "pad_number": "21", "shape": "custom",
            "custom_shape": {
                "anchor": "rect",
                "primitives": [
                    {
                        "type": "poly",
                        "points": [
                            { "x": -1.2, "y": -(0.1 + 0.2) },
                            { "x": 1.2, "y": -0.3 },
                            { "x": 1.2, "y": 0.3 },
                            { "x": -1.2, "y": 0.3 }
                        ],
                        "stroke_width_mm": 0.0,
                        "fill": "solid"
                    },
                    {
                        "type": "circle",
                        "center": { "x": 0.9, "y": 0.9 },
                        "radius_mm": 0.1234567,
                        "stroke_width_mm": 0.0,
                        "fill": "solid"
                    }
                ]
            }
        }),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["custom_shape"]["primitives"][0]["points"][0]["y"],
        -0.3
    );
    assert_eq!(body["custom_shape"]["primitives"][1]["radius_mm"], 0.123457);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("(xy -1.2 -0.3)"), "{written}");
    assert!(written.contains("(end 1.023457 0.9)"), "{written}");
}

/// A new corner ratio is written as KiCad writes it, and reported with its
/// previous and current value.
#[tokio::test]
async fn a_corner_ratio_is_written_as_kicad_writes_it() {
    let (_dir, path) = footprint(BEFORE);
    let (is_error, body) = edit(&path, json!({ "pad_number": "2", "roundrect_rratio": 0.1 })).await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["roundrect_rratio"],
        json!({ "previous": 0.25, "current": 0.1 })
    );
    assert_eq!(
        without_uuids(&std::fs::read_to_string(&path).unwrap()),
        without_uuids(RATIO)
    );
}

/// A rect pad made custom is the pad KiCad writes: the shape token, then
/// `(options …)` and `(primitives …)` in its layout and line endings. The
/// answer reads the shape back off the pad as written.
#[tokio::test]
async fn a_pad_made_custom_is_written_as_kicad_writes_it() {
    let (_dir, path) = footprint(BEFORE);
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "21", "shape": "custom", "custom_shape": cross_and_circle() }),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        without_uuids(&std::fs::read_to_string(&path).unwrap()),
        without_uuids(CUSTOM)
    );
    let mut expected = cross_and_circle();
    expected["clearance"] = json!("outline");
    assert_eq!(body["custom_shape"], expected);
    assert_eq!(body["shape"], "custom");
}

/// A custom pad is redrawn with `custom_shape` alone: its whole primitive list
/// is replaced, and nothing else in the file changes.
#[tokio::test]
async fn a_custom_pad_is_redrawn_without_a_shape_argument() {
    let (_dir, path) = footprint(CUSTOM);
    let mut smaller = cross_and_circle();
    smaller["primitives"].as_array_mut().unwrap().pop();
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "21", "custom_shape": smaller }),
    )
    .await;
    assert!(!is_error, "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("(gr_circle\r\n\t\t\t\t(center 0.9 0.9)"));
    // Without the circle, the file is KiCad's custom pad minus those lines.
    let circle = "\t\t\t(gr_circle\r\n\t\t\t\t(center 0.9 0.9)\r\n\t\t\t\t(end 1.15 0.9)\r\n\t\t\t\t(width 0)\r\n\t\t\t\t(fill yes)\r\n\t\t\t)\r\n";
    assert!(CUSTOM.contains(circle));
    assert_eq!(written, CUSTOM.replace(circle, ""));
}

/// A custom pad made roundrect loses its options and primitives and gets the
/// ratio asked for.
#[tokio::test]
async fn a_custom_pad_made_roundrect_takes_the_ratio() {
    let (_dir, path) = footprint(BEFORE);
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "1", "shape": "roundrect", "roundrect_rratio": 0.2 }),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["roundrect_rratio"]["current"], 0.2);
    let written = std::fs::read_to_string(&path).unwrap();
    let root = parse_sexp(&written).unwrap();
    let pad = root
        .find_all("pad")
        .into_iter()
        .find(|pad| pad.get(1).and_then(SexpNode::as_str) == Some("1"))
        .unwrap();
    assert_eq!(pad.get(3).and_then(SexpNode::as_str), Some("roundrect"));
    assert!(pad.find("options").is_none() && pad.find("primitives").is_none());
    assert_eq!(
        pad.find("roundrect_rratio").and_then(|r| r.get_f64(1)),
        Some(0.2)
    );
}

/// What does not fit the pad is refused before anything is written.
#[tokio::test]
async fn mismatched_shapes_are_refused_before_writing() {
    for (arguments, field, reason) in [
        (
            json!({ "pad_number": "21", "roundrect_rratio": 0.1 }),
            "roundrect_rratio",
            "pad '21' is rect, not roundrect",
        ),
        (
            json!({ "pad_number": "2", "shape": "rect", "roundrect_rratio": 0.1 }),
            "roundrect_rratio",
            "shape is 'rect'",
        ),
        (
            json!({ "pad_number": "2", "roundrect_rratio": 0.6 }),
            "roundrect_rratio",
            "outside 0..0.5",
        ),
        (
            json!({ "pad_number": "2", "custom_shape": cross_and_circle() }),
            "custom_shape",
            "pad '2' is roundrect, not custom",
        ),
        (
            json!({ "pad_number": "21", "shape": "custom" }),
            "custom_shape",
            "needs custom_shape",
        ),
        (
            json!({ "pad_number": "21", "shape": "rect", "custom_shape": cross_and_circle() }),
            "custom_shape",
            "shape is 'rect'",
        ),
        (
            json!({
                "pad_number": "21", "shape": "custom",
                "custom_shape": { "anchor": "rect", "primitives": [] }
            }),
            "custom_shape.primitives",
            "at least one primitive",
        ),
        (
            json!({
                "pad_number": "21", "shape": "custom",
                "custom_shape": { "anchor": "rect", "primitives": [{
                    "type": "line", "start": { "x": 0, "y": 0 }, "end": { "x": 0, "y": 0 },
                    "stroke_width_mm": 0.1
                }] }
            }),
            "custom_shape.primitives[0]",
            "must differ",
        ),
    ] {
        let (_dir, path) = footprint(BEFORE);
        let (is_error, body) = edit(&path, arguments.clone()).await;
        assert!(is_error, "{arguments}: {body}");
        assert_eq!(body["error"]["field"], field, "{arguments}: {body}");
        assert!(
            body["error"]["reason"].as_str().unwrap().contains(reason),
            "{arguments}: {body}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            BEFORE,
            "{arguments}"
        );
    }
}

/// A pad written on one line, as older writers and `create_footprint` do,
/// gets its options and primitives inline.
#[tokio::test]
async fn a_pad_on_one_line_gets_its_custom_shape_inline() {
    let old = "(footprint \"P\" (version 20240108) (layer \"F.Cu\")\n  (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\"))\n)\n";
    let (_dir, path) = footprint(old);
    let (is_error, body) = edit(
        &path,
        json!({ "pad_number": "1", "shape": "custom", "custom_shape": cross_and_circle() }),
    )
    .await;
    assert!(!is_error, "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    let pad_line = written.lines().find(|line| line.contains("(pad")).unwrap();
    assert!(
        pad_line.contains(
            "(options (clearance outline) (anchor rect)) (primitives (gr_poly (pts (xy -1.2 -0.3)"
        ),
        "{pad_line}"
    );
    assert_eq!(written.lines().count(), old.lines().count());
}

/// `get_footprint_info` returns a custom pad's shape in the form the tools
/// take, and nothing extra on other pads.
#[tokio::test]
async fn footprint_info_returns_a_custom_pads_shape() {
    let (_dir, path) = footprint(CUSTOM);
    let result = handle_get_footprint_info(
        &json!({ "footprint_path": path, "include_pads": true }),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let body = body(&result);
    let pads = body["pads"].as_array().unwrap();
    let pad = |number: &str| {
        pads.iter()
            .find(|pad| pad["number"] == number)
            .unwrap_or_else(|| panic!("pad {number}"))
    };
    let mut expected = cross_and_circle();
    expected["clearance"] = json!("outline");
    assert_eq!(pad("21")["custom_shape"], expected);
    assert_eq!(pad("1")["custom_shape"]["anchor"], "circle");
    assert!(pad("2").get("custom_shape").is_none());
}

/// `create_footprint` writes a custom pad that reads back with the shape
/// given, and refuses a custom pad without one.
#[tokio::test]
async fn create_footprint_writes_and_checks_custom_pads() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("Cross.kicad_mod");
    let pad = json!({
        "number": "1", "type": "smd", "shape": "custom",
        "x": 0, "y": 0, "width": 0.6, "height": 0.6,
        "custom_shape": cross_and_circle()
    });
    let result = handle_create_footprint(
        &json!({ "output": output, "name": "Cross", "pads": [pad] }),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{:?}", result.content);
    let written = std::fs::read_to_string(&output).unwrap();
    let root = parse_sexp(&written).unwrap();
    let shape =
        super::super::pad_primitives::CustomPadShape::from_pad(root.find("pad").unwrap()).unwrap();
    assert_eq!(
        shape,
        super::super::pad_primitives::CustomPadShape::from_json(&cross_and_circle()).unwrap()
    );

    for (pad, field) in [
        (
            json!({ "number": "1", "type": "smd", "shape": "custom", "x": 0, "y": 0, "width": 1, "height": 1 }),
            "pads[0].custom_shape",
        ),
        (
            json!({
                "number": "1", "type": "smd", "shape": "rect", "x": 0, "y": 0, "width": 1, "height": 1,
                "custom_shape": cross_and_circle()
            }),
            "pads[0].custom_shape",
        ),
    ] {
        let refused = dir.path().join("Refused.kicad_mod");
        let result = handle_create_footprint(
            &json!({ "output": refused, "name": "Refused", "pads": [pad] }),
            &ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error);
        assert_eq!(body(&result)["error"]["field"], field);
        assert!(!refused.exists());
    }
}

/// Ratios are written as KiCad's `FormatDouble2Str` writes them.
#[test]
fn ratios_are_formatted_as_kicad_formats_them() {
    for (ratio, text) in [
        (0.0, "0"),
        (0.25, "0.25"),
        (0.1, "0.1"),
        (0.05, "0.05"),
        (0.5, "0.5"),
        (1.0 / 6.0, "0.1666666667"),
        (0.00005, "0.00005"),
    ] {
        assert_eq!(format_ratio(ratio), text, "{ratio}");
    }
}
