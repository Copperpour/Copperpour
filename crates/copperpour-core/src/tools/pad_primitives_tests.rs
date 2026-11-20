use super::*;
use copperpour_sexp::parse_sexp;
use copperpour_sexp::writer::find_direct_child_blocks;
use serde_json::json;

// KiCad 10.0.8 wrote each of these from a stock footprint; provenance in
// tests/fixtures/pad_primitives_kicad10.README.md.
const FIXTURES: [(&str, &str); 4] = [
    (
        "vqfn20",
        include_str!("../../tests/fixtures/pad_primitives_vqfn20_kicad10.kicad_mod"),
    ),
    (
        "microsd",
        include_str!("../../tests/fixtures/pad_primitives_microsd_kicad10.kicad_mod"),
    ),
    (
        "wuerth",
        include_str!("../../tests/fixtures/pad_primitives_wuerth_kicad10.kicad_mod"),
    ),
    (
        "fiducial",
        include_str!("../../tests/fixtures/pad_primitives_fiducial_kicad10.kicad_mod"),
    ),
];

/// A custom pad's block, and the text of its `(options …)` and
/// `(primitives …)` children.
struct CustomPadText {
    block: String,
    options: String,
    primitives: String,
}

/// Every custom pad that is a direct child of the footprint in `source`.
fn custom_pads(source: &str) -> Vec<CustomPadText> {
    find_direct_child_blocks(source, "footprint")
        .into_iter()
        .map(|(start, end)| &source[start..end])
        .filter(|block| {
            parse_sexp(block).is_ok_and(|pad| {
                pad.head() == Some("pad") && pad.get(3).and_then(SexpNode::as_str) == Some("custom")
            })
        })
        .map(|block| {
            let child = |tag: &str| {
                find_direct_child_blocks(block, "pad")
                    .into_iter()
                    .map(|(start, end)| &block[start..end])
                    .find(|child| child.starts_with(&format!("({tag}")))
                    .unwrap_or_else(|| panic!("the custom pad has no {tag}"))
                    .to_string()
            };
            CustomPadText {
                block: block.to_string(),
                options: child("options"),
                primitives: child("primitives"),
            }
        })
        .collect()
}

fn shape(block: &str) -> Result<CustomPadShape, String> {
    CustomPadShape::from_pad(&parse_sexp(block).unwrap())
}

/// Every custom pad in KiCad's own files reads into the model and writes back
/// as KiCad wrote it: `(options …)` and `(primitives …)` byte for byte, at a
/// footprint pad's depth (two tabs).
#[test]
fn every_custom_pad_kicad_wrote_is_written_back_byte_for_byte() {
    let mut pads = 0;
    for (name, source) in FIXTURES {
        for pad in custom_pads(source) {
            let shape = shape(&pad.block).unwrap_or_else(|error| panic!("{name}: {error}"));
            // KiCad on Windows wrote the fixtures with CRLF; the model writes
            // \n and leaves the file's line ending to the caller.
            assert_eq!(
                shape.options_sexp(2),
                pad.options.replace("\r\n", "\n"),
                "{name}: options"
            );
            assert_eq!(
                shape.primitives_sexp(2),
                pad.primitives.replace("\r\n", "\n"),
                "{name}: primitives"
            );
            pads += 1;
        }
    }
    assert_eq!(pads, 8 + 2 + 3 + 1, "every custom pad in the fixtures");
}

/// The fixtures hold every primitive type, both anchors, and a polygon whose
/// points KiCad wrapped at column 99, so the test above covers each writer.
#[test]
fn the_fixtures_cover_every_primitive_type_anchor_and_point_wrap() {
    let mut types = std::collections::BTreeSet::new();
    let mut anchors = std::collections::BTreeSet::new();
    let mut wrapped = false;
    for (_, source) in FIXTURES {
        for pad in custom_pads(source) {
            let shape = shape(&pad.block).unwrap();
            anchors.insert(format!("{:?}", shape.anchor));
            for primitive in &shape.primitives {
                types.insert(primitive_type(primitive));
            }
            wrapped |= pad
                .primitives
                .lines()
                .any(|line| line.trim_start().starts_with("(xy") && !line.contains("(pts"))
                && pad.primitives.matches("(xy").count()
                    > pad
                        .primitives
                        .lines()
                        .filter(|line| line.trim_start().starts_with("(xy"))
                        .count();
        }
    }
    assert_eq!(
        types.into_iter().collect::<Vec<_>>(),
        ["arc", "circle", "line", "poly", "rect"]
    );
    assert_eq!(anchors.into_iter().collect::<Vec<_>>(), ["Circle", "Rect"]);
    assert!(wrapped, "a polygon's points span more than one line");
}

fn primitive_type(primitive: &FootprintGraphic) -> &'static str {
    match primitive {
        FootprintGraphic::Line { .. } => "line",
        FootprintGraphic::Arc { .. } => "arc",
        FootprintGraphic::Rect { .. } => "rect",
        FootprintGraphic::Circle { .. } => "circle",
        FootprintGraphic::Poly { .. } => "poly",
    }
}

/// The JSON form is `set_footprint_graphics`' item vocabulary plus the anchor
/// and clearance, and reads back into the same shape.
#[test]
fn a_shape_reads_back_from_its_json() {
    let pads = custom_pads(FIXTURES[0].1);
    let first = shape(&pads[0].block).unwrap();
    assert_eq!(
        first.to_json(),
        json!({
            "anchor": "circle",
            "clearance": "outline",
            "primitives": [{
                "type": "poly",
                "points": [
                    { "x": -0.375, "y": -0.075 },
                    { "x": 0.254289, "y": -0.075 },
                    { "x": 0.375, "y": 0.045711 },
                    { "x": 0.375, "y": 0.075 },
                    { "x": -0.375, "y": 0.075 }
                ],
                "stroke_width_mm": 0.1,
                "fill": "solid"
            }]
        })
    );
    for (name, source) in FIXTURES {
        for pad in custom_pads(source) {
            let shape = shape(&pad.block).unwrap();
            assert_eq!(
                CustomPadShape::from_json(&shape.to_json()),
                Ok(shape),
                "{name}"
            );
        }
    }
}

/// A pad without `(options …)` gets KiCad's defaults: a circle anchor and
/// the outline clearance.
#[test]
fn a_pad_without_options_gets_kicads_defaults() {
    let shape = shape(
        "(pad \"1\" smd custom (at 0 0) (size 0.2 0.2) (layers \"F.Cu\") \
         (primitives (gr_circle (center 0 0) (end 0.5 0) (width 0) (fill yes))))",
    )
    .unwrap();
    assert_eq!(shape.anchor, Anchor::Circle);
    assert_eq!(shape.clearance, Clearance::Outline);
}

/// A primitive without `fill`, as older writers and KiCad's footprint
/// generator leave it, reads as KiCad's reader takes it: a polygon is filled,
/// a rectangle or circle only at width 0. The stock Wuerth footprint's circle
/// (width 3.2, no `fill`) is one; KiCad's resave in the fixture writes
/// `(fill no)` for it.
#[test]
fn a_primitive_without_fill_reads_as_kicad_reads_it() {
    let fill = |primitive: &str| {
        let pad = format!(
            "(pad \"1\" smd custom (at 0 0) (size 0.2 0.2) (layers \"F.Cu\") (primitives {primitive}))"
        );
        match &shape(&pad).unwrap().primitives[0] {
            FootprintGraphic::Poly { fill, .. }
            | FootprintGraphic::Rect { fill, .. }
            | FootprintGraphic::Circle { fill, .. } => fill.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        fill("(gr_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (width 0.1))"),
        Fill::Solid
    );
    assert_eq!(
        fill("(gr_circle (center 0 0) (end 1 0) (width 0))"),
        Fill::Solid
    );
    assert_eq!(
        fill("(gr_circle (center -2.1 0) (end 0 0) (width 3.2))"),
        Fill::None
    );
    assert_eq!(
        fill("(gr_rect (start 0 0) (end 1 1) (width 0))"),
        Fill::Solid
    );
    assert_eq!(
        fill("(gr_rect (start 0 0) (end 1 1) (width 0.1) (fill solid))"),
        Fill::Solid
    );
    assert_eq!(
        fill("(gr_rect (start 0 0) (end 1 1) (width 0.1) (fill none))"),
        Fill::None
    );
}

/// What the model cannot carry is refused with the reason, never dropped.
#[test]
fn what_the_model_cannot_carry_is_refused() {
    let pad = |options: &str, primitives: &str| {
        format!("(pad \"1\" smd custom (at 0 0) (size 0.2 0.2) (layers \"F.Cu\") {options} {primitives})")
    };
    let poly = "(gr_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (width 0) (fill yes))";
    for (options, primitives, reason) in [
        (
            "(options (anchor oval))",
            format!("(primitives {poly})"),
            "unknown custom pad anchor 'oval'",
        ),
        (
            "(options (clearance none))",
            format!("(primitives {poly})"),
            "unknown custom pad clearance 'none'",
        ),
        (
            "(options (anchor circle) (thermal yes))",
            format!("(primitives {poly})"),
            "custom pad option 'thermal' is not supported",
        ),
        ("", String::new(), "the custom pad has no primitives"),
        (
            "",
            "(primitives)".to_string(),
            "the custom pad has no primitives",
        ),
        (
            "",
            format!("(primitives {poly}) (primitives {poly})"),
            "(primitives …) appears more than once",
        ),
        (
            "",
            "(primitives (gr_curve (pts (xy 0 0) (xy 1 0) (xy 1 1) (xy 0 1)) (width 0.1)))"
                .to_string(),
            "Bezier curve primitives (gr_curve) are not supported",
        ),
        (
            "",
            "(primitives (gr_rect (start 0 0) (end 1 1) (radius 0.1) (width 0) (fill yes)))"
                .to_string(),
            "gr_rect clause 'radius' is not supported",
        ),
        (
            "",
            "(primitives (gr_line (start 0 0) (end 0 0) (width 0.1)))".to_string(),
            "line start and end must differ",
        ),
        (
            "",
            "(primitives (gr_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (width 0) (fill hatch)))"
                .to_string(),
            "gr_poly fill 'hatch' is not supported",
        ),
        (
            "",
            "(primitives (gr_line (start 0 0) (end 1 0) (width 0.1) (width 0.2)))".to_string(),
            "gr_line clause 'width' appears more than once",
        ),
    ] {
        let error = shape(&pad(options, &primitives)).unwrap_err();
        assert!(error.contains(reason), "{options} {primitives}: {error}");
    }
}

/// JSON is checked with `set_footprint_graphics`' validators, and the error
/// names the field.
#[test]
fn json_is_validated_and_names_the_field() {
    let field = |value: serde_json::Value| CustomPadShape::from_json(&value).unwrap_err().0;
    assert_eq!(
        field(json!({ "anchor": "circle", "primitives": [] })),
        "primitives"
    );
    assert_eq!(
        field(json!({
            "anchor": "rect",
            "primitives": [{
                "type": "line", "start": { "x": 0, "y": 0 }, "end": { "x": 0, "y": 0 },
                "stroke_width_mm": 0.1
            }]
        })),
        "primitives[0]"
    );
    for refused in [
        json!({ "anchor": "oval", "primitives": [] }),
        json!({ "anchor": "circle", "clearance": "none", "primitives": [] }),
        json!({ "anchor": "circle", "primitives": [], "size": 1 }),
    ] {
        assert!(CustomPadShape::from_json(&refused).is_err(), "{refused}");
    }
}

/// Values finer than KiCad's 1 nm grid are snapped to it, as KiCad's editor
/// stores them: a computed `0.1 + 0.2` becomes `0.3`, a 7-decimal radius and
/// width are rounded, and no negative zero is left. A primitive that rounding
/// makes invalid is refused with the field named (#694 review).
#[test]
fn json_values_are_snapped_to_kicads_nanometre_grid() {
    let shape = CustomPadShape::from_json(&json!({
        "anchor": "rect",
        "primitives": [
            {
                "type": "poly",
                "points": [
                    { "x": 0.1 + 0.2, "y": -0.0000001 },
                    { "x": 1.0, "y": 0.0 },
                    { "x": 1.0, "y": 0.1234567 }
                ],
                "stroke_width_mm": 0.0,
                "fill": "solid"
            },
            {
                "type": "circle",
                "center": { "x": 0.0, "y": 0.0 },
                "radius_mm": 0.1234567,
                "stroke_width_mm": 0.1234567,
                "fill": "none"
            }
        ]
    }))
    .unwrap();
    assert_eq!(
        shape.to_json()["primitives"],
        json!([
            {
                "type": "poly",
                "points": [
                    { "x": 0.3, "y": 0.0 },
                    { "x": 1.0, "y": 0.0 },
                    { "x": 1.0, "y": 0.123457 }
                ],
                "stroke_width_mm": 0.0,
                "fill": "solid"
            },
            {
                "type": "circle",
                "center": { "x": 0.0, "y": 0.0 },
                "radius_mm": 0.123457,
                "stroke_width_mm": 0.123457,
                "fill": "none"
            }
        ])
    );
    // What the model writes reads back as the snapped shape.
    let written = format!(
        "(pad \"1\" smd custom
		(at 0 0)
		(size 0.2 0.2)
		(layers \"F.Cu\")
		{}
		{}
	)",
        shape.options_sexp(2),
        shape.primitives_sexp(2)
    );
    assert!(
        !written.contains("-0 ") && !written.contains("-0)"),
        "{written}"
    );
    assert_eq!(self::shape(&written), Ok(shape));

    let (field, reason) = CustomPadShape::from_json(&json!({
        "anchor": "rect",
        "primitives": [{
            "type": "line", "start": { "x": 0.0, "y": 0.0 }, "end": { "x": 0.0000004, "y": 0.0 },
            "stroke_width_mm": 0.1
        }]
    }))
    .unwrap_err();
    assert_eq!(field, "primitives[0]");
    assert!(reason.contains("1 nm grid"), "{reason}");
}

/// Every custom pad in KiCad's installed stock library reads into the model,
/// and what the model writes reads back into the same shape. The only ones
/// refused are polygons with arc segments in their outline: 3 pads in 2
/// footprints of KiCad 10.0.8's library. Skips without an installed library.
#[test]
fn every_stock_custom_pad_reads_and_writes_back() {
    let Some(footprints) = crate::tools::find_kicad_library_dirs("footprints")
        .into_iter()
        .find(|dir| dir.join("Package_DFN_QFN.pretty").is_dir())
    else {
        eprintln!("SKIP: no installed KiCad footprint library");
        return;
    };
    let mut pads = 0;
    let mut arc_outlines = Vec::new();
    for library in std::fs::read_dir(&footprints).unwrap() {
        let library = library.unwrap().path();
        if library.extension().and_then(|e| e.to_str()) != Some("pretty") {
            continue;
        }
        for file in std::fs::read_dir(&library).unwrap() {
            let file = file.unwrap().path();
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            if !source.contains(" custom") {
                continue;
            }
            for pad in custom_pads(&source) {
                let shape = match shape(&pad.block) {
                    Ok(shape) => shape,
                    Err(error) if error.contains("arc segments") => {
                        arc_outlines.push(file.file_name().unwrap().to_owned());
                        continue;
                    }
                    Err(error) => panic!("{}: {error}", file.display()),
                };
                let written = format!(
                    "(pad \"1\" smd custom\n\t\t{}\n\t\t{}\n\t)",
                    shape.options_sexp(2),
                    shape.primitives_sexp(2)
                );
                assert_eq!(
                    self::shape(&written).as_ref(),
                    Ok(&shape),
                    "{}",
                    file.display()
                );
                pads += 1;
            }
        }
    }
    assert!(pads > 1000, "only {pads} stock custom pads found");
    assert!(
        arc_outlines.len() <= 3,
        "more polygons with arc segments than KiCad 10.0.8 has: {arc_outlines:?}"
    );
    eprintln!(
        "{pads} stock custom pads read and written back; refused for arc segments: {arc_outlines:?}"
    );
}
