//! `create_footprint` writes the KiCad 10 format KiCad itself writes, and its
//! courtyard and silk go around the body (#711).

use super::*;
use crate::router::ToolRouter;
use crate::tools::footprint_kicad10::{Graphic, Shape};
use crate::tools::{ServerConfig, ToolContext};
use std::sync::Arc;

fn ctx() -> ToolContext {
    ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()))
}

/// `text` with its UUIDs and generator masked: KiCad gives new UUIDs on each
/// save and writes `pcbnew` as the generator.
fn masked(text: &str) -> String {
    solder_margins::without_uuids(text)
        .replace("(generator \"copperpour\")", "(generator \"pcbnew\")")
}

async fn created(arguments: serde_json::Value) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("F.kicad_mod");
    let mut arguments = arguments;
    arguments["output"] = json!(path);
    let result = handle_create_footprint(&arguments, &ctx()).await.unwrap();
    assert!(!result.is_error, "{:?}", result.content);
    std::fs::read_to_string(path).unwrap()
}

/// KiCad 10.0.8's `kicad-cli fp upgrade --force` of what `create_footprint`
/// wrote for these arguments gives the same file, UUIDs and generator aside.
/// Provenance in tests/fixtures/create_footprint_kicad10.README.md.
#[tokio::test]
async fn create_footprint_writes_what_kicad_writes() {
    let cases = [
        (
            "smd_body",
            json!({"name": "SMD2", "description": "two pads \"quoted\"", "body_width": 4, "body_height": 2,
                "pads": [{"number": "2", "type": "smd", "shape": "roundrect", "x": 1.5, "y": 0, "width": 0.8, "height": 1.2, "roundrect_rratio": 0.25},
                         {"number": "1", "type": "smd", "shape": "rect", "x": -1.5, "y": 0, "width": 0.8, "height": 1.2}],
                "model": {"path": "${KICAD10_3DMODEL_DIR}/x.step", "rotate": {"z": 90}, "offset": {"x": 0.1, "y": -0.2}}}),
            include_str!("../../tests/fixtures/create_footprint_smd_body_kicad10.kicad_mod"),
        ),
        (
            "tht",
            json!({"name": "THT3",
                "pads": [{"number": "10", "type": "thru_hole", "shape": "rect", "x": 0, "y": 0, "width": 1.7, "height": 1.7, "drill": 1.0, "fabrication_property": "castellated"},
                         {"number": "2", "type": "thru_hole", "shape": "oval", "x": 2.54, "y": 0, "width": 1.7, "height": 2.2, "drill": {"shape": "oval", "width": 0.8, "height": 1.2}, "rotation": 90},
                         {"number": "", "type": "np_thru_hole", "shape": "circle", "x": 5, "y": 0, "width": 2, "height": 2, "drill": 2},
                         {"number": "3", "type": "thru_hole", "shape": "circle", "x": 2.54, "y": 2.54, "width": 1.7, "height": 1.7, "drill": 1.0, "rotation": -90}]}),
            include_str!("../../tests/fixtures/create_footprint_tht_kicad10.kicad_mod"),
        ),
        (
            "large_body",
            json!({"name": "BOURNS", "body_width": 12.62, "body_height": 8.48,
                "pads": [{"number": "1", "type": "smd", "shape": "rect", "x": -5.715, "y": 0, "width": 2.5, "height": 3},
                         {"number": "2", "type": "smd", "shape": "rect", "x": 5.715, "y": 0, "width": 2.5, "height": 3},
                         {"number": "3", "type": "smd", "shape": "custom", "x": 0, "y": 3, "width": 0.5, "height": 0.5,
                          "custom_shape": {"anchor": "circle", "primitives": [{"type": "poly", "points": [{"x": -1, "y": -0.5}, {"x": 1, "y": -0.5}, {"x": 1, "y": 0.5}, {"x": -1, "y": 0.5}], "stroke_width_mm": 0.0, "fill": "solid"}]}}]}),
            include_str!("../../tests/fixtures/create_footprint_large_body_kicad10.kicad_mod"),
        ),
        (
            "back_without_silk",
            json!({"name": "Back", "silkscreen": "none",
                "pads": [{"number": "1", "type": "smd", "shape": "rect", "x": -2, "y": 0, "width": 1, "height": 1, "layers": ["B.Cu", "B.Mask"]},
                         {"number": "2", "type": "smd", "shape": "rect", "x": 2, "y": 0, "width": 1, "height": 1, "layers": ["B.Cu", "B.Mask"]}]}),
            include_str!("../../tests/fixtures/create_footprint_back_kicad10.kicad_mod"),
        ),
    ];
    for (case, arguments, kicad) in cases {
        let written = created(arguments).await;
        assert_eq!(masked(&written), masked(kicad), "{case}");
        assert!(written.contains("(generator \"copperpour\")"), "{case}");
        assert_eq!(
            written.matches("(uuid \"").count(),
            kicad.matches("(uuid \"").count(),
            "{case}: every item has its own UUID"
        );
    }
}

/// Two writes of the same footprint differ in their UUIDs only.
#[tokio::test]
async fn each_write_gives_new_uuids() {
    let arguments = json!({"name": "R", "pads": [
        {"number": "1", "type": "smd", "shape": "rect", "x": -1, "y": 0, "width": 1, "height": 1},
        {"number": "2", "type": "smd", "shape": "rect", "x": 1, "y": 0, "width": 1, "height": 1}]});
    let (first, second) = (created(arguments.clone()).await, created(arguments).await);
    assert_ne!(first, second);
    assert_eq!(masked(&first), masked(&second));
}

fn pad(number: &str, x: f64, y: f64, w: f64, h: f64) -> PadGeom {
    PadGeom {
        number: number.into(),
        pad_type: "smd".into(),
        x,
        y,
        w,
        h,
        rotation: 0.0,
        custom_bounds: None,
    }
}

fn on(outlines: &FootprintOutlines, layer: &str) -> Vec<Shape> {
    outlines
        .graphics
        .iter()
        .filter(|graphic: &&Graphic| graphic.layer == layer)
        .map(|graphic| graphic.shape.clone())
        .collect()
}

fn rounded(shapes: Vec<Shape>) -> Vec<Shape> {
    let r = |(x, y): (f64, f64)| (round6(x), round6(y));
    shapes
        .into_iter()
        .map(|shape| match shape {
            Shape::Line { start, end } => Shape::Line {
                start: r(start),
                end: r(end),
            },
            Shape::Rect { start, end } => Shape::Rect {
                start: r(start),
                end: r(end),
            },
            other => other,
        })
        .collect()
}

/// #704's example: a Bourns 70AAJ-5-M0, body 12.62 x 8.48 mm, pads spanning
/// x ±5.715. Before, the courtyard was x ±5.965 and the silk x ±5.865, inside
/// the body.
#[test]
fn the_courtyard_and_silk_go_around_the_body() {
    let pads = [
        pad("1", -4.465, 0.0, 2.5, 3.0),
        pad("2", 4.465, 0.0, 2.5, 3.0),
    ];
    let outlines = build_footprint_graphics(
        &json!({ "body_width": 12.62, "body_height": 8.48 }),
        &pads,
        &["F.CrtYd"],
        Silkscreen::Outline,
    );
    assert_eq!(
        rounded(on(&outlines, "F.CrtYd")),
        [Shape::Rect {
            start: (-6.56, -4.49),
            end: (6.56, 4.49)
        }],
        "body plus the 0.25 mm SMD clearance"
    );
    let line = |start, end| Shape::Line { start, end };
    let mut silk = rounded(on(&outlines, "F.SilkS"));
    silk.retain(|shape| matches!(shape, Shape::Line { .. }));
    assert_eq!(
        silk,
        [
            line((-6.42, -4.35), (6.42, -4.35)),
            line((-6.42, 4.35), (6.42, 4.35)),
            line((-6.42, -4.35), (-6.42, 4.35)),
            line((6.42, -4.35), (6.42, 4.35)),
        ],
        "0.11 mm outside the body, unbroken: no pad comes within 0.2 mm"
    );
    assert_eq!(outlines.reference_at.1, -4.49 - 1.0);
}

/// Where the silk outline crosses or nears a pad, it is broken so that its
/// edge keeps 0.2 mm from the copper; pieces shorter than the line are dropped.
#[test]
fn the_silk_is_broken_near_pads() {
    // A 3 x 3 mm body whose side pads reach past it.
    let pads = [
        pad("1", -1.45, -0.5, 0.8, 0.3),
        pad("2", -1.45, 0.5, 0.8, 0.3),
        pad("3", 1.45, 0.5, 0.8, 0.3),
        pad("4", 1.45, -0.5, 0.8, 0.3),
    ];
    let outlines = build_footprint_graphics(
        &json!({ "body_width": 3.0, "body_height": 3.0 }),
        &pads,
        &["F.CrtYd"],
        Silkscreen::Outline,
    );
    let keep_out = SILK_PAD_CLEARANCE + SILK_WIDTH / 2.0;
    let mut pieces = 0;
    for shape in on(&outlines, "F.SilkS") {
        let Shape::Line { start, end } = shape else {
            continue;
        };
        pieces += 1;
        assert!(
            ((end.0 - start.0).abs() + (end.1 - start.1).abs()) >= SILK_WIDTH,
            "{start:?} {end:?}"
        );
        for pad in &pads {
            let (x0, y0, x1, y1) = pad.extent();
            let apart = start.0.max(end.0) <= x0 - keep_out + 1e-9
                || start.0.min(end.0) >= x1 + keep_out - 1e-9
                || start.1.max(end.1) <= y0 - keep_out + 1e-9
                || start.1.min(end.1) >= y1 + keep_out - 1e-9;
            assert!(apart, "silk {start:?}-{end:?} too near pad {}", pad.number);
        }
    }
    // Top and bottom whole; each side broken around its two pads into three.
    assert_eq!(pieces, 2 + 3 + 3, "{:?}", on(&outlines, "F.SilkS"));
}

/// Without a body, the courtyard and silk follow the pad envelope as before.
#[test]
fn without_a_body_the_outlines_follow_the_pads() {
    let pads = [pad("1", -1.0, 0.0, 0.6, 0.8), pad("2", 1.0, 0.0, 0.6, 0.8)];
    let outlines = build_footprint_graphics(&json!({}), &pads, &["F.CrtYd"], Silkscreen::Outline);
    assert_eq!(
        rounded(on(&outlines, "F.CrtYd")),
        [Shape::Rect {
            start: (-1.55, -0.65),
            end: (1.55, 0.65)
        }]
    );
    let silk = rounded(on(&outlines, "F.SilkS"));
    assert!(silk.contains(&Shape::Rect {
        start: (-1.45, -0.55),
        end: (1.45, 0.55)
    }));
}

#[test]
fn pin1_dot_is_omitted_when_no_interior_position_fits() {
    for pads in [
        [pad("1", -0.5, 0.0, 0.8, 0.8), pad("2", 0.5, 0.0, 0.8, 0.8)],
        [pad("1", -1.0, 0.0, 0.2, 0.2), pad("2", 1.0, 0.0, 0.2, 0.2)],
        [pad("1", 0.0, -1.0, 0.2, 0.2), pad("2", 0.0, 1.0, 0.2, 0.2)],
        [pad("1", 0.0, -0.5, 0.8, 0.8), pad("2", 0.0, 0.5, 0.8, 0.8)],
    ] {
        let outlines =
            build_footprint_graphics(&json!({}), &pads, &["F.CrtYd"], Silkscreen::Pin1Only);
        assert!(on(&outlines, "F.SilkS").is_empty(), "{pads:?}");
        assert!(matches!(
            on(&outlines, "F.Fab").as_slice(),
            [Shape::Poly { .. }]
        ));
    }
}

#[tokio::test]
async fn create_footprint_dot_clears_custom_copper() {
    let written = created(json!({
        "name": "Custom", "silkscreen": "pin1_only",
        "pads": [
            {"number": "1", "type": "smd", "shape": "custom",
             "x": -2, "y": 0, "width": 1, "height": 1,
             "custom_shape": {"anchor": "rect", "primitives": [
                 {"type": "rect", "start": {"x": 0, "y": -0.5},
                  "end": {"x": 1.2, "y": 0.5}, "stroke_width_mm": 0.0, "fill": "solid"}
             ]}},
            {"number": "2", "type": "smd", "shape": "rect",
             "x": 2, "y": 0, "width": 1, "height": 1}
        ]
    }))
    .await;
    let footprint = parse_sexp(&written).unwrap();
    let dot = footprint.find("fp_circle").unwrap();
    // The custom copper reaches x=-0.8. A 0.2 mm outer radius and 0.2 mm
    // clearance place the circle centre at x=-0.4.
    assert_eq!(dot.find("center").unwrap().get_f64(1), Some(-0.4));
}

#[test]
fn pin1_dot_tries_both_directions_on_a_centered_axis() {
    for (horizontal, blocked) in [(true, 1.0), (true, -1.0), (false, 1.0), (false, -1.0)] {
        let position = |along| {
            if horizontal {
                (along, 0.0)
            } else {
                (0.0, along)
            }
        };
        let make_pad = |number, along, size| {
            let (x, y) = position(along);
            pad(number, x, y, size, size)
        };
        let pads = [
            make_pad("1", 0.0, 1.0),
            make_pad("2", -2.0, 1.0),
            make_pad("3", 2.0, 1.0),
            make_pad("4", blocked, 0.5),
        ];
        let outlines =
            build_footprint_graphics(&json!({}), &pads, &["F.CrtYd"], Silkscreen::Pin1Only);
        let silk = on(&outlines, "F.SilkS");
        let [Shape::Circle { center, .. }] = silk.as_slice() else {
            panic!(
                "expected one pin-1 dot on horizontal={horizontal}, blocker={blocked}: {silk:?}"
            );
        };
        assert_eq!(*center, position(-blocked * 0.9));
    }
}

#[test]
fn a_rotated_pad_counts_with_its_rotated_extent() {
    let mut rotated = pad("1", 0.0, 0.0, 0.8, 1.2);
    rotated.rotation = 90.0;
    let (x0, y0, x1, y1) = rotated.extent();
    assert_eq!(
        (round6(x0), round6(y0), round6(x1), round6(y1)),
        (-0.6, -0.4, 0.6, 0.4)
    );
}

#[test]
fn body_height_is_described_as_the_bodys_y_size() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == "create_footprint")
        .unwrap();
    let description = tool.input_schema["properties"]["body_height"]["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("size along Y"), "{description}");
    assert!(description.contains("not the 3D height"), "{description}");
}

/// A piece of silk shorter than a line's width is dropped, not drawn as a dot.
#[test]
fn silk_slivers_are_dropped() {
    // The pad's keep-out spans x ±0.96 (0.7 + 0.2 + half the 0.12 line).
    let pads = [pad("1", 0.0, 0.0, 1.4, 0.2)];
    assert_eq!(
        silk_pieces(&pads, true, 0.0, -1.0, 1.0),
        Vec::<(f64, f64)>::new()
    );
    let kept = silk_pieces(&pads, true, 0.0, -1.2, 1.2);
    assert_eq!(kept.len(), 2, "{kept:?}");
    for (start, end) in kept {
        assert!((end - start - 0.24).abs() < 1e-9, "{start} {end}");
    }
}
