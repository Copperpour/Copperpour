use super::*;

#[test]
fn pin_root_is_on_the_body_side_of_the_connection() {
    // Left pin (points right): bulb on the left, root to its right (body).
    let (lx, ly) = pin_root(-10.16, 0.0, 0.0, 2.54);
    assert!(
        (lx - -7.62).abs() < 1e-9 && ly.abs() < 1e-9,
        "left {lx},{ly}"
    );
    // Right pin (points left): root to the left of the bulb.
    let (rx, ry) = pin_root(10.16, 0.0, 180.0, 2.54);
    assert!(
        (rx - 7.62).abs() < 1e-9 && ry.abs() < 1e-9,
        "right {rx},{ry}"
    );
    // Up pin (points up, Y-up): root above the bulb.
    let (ux, uy) = pin_root(0.0, -5.0, 90.0, 2.54);
    assert!(ux.abs() < 1e-9 && (uy - -2.46).abs() < 1e-9, "up {ux},{uy}");
}

/// A pin for the body-sizing tests. `"~"` is KiCAD's "unnamed" sentinel, so
/// it contributes no name width.
fn test_pin(x: f64, y: f64, angle: f64, length: f64, name: &str) -> PinGeom {
    PinGeom {
        x,
        y,
        angle,
        length,
        name: name.into(),
    }
}

#[test]
fn symbol_body_rect_touches_side_pins_and_spaces_the_ends() {
    let pin = |x, y, angle| test_pin(x, y, angle, 2.54, "~");
    // Three pins on the left (point right), two on the right (point left).
    let pins = vec![
        pin(-10.16, 2.54, 0.0),
        pin(-10.16, 0.0, 0.0),
        pin(-10.16, -2.54, 0.0),
        pin(10.16, 2.54, 180.0),
        pin(10.16, -2.54, 180.0),
    ];
    let (min_x, min_y, max_x, max_y) = symbol_body_rect(&pins, true).unwrap();
    // Left/right edges pass through the pin roots (pins touch the border).
    assert!((min_x - -7.62).abs() < 1e-9, "left edge {min_x}");
    assert!((max_x - 7.62).abs() < 1e-9, "right edge {max_x}");
    // Connection bulbs at x = ±10.16 stay outside the body.
    assert!(min_x > -10.16 && max_x < 10.16);
    // Top/bottom edges have no pins → spacing beyond the outermost pins.
    assert!(max_y >= 2.54 + 2.5, "top spacing {max_y}");
    assert!(min_y <= -2.54 - 2.5, "bottom spacing {min_y}");
    assert!(symbol_body_rect(&[], true).is_none());
}

/// A box that only touches the pin roots leaves the two columns of pin
/// names overlapping: here a 26-character name facing an 8-character one.
#[test]
fn body_widens_so_facing_pin_names_do_not_collide() {
    let pin = |x, angle, name| test_pin(x, 0.0, angle, 5.08, name);
    let pins = vec![
        pin(-25.4, 0.0, "LONG/MULTI/FUNCTION/NAME/X"),
        pin(25.4, 180.0, "SHORT/NM"),
    ];
    let roots_only = symbol_body_rect(&pins, false).unwrap();
    assert!(
        (roots_only.2 - roots_only.0 - 40.64).abs() < 1e-9,
        "without names the body still hugs the pin roots: {roots_only:?}"
    );

    let (min_x, _, max_x, _) = symbol_body_rect(&pins, true).unwrap();
    let inner = max_x - min_x - 2.0 * PIN_NAME_OFFSET;
    let text = pin_name_width("LONG/MULTI/FUNCTION/NAME/X", PIN_TEXT)
        + pin_name_width("SHORT/NM", PIN_TEXT);
    assert!(
        inner >= text + PIN_NAME_GAP,
        "body {:.2} mm leaves {:.2} mm for {:.2} mm of names",
        max_x - min_x,
        inner,
        text
    );
    // Edges stay on the schematic grid so the pins meeting them do too.
    assert!(
        (min_x / SYMBOL_GRID).fract().abs() < 1e-9,
        "off grid {min_x}"
    );
}

/// Name width counts drawn glyphs, not the markup around them.
#[test]
fn body_width_ignores_unnamed_and_overbar_markup() {
    assert_eq!(pin_name_width("~", PIN_TEXT), 0.0);
    // `~{RST}` is RST with an overbar: three glyphs, not six.
    assert!((pin_name_width("~{RST}", PIN_TEXT) - pin_name_width("RST", PIN_TEXT)).abs() < 1e-9);
}

/// A row with only one name needs room for that name, not for a gap too.
#[test]
fn a_row_with_one_name_reserves_no_column_gap() {
    let name = "SOLO/PIN/NAME";
    let one_sided = names_span(&[test_pin(-25.4, 0.0, 0.0, 5.08, name)], Axis::Horizontal);
    assert!(
        (one_sided - (pin_name_width(name, PIN_TEXT) + 2.0 * PIN_NAME_OFFSET)).abs() < 1e-9,
        "one name reserved {one_sided} mm"
    );
    // The same two names on separate rows never share one, so neither does.
    let staggered = names_span(
        &[
            test_pin(-25.4, 0.0, 0.0, 5.08, name),
            test_pin(25.4, 2.54, 180.0, 5.08, name),
        ],
        Axis::Horizontal,
    );
    assert!(
        (staggered - one_sided).abs() < 1e-9,
        "staggered {staggered}"
    );
}

/// Vertical pins get the same treatment on height — their names are drawn
/// across it just as horizontal pins' names are drawn across the width.
#[test]
fn body_widens_vertically_for_facing_top_and_bottom_names() {
    let pins = vec![
        test_pin(0.0, -25.4, 90.0, 5.08, "LONG/MULTI/FUNCTION/NAME/X"),
        test_pin(0.0, 25.4, 270.0, 5.08, "SHORT/NM"),
    ];
    let (_, min_y, _, max_y) = symbol_body_rect(&pins, true).unwrap();
    let inner = max_y - min_y - 2.0 * PIN_NAME_OFFSET;
    let text = pin_name_width("LONG/MULTI/FUNCTION/NAME/X", PIN_TEXT)
        + pin_name_width("SHORT/NM", PIN_TEXT);
    assert!(
        inner >= text + PIN_NAME_GAP,
        "body {:.2} mm tall leaves {:.2} mm for {:.2} mm of names",
        max_y - min_y,
        inner,
        text
    );
}

/// `grow_to` snaps both edges to the grid even when the box it widens is
/// centred off it — otherwise the pins slid out to meet them land off grid.
#[test]
fn grow_to_lands_on_grid_from_an_off_grid_centre() {
    let (mut lo, mut hi) = (-20.32, 16.51);
    grow_to(&mut lo, &mut hi, 50.0);
    assert!(hi - lo >= 50.0, "grew to {lo}..{hi}");
    for edge in [lo, hi] {
        assert!((edge / SYMBOL_GRID).fract().abs() < 1e-9, "off grid {edge}");
    }
}

/// Pins that defined the original box must slide out to meet the widened
/// one, or they float inside the body, off its outline.
#[tokio::test]
async fn widening_the_body_slides_pins_out_to_meet_it() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("w.kicad_sym");
    handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "WIDE",
            // Required by the schema; this test predates its enforcement
            // and relied on the handler's "U" default (#218).
            "reference_prefix": "U",
            "pins": [
                {"number":"1","name":"LONG/MULTI/FUNCTION/NAME/X","type":"bidirectional",
                 "x":-25.4,"y":0.0,"angle":0,"length":5.08},
                {"number":"2","name":"SHORT/NM","type":"bidirectional",
                 "x":25.4,"y":0.0,"angle":180,"length":5.08}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let out = std::fs::read_to_string(&lib).unwrap();
    let tree = copperpour_sexp::parse_sexp(&out).unwrap();
    // kicad_symbol_lib → (symbol "WIDE") → (symbol "WIDE_0_1") → body + pins
    let unit = tree.find("symbol").unwrap().find("symbol").unwrap();
    let rect = unit.find("rectangle").unwrap();
    let (sx, _) = copperpour_sexp::schematic::parse_start(rect).unwrap();
    let (ex, _) = copperpour_sexp::schematic::parse_end(rect).unwrap();
    for pin in unit.find_all("pin") {
        let (px, _, angle) = copperpour_sexp::schematic::parse_at(pin).unwrap();
        let root = if angle == 0.0 { px + 5.08 } else { px - 5.08 };
        let edge = if angle == 0.0 { sx } else { ex };
        assert!(
            (root - edge).abs() < 1e-6,
            "pin at {px} (angle {angle}) roots at {root}, body edge is {edge}"
        );
    }
}

#[tokio::test]
async fn create_symbol_emits_body_and_shows_pins() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("test.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "TEST_IC",
        "reference_prefix": "U",
        "pins": [
            {"number":"1","name":"IN","type":"input","x":-7.62,"y":2.54,"angle":0,"length":2.54},
            {"number":"2","name":"GND","type":"power_in","x":-7.62,"y":-2.54,"angle":0,"length":2.54},
            {"number":"3","name":"OUT","type":"output","x":7.62,"y":0.0,"angle":180,"length":2.54}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error);
    let c = std::fs::read_to_string(&lib).unwrap();
    assert!(
        c.contains("(rectangle"),
        "missing symbol body rectangle:\n{c}"
    );
    assert!(
        c.contains("(generator \"copperpour\")"),
        "stale generator string"
    );
    assert!(
        !c.contains("(pin_numbers hide)"),
        "KiCad 10 shows pin numbers by omitting the hide override"
    );
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "generated symbol doesn't parse"
    );
}

/// A caller who draws the body gets exactly what they drew, and no box
/// around it: the automatic rectangle exists to give pins something to sit
/// on, and a drawn body already has one (#501).
#[tokio::test]
async fn create_symbol_graphics_replace_the_automatic_body_rectangle() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("g.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "DRAWN",
            "reference_prefix": "VT",
            "pins": [
                {"number":"1","name":"A","type":"passive","x":-2.54,"y":3.81,"angle":270,"length":0.0}
            ],
            "graphics": [
                {"type":"rect","start":{"x":-5.08,"y":3.81},"end":{"x":5.08,"y":-3.81},
                 "stroke_width_mm":0.254,"fill":"background"},
                {"type":"poly","points":[{"x":-3.81,"y":1.27},{"x":-1.27,"y":1.27},{"x":-2.54,"y":-1.27}],
                 "stroke_width_mm":0.254,"fill":"none"}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let c = std::fs::read_to_string(&lib).unwrap();
    assert_eq!(
        c.matches("(rectangle").count(),
        1,
        "exactly the one rectangle the caller drew, no automatic body:\n{c}"
    );
    assert!(
        c.contains("(polyline"),
        "the drawn polygon is missing:\n{c}"
    );
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "generated symbol doesn't parse"
    );
}

/// The automatic-body path slides every pin out to the rectangle it
/// computed, so a caller cannot plan wiring from the coordinates they sent
/// (#293). Drawn bodies must not do that — the drawing already fixes where
/// the pins belong, and a transcription placing parts against a scan
/// depends on the reach it asked for.
#[tokio::test]
async fn create_symbol_graphics_write_pin_coordinates_exactly_as_supplied() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("g.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "DRAWN",
            "reference_prefix": "VT",
            "pins": [
                {"number":"1","name":"K","type":"passive","x":-2.54,"y":-3.81,"angle":90,"length":0.0},
                {"number":"2","name":"A","type":"passive","x":-2.54,"y":3.81,"angle":270,"length":0.0}
            ],
            "graphics": [
                {"type":"rect","start":{"x":-5.08,"y":3.81},"end":{"x":5.08,"y":-3.81},
                 "stroke_width_mm":0.254,"fill":"background"}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let c = std::fs::read_to_string(&lib).unwrap();
    assert!(c.contains("(at -2.54 3.81 270)"), "pin 2 moved:\n{c}");
    assert!(c.contains("(at -2.54 -3.81 90)"), "pin 1 moved:\n{c}");
}

/// Every primitive in the shared vocabulary reaches the file in the
/// `.kicad_sym` spelling — `line` and `poly` both become `polyline`,
/// because a symbol library has no separate line node.
#[tokio::test]
async fn create_symbol_graphics_emit_every_primitive() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("g.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "ALL",
            "reference_prefix": "U",
            "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
            "graphics": [
                {"type":"line","start":{"x":-1.0,"y":0.0},"end":{"x":1.0,"y":0.0},"stroke_width_mm":0.2},
                {"type":"poly","points":[{"x":0.0,"y":0.0},{"x":1.0,"y":1.0},{"x":2.0,"y":0.0}],
                 "stroke_width_mm":0.2,"fill":"none"},
                {"type":"rect","start":{"x":-2.0,"y":2.0},"end":{"x":2.0,"y":-2.0},
                 "stroke_width_mm":0.2,"fill":"background"},
                {"type":"circle","center":{"x":0.0,"y":0.0},"radius_mm":0.5,
                 "stroke_width_mm":0.2,"fill":"outline"},
                {"type":"arc","start":{"x":-1.0,"y":0.0},"mid":{"x":0.0,"y":1.0},"end":{"x":1.0,"y":0.0},
                 "stroke_width_mm":0.2}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let c = std::fs::read_to_string(&lib).unwrap();
    assert_eq!(c.matches("(polyline").count(), 2, "line and poly:\n{c}");
    assert_eq!(c.matches("(rectangle").count(), 1, "{c}");
    assert_eq!(c.matches("(circle").count(), 1, "{c}");
    assert_eq!(c.matches("(arc").count(), 1, "{c}");
    // KiCad's symbol fills, not the footprint side's none/solid.
    assert!(c.contains("(fill (type background))"), "{c}");
    assert!(c.contains("(fill (type outline))"), "{c}");
    assert!(c.contains("(fill (type none))"), "{c}");
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "generated symbol doesn't parse"
    );
}

/// Malformed geometry is refused, not drawn wrongly and not dropped. The
/// dispatch validates required *arguments*, not a `oneOf` inside an array
/// item, so without this a misspelled `type` emits nothing at all and the
/// call still reports success. Found by adversarial review of this feature.
#[tokio::test]
async fn create_symbol_graphics_refuse_malformed_primitives_without_writing() {
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "unknown type",
            json!({"type":"polyline","points":[{"x":0,"y":0},{"x":1,"y":1},{"x":2,"y":0}],
                   "stroke_width_mm":0.2,"fill":"none"}),
        ),
        (
            "line without an end",
            json!({"type":"line","start":{"x":-1,"y":0},"stroke_width_mm":0.2}),
        ),
        (
            "no stroke width",
            json!({"type":"rect","start":{"x":-2,"y":2},"end":{"x":2,"y":-2},"fill":"none"}),
        ),
        (
            "poly with one point",
            json!({"type":"poly","points":[{"x":0,"y":0}],"stroke_width_mm":0.2,"fill":"none"}),
        ),
        (
            "fill outside the vocabulary",
            json!({"type":"rect","start":{"x":-2,"y":2},"end":{"x":2,"y":-2},
                   "stroke_width_mm":0.2,"fill":"chartreuse"}),
        ),
        (
            "zero radius",
            json!({"type":"circle","center":{"x":0,"y":0},"radius_mm":0,
                   "stroke_width_mm":0.2,"fill":"none"}),
        ),
        ("entry is not an object at all", json!("rect")),
    ];
    for (label, primitive) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("bad.kicad_sym");
        let res = handle_create_symbol(
            &json!({
                "library_path": lib.to_string_lossy(),
                "name": "BAD",
                "reference_prefix": "U",
                "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
                "graphics": [primitive]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(res.is_error, "{label} must be refused");
        let message = format!("{res:?}");
        assert!(
            message.contains("graphics[0]"),
            "{label}: the refusal must name the offending entry: {message}"
        );
        if label == "entry is not an object at all" {
            // Without the shape check this still refuses, but as
            // `unknown type ""` — which sends the caller looking for a
            // typo in a field they never wrote.
            assert!(
                message.contains("must be an object describing one primitive"),
                "{label}: the refusal must say what is actually wrong: {message}"
            );
        }
        assert!(!lib.exists(), "{label}: nothing may be written");
    }
}

/// A present-but-malformed `graphics` must not read as absent. Reading it
/// with `as_array().unwrap_or_default()` turned a wrong request into an
/// empty one: the symbol came out with an automatic rectangle and a
/// success, and the caller's drawing was nowhere. Found on the second
/// audit pass, after the primitive validation was already in.
#[tokio::test]
async fn create_symbol_refuses_a_graphics_argument_that_is_not_an_array() {
    for (label, args) in [
        (
            "top level",
            json!({
                "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
                "graphics": "rect"
            }),
        ),
        (
            "per unit",
            json!({
                "units": [{
                    "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
                    "graphics": { "type": "rect" }
                }]
            }),
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("na.kicad_sym");
        let mut args = args;
        args["library_path"] = json!(lib.to_string_lossy());
        args["name"] = json!("NA");
        args["reference_prefix"] = json!("U");
        let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
        assert!(res.is_error, "{label} must be refused");
        assert!(
            format!("{res:?}").contains("must be an array of drawing primitives"),
            "{label}: {res:?}"
        );
        assert!(!lib.exists(), "{label}: nothing may be written");
    }
}

/// `line` and `arc` take no fill on either side of the shared schema, but
/// the emitter would have honoured one on a `line`. Schema saying one thing
/// while the writer does another is the same defect as ignoring the field.
#[tokio::test]
async fn create_symbol_refuses_a_fill_on_primitives_that_take_none() {
    for primitive in [
        json!({"type":"line","start":{"x":-1,"y":0},"end":{"x":1,"y":0},
               "stroke_width_mm":0.2,"fill":"outline"}),
        json!({"type":"arc","start":{"x":-1,"y":0},"mid":{"x":0,"y":1},"end":{"x":1,"y":0},
               "stroke_width_mm":0.2,"fill":"none"}),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("f.kicad_sym");
        let res = handle_create_symbol(
            &json!({
                "library_path": lib.to_string_lossy(),
                "name": "F",
                "reference_prefix": "U",
                "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
                "graphics": [primitive]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(res.is_error);
        assert!(format!("{res:?}").contains("takes no 'fill'"), "{res:?}");
        assert!(!lib.exists());
    }
}

/// A drawn body writes pin coordinates exactly as supplied, so a pin with
/// no `x`/`y` has none to write — and `unwrap_or(0.0)` was answering with
/// an origin the caller never gave, in the one branch whose whole promise
/// is not to invent coordinates. Refuse, naming the indexed field, and
/// write nothing. Found in review of #502.
#[tokio::test]
async fn create_symbol_refuses_a_drawn_pin_without_coordinates() {
    let rect = json!({"type":"rect","start":{"x":-5.08,"y":3.81},"end":{"x":5.08,"y":-3.81},
                      "stroke_width_mm":0.254,"fill":"background"});
    for (label, field, args) in [
        (
            "top-level pins, x absent",
            "pins[1].x",
            json!({
                "pins": [
                    {"number":"1","name":"A","type":"passive","x":-2.54,"y":3.81},
                    {"number":"2","name":"K","type":"passive","y":-3.81}
                ],
                "graphics": [rect.clone()]
            }),
        ),
        (
            "top-level pins, y is a string",
            "pins[0].y",
            json!({
                "pins": [
                    {"number":"1","name":"A","type":"passive","x":-2.54,"y":"3.81"}
                ],
                "graphics": [rect.clone()]
            }),
        ),
        (
            "the drawn unit of a multi-unit symbol",
            "units[1].pins[0].x",
            json!({
                "units": [
                    {"pins": [{"number":"1","name":"A","type":"passive","x":-2.54,"y":3.81}]},
                    {"pins": [{"number":"2","name":"K","type":"passive","y":-3.81}],
                     "graphics": [rect.clone()]}
                ]
            }),
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("drawn.kicad_sym");
        let mut args = args;
        args["library_path"] = json!(lib.to_string_lossy());
        args["name"] = json!("DRAWN");
        args["reference_prefix"] = json!("VT");
        let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
        assert!(res.is_error, "{label} must be refused");
        let message = format!("{res:?}");
        assert!(
            message.contains(field),
            "{label}: the refusal must name {field}: {message}"
        );
        assert!(!lib.exists(), "{label}: nothing may be written");
    }
}

/// Supplied geometry supersedes the glyph, so a triangular glyph must not
/// still drive the power split. It did: the power pins were moved to a
/// generated unit and `layout_power_unit` overwrote the coordinates the
/// caller gave, breaking both advertised contracts at once — that geometry
/// wins, and that pin coordinates are written as supplied.
///
/// This test asserted the old behaviour and was declared a scope test, on
/// the reasoning that it only pinned how far the coordinate guard reached.
/// **Declaring a test as scope does not exempt its assertion from being
/// wrong**; it pinned a contract, and the contract was the wrong one.
#[tokio::test]
async fn create_symbol_graphics_suppress_the_glyph_power_split() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("split.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "SPLIT",
            "reference_prefix": "U",
            "glyph": "opamp",
            "pins": [
                {"number":"1","name":"OUT","type":"output","x":7.62,"y":0.0,"angle":180},
                {"number":"2","name":"IN","type":"input","x":-7.62,"y":2.54,"angle":0},
                {"number":"8","name":"V+","type":"power_in","x":0.0,"y":7.62,"angle":270},
                {"number":"4","name":"V-","type":"power_in","x":0.0,"y":-7.62,"angle":90}
            ],
            "graphics": [
                {"type":"poly","points":[{"x":-5.08,"y":5.08},{"x":5.08,"y":0.0},{"x":-5.08,"y":-5.08}],
                 "stroke_width_mm":0.254,"fill":"background"}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let response: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(response["power_pin_count"], 2);
    let content = std::fs::read_to_string(&lib).unwrap();
    assert!(
        !content.contains("\"SPLIT_2_1\""),
        "geometry supersedes the glyph, so there must be no generated power unit:\n{content}"
    );
    assert!(
        content.contains("\"SPLIT_0_1\""),
        "one drawn single unit was expected:\n{content}"
    );
    // The whole point: the caller's own power-pin coordinates survive.
    assert!(
        content.contains("(at 0 7.62 270)") && content.contains("(at 0 -7.62 90)"),
        "the supplied power-pin coordinates were not written as given:\n{content}"
    );
    assert!(
        !content.contains("(rectangle"),
        "no automatic body may be drawn beside the supplied geometry:\n{content}"
    );
}

/// The other half: with the glyph actually rendering — no `graphics` — the
/// split still happens, because a triangle's apex genuinely has no room for
/// power-pin names. Confining the split must not delete it.
#[tokio::test]
async fn create_symbol_triangular_glyph_without_graphics_still_splits_power() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("glyph.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "GLYPH",
            "reference_prefix": "U",
            "glyph": "opamp",
            "pins": [
                {"number":"1","name":"OUT","type":"output","x":7.62,"y":0.0,"angle":180},
                {"number":"2","name":"IN","type":"input","x":-7.62,"y":2.54,"angle":0},
                {"number":"8","name":"V+","type":"power_in"},
                {"number":"4","name":"V-","type":"power_in"}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let response: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(response["power_pin_count"], 2);
    let content = std::fs::read_to_string(&lib).unwrap();
    assert!(
        content.contains("\"GLYPH_2_1\""),
        "the generated power unit is missing:\n{content}"
    );
    // Making `graphics` presence meaningful turned `&[]` into a different
    // request at every call site, and the two that build generated power
    // units must pass `None`, not `Some(&[])` — otherwise the power unit
    // comes out bodyless and the assertion above would not notice, because
    // it checks that the unit exists rather than that it has a body.
    let power_unit = &content[content.find("\"GLYPH_2_1\"").unwrap()..];
    assert!(
        power_unit.contains("(rectangle"),
        "the generated power unit lost its automatic body:\n{content}"
    );
}

/// And with geometry supplied, a power pin is a drawn pin like any other,
/// so it needs coordinates. Before the split was confined, this request
/// succeeded and the pin was placed wherever `layout_power_unit` put it.
#[tokio::test]
async fn create_symbol_drawn_power_pin_without_coordinates_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("nopc.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "NOPC",
            "reference_prefix": "U",
            "glyph": "opamp",
            "pins": [
                {"number":"1","name":"OUT","type":"output","x":7.62,"y":0.0,"angle":180},
                {"number":"8","name":"V+","type":"power_in"}
            ],
            "graphics": [
                {"type":"poly","points":[{"x":-5.08,"y":5.08},{"x":5.08,"y":0.0},{"x":-5.08,"y":-5.08}],
                 "stroke_width_mm":0.254,"fill":"background"}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(res.is_error, "{res:?}");
    let message = format!("{res:?}");
    assert!(
        message.contains("pins[1].x"),
        "the refusal must name the power pin: {message}"
    );
    assert!(!lib.exists(), "nothing may be written");
}

/// The same contract restated where the coordinate is used, so a future
/// drawn call site cannot reintroduce the invented origin without the
/// handler's indexed guard in front of it.
#[test]
fn build_symbol_unit_refuses_to_invent_a_drawn_pin_coordinate() {
    let pins = vec![json!({"number":"1","name":"A","type":"passive","y":3.81})];
    let graphics = vec![
        json!({"type":"rect","start":{"x":-1.0,"y":1.0},"end":{"x":1.0,"y":-1.0},
                               "stroke_width_mm":0.2,"fill":"none"}),
    ];
    let error = match build_symbol_unit(&pins, None, true, Some(&graphics)) {
        Ok(_) => panic!("a drawn pin with no x must not be built"),
        Err(error) => error,
    };
    assert_eq!(
        error.kind,
        crate::tools::library::symbol_ops::SymbolUnitErrorKind::Pins,
        "{error}"
    );
    assert!(error.to_string().contains("exactly as supplied"), "{error}");
}

/// `fill` is schema-required on the three closed shapes, and a missing one
/// was emitted as `(fill (type none))` — Konnect choosing an unfilled body
/// where the stock libraries use KiCad's pale `background`. Found in review
/// of #502.
#[tokio::test]
async fn create_symbol_requires_a_fill_on_the_closed_primitives() {
    for primitive in [
        json!({"type":"rect","start":{"x":-2.0,"y":2.0},"end":{"x":2.0,"y":-2.0},
               "stroke_width_mm":0.2}),
        json!({"type":"circle","center":{"x":0.0,"y":0.0},"radius_mm":1.0,
               "stroke_width_mm":0.2}),
        json!({"type":"poly","points":[{"x":0.0,"y":0.0},{"x":1.0,"y":1.0},{"x":2.0,"y":0.0}],
               "stroke_width_mm":0.2}),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("nofill.kicad_sym");
        let res = handle_create_symbol(
            &json!({
                "library_path": lib.to_string_lossy(),
                "name": "NOFILL",
                "reference_prefix": "U",
                "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270}],
                "graphics": [primitive.clone()]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(res.is_error, "{primitive} must be refused");
        let message = format!("{res:?}");
        assert!(
            message.contains("'fill' is required"),
            "{primitive}: {message}"
        );
        assert!(!lib.exists(), "{primitive}: nothing may be written");
    }
}

/// Every primitive schema declares `additionalProperties: false`, but the
/// dispatch does not enforce a `oneOf` inside an array item, so an extra
/// key reached the emitter and was dropped without a word — including a key
/// that is real on a different primitive. Found in review of #502.
#[tokio::test]
async fn create_symbol_rejects_unknown_keys_on_a_primitive() {
    for (key, primitive) in [
        (
            "colour",
            json!({"type":"rect","start":{"x":-2.0,"y":2.0},"end":{"x":2.0,"y":-2.0},
                   "stroke_width_mm":0.2,"fill":"none","colour":"red"}),
        ),
        (
            // Real on `circle`, not on `rect`: the set is per primitive.
            "radius_mm",
            json!({"type":"rect","start":{"x":-2.0,"y":2.0},"end":{"x":2.0,"y":-2.0},
                   "stroke_width_mm":0.2,"fill":"none","radius_mm":9.0}),
        ),
        (
            "layer",
            json!({"type":"line","start":{"x":-1.0,"y":0.0},"end":{"x":1.0,"y":0.0},
                   "stroke_width_mm":0.2,"layer":"F.SilkS"}),
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("extra.kicad_sym");
        let res = handle_create_symbol(
            &json!({
                "library_path": lib.to_string_lossy(),
                "name": "EXTRA",
                "reference_prefix": "U",
                "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270}],
                "graphics": [primitive.clone()]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(res.is_error, "{primitive} must be refused");
        let message = format!("{res:?}");
        assert!(
            message.contains(&format!("graphics[0].{key}")),
            "the refusal must name the offending key: {message}"
        );
        assert!(!lib.exists(), "{primitive}: nothing may be written");
    }
}

/// `graphics_primitive_keys` copies the schema's property list by hand,
/// because the dispatch cannot enforce the nested `oneOf`. This is what
/// keeps the copy honest — and it also pins the property that makes one
/// list serve for both checks: on these primitives every declared property
/// is also required.
#[test]
fn symbol_graphics_allow_exactly_the_schema_keys() {
    let schema = symbol_graphics_schema();
    let branches = schema["items"]["oneOf"].as_array().expect("oneOf");
    assert_eq!(branches.len(), 5);
    for branch in branches {
        let kind = branch["properties"]["type"]["const"].as_str().unwrap();
        let mut declared: Vec<&str> = branch["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        declared.sort_unstable();
        let mut required: Vec<&str> = branch["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort_unstable();
        assert_eq!(declared, required, "{kind}: every property is required");
        let mut accepted = graphics_primitive_keys(kind).expect(kind).to_vec();
        accepted.sort_unstable();
        assert_eq!(accepted, declared, "{kind}: validator against schema");
    }
    assert!(graphics_primitive_keys("polyline").is_none());
}

/// An explicitly empty list is a coherent request — "give me no body at
/// all" — and #501 suppresses the automatic body "whenever `graphics` is
/// given for that unit". `[]` is given. This asserted the opposite until
/// review caught it: `graphics_arg` collapsed `[]` into absent, so the one
/// request that cannot be expressed any other way drew a rectangle.
///
/// It must still not be confused with the malformed cases above, which
/// refuse.
#[tokio::test]
async fn create_symbol_accepts_an_empty_graphics_list_as_no_body_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("e.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "E",
            "reference_prefix": "U",
            "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
            "graphics": []
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    let content = std::fs::read_to_string(&lib).unwrap();
    assert!(
        !content.contains("(rectangle"),
        "`graphics: []` is given, so no automatic body may be drawn:\n{content}"
    );
    assert!(
        content.contains("(pin passive"),
        "the pins must still be written:\n{content}"
    );
    assert!(
        copperpour_sexp::parser::parse_sexp(&content).is_ok(),
        "a bodyless symbol must still parse:\n{content}"
    );
}

/// Omitting `graphics` entirely is the other half of that contract, and is
/// what keeps the correction above from being a silent behaviour change for
/// every existing caller: no key means the automatic body, as before.
#[tokio::test]
async fn create_symbol_without_a_graphics_key_still_draws_the_automatic_body() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("auto.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "AUTO",
            "reference_prefix": "U",
            "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    assert!(
        std::fs::read_to_string(&lib)
            .unwrap()
            .contains("(rectangle"),
        "an omitted `graphics` must still get the automatic body"
    );
}

/// Geometry sent at the top level while `units` is in use would never reach
/// the file. Losing a redundant pin list to `units` is one thing; losing a
/// drawing and reporting success is another.
#[tokio::test]
async fn create_symbol_refuses_top_level_graphics_when_units_are_given() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("u.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "U",
            "reference_prefix": "U",
            "units": [{"pins":[{"number":"1","name":"A","type":"passive","x":-2.54,"y":3.81,"angle":270,"length":0.0}]}],
            "graphics": [{"type":"rect","start":{"x":-5.08,"y":3.81},"end":{"x":5.08,"y":-3.81},
                          "stroke_width_mm":0.254,"fill":"background"}]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(res.is_error);
    assert!(format!("{res:?}").contains("units[].graphics"), "{res:?}");
    assert!(!lib.exists(), "nothing may be written");
}

/// A glyph that geometry replaced is reported, not discarded in silence.
#[tokio::test]
async fn create_symbol_warns_when_graphics_replace_a_requested_glyph() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("g.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "G",
            "reference_prefix": "U",
            "glyph": "opamp",
            "pins": [{"number":"1","name":"P","type":"passive","x":0.0,"y":5.08,"angle":270,"length":0.0}],
            "graphics": [{"type":"rect","start":{"x":-2,"y":2},"end":{"x":2,"y":-2},
                          "stroke_width_mm":0.2,"fill":"none"}]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{res:?}");
    assert!(
        format!("{res:?}").contains("glyph 'opamp' was not drawn"),
        "the discarded glyph must be reported: {res:?}"
    );
}

/// The point of building both schemas from one function: the geometry
/// vocabulary cannot drift from `set_footprint_graphics`. If someone adds a
/// primitive to one, this fails until they have thought about the other.
#[test]
fn symbol_graphics_share_the_footprint_primitive_vocabulary() {
    let names = |schema: &serde_json::Value| -> Vec<String> {
        schema["items"]["oneOf"]
            .as_array()
            .expect("oneOf")
            .iter()
            .map(|v| {
                v["properties"]["type"]["const"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    let symbol = symbol_graphics_schema();
    let footprint = crate::tools::footprint_graphics::graphics_schema_with_fill(&["none", "solid"]);
    assert_eq!(names(&symbol), names(&footprint));
    // Fill is the one deliberate difference.
    let fill = |s: &serde_json::Value, i: usize| {
        s["items"]["oneOf"][i]["properties"]["fill"]["enum"].clone()
    };
    assert_eq!(fill(&symbol, 2), json!(["none", "outline", "background"]));
    assert_eq!(fill(&footprint, 2), json!(["none", "solid"]));
}

#[tokio::test]
async fn create_symbol_emits_kicad10_library_match_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("modern.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "Modern",
        "reference_prefix": "U",
        "pins": [
            {"number":"1","name":"IN","type":"input","x":-7.62,"y":0.0,"angle":0}
        ]
    });

    let result = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error);
    let output = std::fs::read_to_string(&lib).unwrap();

    assert!(output.contains("(version 20251024)"), "{output}");
    assert!(output.contains("(generator_version \"10.0\")"), "{output}");
    assert!(output.contains("(exclude_from_sim no)"), "{output}");
    assert!(output.contains("(in_pos_files yes)"), "{output}");
    assert!(
        output.contains("(duplicate_pin_numbers_are_jumpers no)"),
        "{output}"
    );
    assert!(
        output.contains("(property \"Description\" \"\""),
        "{output}"
    );
    assert!(output.contains("(show_name no)"), "{output}");
    assert!(output.contains("(do_not_autoplace no)"), "{output}");
    assert!(output.contains("(hide yes)"), "{output}");
    assert!(output.contains("(embedded_fonts no)"), "{output}");
}

#[tokio::test]
async fn create_symbol_writes_datasheet_and_normalizes_the_placeholder() {
    // `~` must never reach the file: KiCad's library loader normalises it to
    // "" and its lib_symbols loader does not, so a symbol carrying it fails
    // ERC's library-match check for as long as it exists.
    let tmp = tempfile::tempdir().unwrap();
    let pins = json!([{"number":"1","name":"IN","type":"input","x":-7.62,"y":0.0,"angle":0}]);

    for (i, (given, written)) in [
        (
            json!("https://example.com/ds.pdf"),
            "https://example.com/ds.pdf",
        ),
        (json!("~"), ""),
        (serde_json::Value::Null, ""),
    ]
    .into_iter()
    .enumerate()
    {
        let lib = tmp.path().join(format!("ds{i}.kicad_sym"));
        let mut args = json!({
            "library_path": lib.to_string_lossy(),
            "name": "DS",
            "reference_prefix": "U",
            "pins": pins,
        });
        if !given.is_null() {
            args["datasheet"] = given;
        }

        let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error);
        let output = std::fs::read_to_string(&lib).unwrap();
        assert!(
            output.contains(&format!("(property \"Datasheet\" \"{written}\"")),
            "{output}"
        );
    }
}

#[tokio::test]
async fn create_symbol_single_unit_uses_unit_0_only() {
    // Regression: without `units`, a symbol is one sub-symbol NAME_0_1 and
    // creates no NAME_1_1 unit (unchanged from before multi-unit support).
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("s.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "SINGLE",
        "reference_prefix": "U",
        "pins": [{"number":"1","name":"A","type":"passive","x":-5.08,"y":0.0,"angle":0,"length":2.54}]
    });
    handle_create_symbol(&args, &test_ctx()).await.unwrap();
    let c = std::fs::read_to_string(&lib).unwrap();
    assert!(
        c.contains("(symbol \"SINGLE_0_1\""),
        "single unit lives in _0_1:\n{c}"
    );
    assert!(
        !c.contains("SINGLE_1_1"),
        "single unit must not create a _1_1 unit"
    );
}

/// A 14-pin module with pins declared on two facing columns, as the report
/// describes it. `names` decides how wide the body has to be.
async fn module_response(names: &[&str], x: f64) -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("mod.kicad_sym");
    let pins: Vec<serde_json::Value> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            // Facing columns: each row carries a left and a right pin, which
            // is what makes two long names run into each other across the
            // body and forces it wider.
            let left = i % 2 == 0;
            json!({
                "number": (i + 1).to_string(),
                "name": name,
                "type": "bidirectional",
                "x": if left { -x } else { x },
                "y": (i / 2) as f64 * 2.54,
                "angle": if left { 0 } else { 180 },
                "length": 2.54
            })
        })
        .collect();
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "MODULE",
            "reference_prefix": "U",
            "pins": pins
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{:?}", res.content);
    serde_json::from_str(&result_text(&res)).unwrap()
}

fn pin_x(report: &serde_json::Value, number: &str) -> f64 {
    report["units"][0]["pins"]
        .as_array()
        .expect("units[0].pins is an array")
        .iter()
        .find(|p| p["number"] == number)
        .expect("pin present")["x"]
        .as_f64()
        .expect("x is a number")
}

/// The reported case: long pin names widen the body, the pins slide out to
/// meet it, and the response now says where they went.
#[tokio::test]
async fn long_pin_names_move_the_pins_and_the_response_reports_it() {
    let long = ["D4/A4/SDA/P0.04"; 14];
    let report = module_response(&long, 19.05).await;

    let first = &report["units"][0]["pins"][0];
    assert_eq!(report["units"][0]["body"], "rectangle");
    assert!(
        first["requested"].is_object(),
        "a moved pin carries the position that was asked for: {first}"
    );
    assert_eq!(first["requested"]["x"], json!(-19.05));
    assert!(
        pin_x(&report, "1") < -19.05,
        "the pin must sit further out than requested, got {}",
        pin_x(&report, "1")
    );

    let warnings = report["warnings"]
        .as_array()
        .expect("a move is worth a warning");
    assert_eq!(warnings.len(), 1, "one summary per unit, not one per pin");
    let text = warnings[0].as_str().unwrap();
    assert!(text.contains("14 of 14 pins"), "{text}");
    assert!(text.contains("unit 1"), "{text}");
}

/// #747: a pin slid out to the widened body is reported where the file
/// puts it. KiCad 10.0.6 resaves this symbol with its pins at ±27.94; the
/// response used to say `-27.939999999999998`.
#[tokio::test]
async fn slid_out_pins_report_the_written_coordinate() {
    let names: Vec<String> = (0..12).map(|i| format!("VERYLONGPINNAME{i}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let report = module_response(&names, 7.62).await;

    let noisy = crate::tools::noisy_numbers(&report);
    assert!(noisy.is_empty(), "reported {noisy:?}");
    let pins = report["units"][0]["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 12, "{report}");
    for pin in pins {
        let x = pin["x"].as_f64().unwrap();
        assert_eq!(x.abs(), 27.94, "{pin}");
    }
}

/// The other row of the report's table: short names need no extra width, so
/// the pins stay where they were put and nothing is warned about.
#[tokio::test]
async fn short_pin_names_leave_the_pins_alone() {
    let short = ["D4/SDA"; 14];
    let report = module_response(&short, 19.05).await;

    assert_eq!(pin_x(&report, "1"), -19.05);
    assert!(
        report["units"][0]["pins"][0]["requested"].is_null(),
        "an unmoved pin needs no `requested`: {}",
        report["units"][0]["pins"][0]
    );
    assert!(
        report["warnings"].is_null(),
        "nothing moved, so nothing to warn about: {}",
        report["warnings"]
    );
}

/// Two symbols with identical pin coordinates but different name lengths
/// come out with different connection points. That is the surprise the
/// report is about, and the response is what makes it visible.
#[tokio::test]
async fn identical_coordinates_can_still_resolve_differently() {
    let long = module_response(&["D4/A4/SDA/P0.04"; 14], 19.05).await;
    let short = module_response(&["D4/SDA"; 14], 19.05).await;

    assert_ne!(
        pin_x(&long, "1"),
        pin_x(&short, "1"),
        "same request, different result — the caller has to be told"
    );
}

/// Every pin on an edge is aligned to it, names or no names. A ragged
/// column is squared up even with `show_pin_names` off, so the response has
/// to report that too.
#[tokio::test]
async fn pins_are_aligned_to_the_body_edge_even_without_names() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("ragged.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "RAGGED",
            "reference_prefix": "U",
            "show_pin_names": false,
            "pins": [
                {"number":"1","name":"A","type":"input","x":-7.62,"y":0.0,"angle":0,"length":2.54},
                {"number":"2","name":"B","type":"input","x":-12.7,"y":2.54,"angle":0,"length":2.54}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{:?}", res.content);
    let report: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();

    assert_eq!(
        pin_x(&report, "1"),
        pin_x(&report, "2"),
        "both pins end up on the same edge"
    );
    let moved = report["units"][0]["pins"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["requested"].is_object())
        .count();
    assert_eq!(moved, 1, "the pin that was pulled out is marked: {report}");
}

/// A glyph places its pins by type — documented, intended, and reported
/// without a warning, or every correct call would carry one.
#[tokio::test]
async fn a_glyph_unit_reports_its_placement_without_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("gate.kicad_sym");
    let res = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "GATE",
            "reference_prefix": "U",
            "glyph": "inverter",
            "pins": [
                {"number":"1","name":"A","type":"input","x":100.0,"y":100.0},
                {"number":"2","name":"Y","type":"output","x":200.0,"y":200.0}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{:?}", res.content);
    let report: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();

    assert_eq!(report["units"][0]["body"], "inverter");
    assert_ne!(
        pin_x(&report, "1"),
        100.0,
        "a glyph ignores the requested position"
    );
    assert!(
        report["warnings"].is_null(),
        "a glyph move is intended, not a surprise: {}",
        report["warnings"]
    );
}

#[tokio::test]
async fn create_symbol_accepts_all_12_kicad_pin_types() {
    // One pin per valid electrical type; the generated library must carry
    // each type verbatim and still parse (#55).
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("types.kicad_sym");
    let pins: Vec<serde_json::Value> = ALLOWED_PIN_ELECTRICAL_TYPES
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({
                "number": (i + 1).to_string(),
                "name": format!("P{}", i + 1),
                "type": t,
                "x": -7.62, "y": (i as f64) * 2.54, "angle": 0, "length": 2.54
            })
        })
        .collect();
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "ALL_TYPES",
        "reference_prefix": "U",
        "pins": pins
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(
        !res.is_error,
        "all valid types must pass: {:?}",
        res.content
    );
    let c = std::fs::read_to_string(&lib).unwrap();
    for t in ALLOWED_PIN_ELECTRICAL_TYPES {
        assert!(
            c.contains(&format!("(pin {} line", t)),
            "missing pin type {t}:\n{c}"
        );
    }
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "generated symbol doesn't parse"
    );
}

#[tokio::test]
async fn create_symbol_rejects_not_connected_with_suggestion() {
    // KiCAD's enum is `no_connect`; `not_connected` used to be interpolated
    // verbatim, producing a library eeschema refuses to load (#55).
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("nc.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "BAD_NC",
        "reference_prefix": "U",
        "pins": [
            {"number":"1","name":"NC","type":"not_connected","x":-5.08,"y":0.0}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(res.is_error, "not_connected must be rejected");
    let text = result_text(&res);
    assert!(
        text.contains("not_connected"),
        "error must name the invalid token: {text}"
    );
    assert!(
        text.contains("no_connect"),
        "error must suggest the valid spelling: {text}"
    );
    assert!(
        !lib.exists(),
        "nothing may be written when validation fails"
    );
}

#[tokio::test]
async fn create_symbol_rejects_dual_electrical_type() {
    // "output bidirectional" is two types in one string — KiCAD expects
    // exactly one (#55, bug 2).
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("dual_type.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "BAD_DUAL",
        "reference_prefix": "U",
        "pins": [
            {"number":"1","name":"IO","type":"output bidirectional","x":-5.08,"y":0.0}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(res.is_error, "dual electrical type must be rejected");
    let text = result_text(&res);
    assert!(
        text.contains("output bidirectional"),
        "error must name the invalid token: {text}"
    );
    assert!(!lib.exists(), "nothing may be written on failure");
}

#[tokio::test]
async fn create_symbol_invalid_type_in_multi_unit_writes_nothing() {
    // The multi-unit and power-pin paths validate too, and an existing
    // library file must be left untouched on failure.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("existing.kicad_sym");
    let before = "(kicad_symbol_lib\n  (version 20240108)\n  (generator \"konnect\")\n)\n";
    std::fs::write(&lib, before).unwrap();
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "BAD_MULTI",
        "reference_prefix": "U",
        "units": [
            { "pins": [{"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}] },
            { "pins": [{"number":"2","name":"B","type":"totem_pole","x":-5.08,"y":0.0}] }
        ],
        "power_pins": [
            {"number":"3","name":"VCC","type":"power_in","x":0.0,"y":5.08,"angle":270}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(res.is_error, "invalid type in unit 2 must be rejected");
    assert!(result_text(&res).contains("totem_pole"));
    assert_eq!(
        std::fs::read_to_string(&lib).unwrap(),
        before,
        "existing library must be untouched on failure"
    );
}

#[tokio::test]
async fn create_symbol_multi_unit_emits_units_and_common() {
    // A dual op-amp: two signal units + power pins as a dedicated 3rd unit.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("dual.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "DUAL_OPAMP",
        "reference_prefix": "U",
        "value": "DUAL_OPAMP",
        "units": [
            { "pins": [
                {"number":"3","name":"+","type":"input","x":-10.16,"y":2.54,"angle":0,"length":2.54},
                {"number":"2","name":"-","type":"input","x":-10.16,"y":-2.54,"angle":0,"length":2.54},
                {"number":"1","name":"~","type":"output","x":10.16,"y":0.0,"angle":180,"length":2.54}
            ]},
            { "pins": [
                {"number":"5","name":"+","type":"input","x":-10.16,"y":2.54,"angle":0,"length":2.54},
                {"number":"6","name":"-","type":"input","x":-10.16,"y":-2.54,"angle":0,"length":2.54},
                {"number":"7","name":"~","type":"output","x":10.16,"y":0.0,"angle":180,"length":2.54}
            ]}
        ],
        "power_pins": [
            {"number":"8","name":"V+","type":"power_in","x":0.0,"y":7.62,"angle":270,"length":2.54},
            {"number":"4","name":"V-","type":"power_in","x":0.0,"y":-7.62,"angle":90,"length":2.54}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error);
    let c = std::fs::read_to_string(&lib).unwrap();
    // Two signal units + a dedicated power unit (unit 3). No common _0_1,
    // and the power pins are NOT drawn on every unit.
    assert!(
        !c.contains("DUAL_OPAMP_0_1"),
        "multi-unit must not use a common _0_1:\n{c}"
    );
    assert!(
        c.contains("(symbol \"DUAL_OPAMP_1_1\""),
        "missing signal unit 1"
    );
    assert!(
        c.contains("(symbol \"DUAL_OPAMP_2_1\""),
        "missing signal unit 2"
    );
    assert!(
        c.contains("(symbol \"DUAL_OPAMP_3_1\""),
        "missing dedicated power unit 3"
    );
    assert!(
        !c.contains("DUAL_OPAMP_4_1"),
        "should be exactly three units"
    );
    // The power pins appear once (in the power unit), not per signal unit.
    assert_eq!(
        c.matches("\"V+\"").count(),
        1,
        "V+ must appear exactly once"
    );
    assert_eq!(
        c.matches("\"V-\"").count(),
        1,
        "V- must appear exactly once"
    );
    // A body rectangle per unit (2 signal + 1 power).
    assert_eq!(c.matches("(rectangle").count(), 3, "one body per unit");
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "multi-unit symbol doesn't parse"
    );
}

/// Shared power pins with their default angle used to reach the automatic
/// rectangular builder unchanged. Both pins were consequently placed at
/// one coordinate, silently shorting the generated symbol's supply rails.
#[tokio::test]
async fn multi_unit_power_pins_are_distributed_within_the_power_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("power-layout.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "POWER_LAYOUT",
        "reference_prefix": "U",
        "units": [{ "pins": [
            {"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}
        ]}],
        "power_pins": [
            {"number":"14","name":"VCC","type":"power_in","x":0.0,"y":0.0},
            {"number":"7","name":"GND","type":"power_in","x":0.0,"y":0.0}
        ]
    });

    let result = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!result.is_error);
    let content = std::fs::read_to_string(&lib).unwrap();
    let marker = "(symbol \"POWER_LAYOUT_2_1\"";
    let start = content.find(marker).expect("dedicated power unit");
    let (_, end) = find_balanced_block(&content, start).expect("balanced power unit");
    let power_unit = &content[start..end];
    let positions: Vec<(f64, f64, f64)> = find_block_starts(power_unit, "pin")
        .into_iter()
        .filter_map(|pin_start| find_balanced_block(power_unit, pin_start))
        .map(|(pin_start, pin_end)| {
            let pin = parse_sexp(&power_unit[pin_start..pin_end]).unwrap();
            copperpour_sexp::schematic::parse_at(&pin).unwrap()
        })
        .collect();

    assert_eq!(positions.len(), 2, "expected two pins in {power_unit}");
    assert_ne!(
        (positions[0].0, positions[0].1),
        (positions[1].0, positions[1].1),
        "VCC and GND must not share a connection point: {power_unit}"
    );
    assert!(positions[0].1 > 0.0 && positions[1].1 < 0.0);
    assert_eq!((positions[0].2, positions[1].2), (270.0, 90.0));
}

/// Distinct explicit angles already select opposite sides correctly. Their
/// order must not be reinterpreted as VCC-first/GND-second by auto-layout.
#[tokio::test]
async fn multi_unit_power_pins_preserve_distinct_explicit_angles() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("explicit-power-layout.kicad_sym");
    let result = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "EXPLICIT_POWER",
            "reference_prefix": "U",
            "units": [{ "pins": [
                {"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}
            ]}],
            // Reversed semantic order and the two-pin top group are
            // intentional: explicit angles and positions remain authoritative.
            "power_pins": [
                {"number":"7","name":"GND","type":"power_in","x":0.0,"y":-7.62,"angle":90},
                {"number":"6","name":"VREF","type":"power_in","x":-2.54,"y":7.62,"angle":270},
                {"number":"14","name":"VCC","type":"power_in","x":2.54,"y":7.62,"angle":270}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error);
    let content = std::fs::read_to_string(&lib).unwrap();
    let marker = "(symbol \"EXPLICIT_POWER_2_1\"";
    let start = content.find(marker).expect("dedicated power unit");
    let (_, end) = find_balanced_block(&content, start).expect("balanced power unit");
    let power_unit = &content[start..end];
    let mut gnd = None;
    let mut vref = None;
    let mut vcc = None;
    for (pin_start, pin_end) in find_block_starts(power_unit, "pin")
        .into_iter()
        .filter_map(|pin_start| find_balanced_block(power_unit, pin_start))
    {
        let block = &power_unit[pin_start..pin_end];
        let pin = parse_sexp(block).unwrap();
        let at = copperpour_sexp::schematic::parse_at(&pin).unwrap();
        if block.contains("(name \"GND\"") {
            gnd = Some(at);
        } else if block.contains("(name \"VREF\"") {
            vref = Some(at);
        } else if block.contains("(name \"VCC\"") {
            vcc = Some(at);
        }
    }

    let (_, gnd_y, gnd_angle) = gnd.expect("GND pin");
    let (vref_x, vref_y, vref_angle) = vref.expect("VREF pin");
    let (vcc_x, vcc_y, vcc_angle) = vcc.expect("VCC pin");
    assert!(gnd_y < 0.0 && vcc_y > 0.0, "{power_unit}");
    assert!(vref_y > 0.0 && vref_x < vcc_x, "{power_unit}");
    assert_eq!((gnd_angle, vref_angle, vcc_angle), (90.0, 270.0, 270.0));
}

/// KiCad treats 0 and 360 degrees as the same direction. If their resolved
/// connection points coincide, refusing the request is safer than writing
/// a power unit that shorts itself while reporting success.
#[tokio::test]
async fn equivalent_explicit_power_pin_angles_cannot_share_a_connection_point() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("equivalent-angles.kicad_sym");
    let result = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "EQUIVALENT_ANGLES",
            "reference_prefix": "U",
            "units": [{ "pins": [
                {"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}
            ]}],
            "power_pins": [
                {"number":"14","name":"VCC","type":"power_in","x":0.0,"y":0.0,"angle":0},
                {"number":"7","name":"GND","type":"power_in","x":0.0,"y":0.0,"angle":360}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(result.is_error);
    let message = result_text(&result);
    assert!(message.contains("same connection point"), "{message}");
    assert!(message.contains("14 (VCC)") && message.contains("7 (GND)"));
    assert!(!lib.exists(), "a colliding symbol must not be written");
}

/// Multiple hidden power-pin gates may intentionally represent one
/// physical pin number at one anchor. That is a legal KiCad stack, not a
/// short between distinct supply rails.
#[tokio::test]
async fn identical_power_pin_numbers_may_share_a_connection_point() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("stacked-power.kicad_sym");
    let result = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "STACKED_POWER",
            "reference_prefix": "U",
            "units": [{ "pins": [
                {"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}
            ]}],
            "power_pins": [
                {"number":"14","name":"VCC","type":"power_in","x":0.0,"y":0.0},
                {"number":"14","name":"VCC","type":"power_in","x":0.0,"y":0.0}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{result:?}");
    let content = std::fs::read_to_string(&lib).expect("a legal stacked pin must be written");
    let marker = "(symbol \"STACKED_POWER_2_1\"";
    let start = content.find(marker).expect("dedicated power unit");
    let (_, end) = find_balanced_block(&content, start).expect("balanced power unit");
    let power_unit = &content[start..end];
    let positions: Vec<_> = find_block_starts(power_unit, "pin")
        .into_iter()
        .filter_map(|pin_start| find_balanced_block(power_unit, pin_start))
        .map(|(pin_start, pin_end)| {
            let pin = parse_sexp(&power_unit[pin_start..pin_end]).unwrap();
            copperpour_sexp::schematic::parse_at(&pin).unwrap()
        })
        .collect();
    assert_eq!(positions.len(), 2, "expected a two-pin stack");
    assert_eq!(
        positions[0], positions[1],
        "the shared pin number must stay stacked"
    );
}

/// `power_pin_count` describes generated output, not ignored request data.
#[tokio::test]
async fn power_pins_without_units_report_zero_written_pins() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("ignored-power.kicad_sym");
    let result = handle_create_symbol(
        &json!({
            "library_path": lib.to_string_lossy(),
            "name": "IGNORED_POWER",
            "reference_prefix": "U",
            "pins": [{"number":"1","name":"A","type":"input","x":-5.08,"y":0.0}],
            "power_pins": [
                {"number":"2","name":"VCC","type":"power_in","x":0.0,"y":5.08},
                {"number":"3","name":"GND","type":"power_in","x":0.0,"y":-5.08}
            ]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error);
    let response: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(response["power_pin_count"], 0);
}

async fn make_symbol(glyph: &str, pins: serde_json::Value) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("g.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "G",
        "reference_prefix": "U",
        "glyph": glyph,
        "pins": pins,
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error, "{glyph} create_symbol errored");
    let c = std::fs::read_to_string(&lib).unwrap();
    assert!(
        copperpour_sexp::parser::parse_sexp(&c).is_ok(),
        "{glyph} output doesn't parse:\n{c}"
    );
    c
}

#[tokio::test]
async fn glyph_opamp_draws_triangle_and_orders_inputs_top_to_bottom() {
    // Inputs are placed in the order listed, top first. Passing + then -
    // gives KiCAD's convention (+ on top, - on bottom).
    let c = make_symbol(
        "opamp",
        json!([
            {"number":"3","name":"+","type":"input"},
            {"number":"2","name":"-","type":"input"},
            {"number":"1","name":"OUT","type":"output"}
        ]),
    )
    .await;
    assert!(c.contains("(polyline"), "op-amp draws a triangle:\n{c}");
    assert!(
        !c.contains("(rectangle"),
        "op-amp must not draw a rectangle"
    );
    // Caller x/y are ignored; pins land on the fixed anchors.
    let top = c.find("(at -7.62 2.54 0)").expect("top input anchor");
    let bot = c.find("(at -7.62 -2.54 0)").expect("bottom input anchor");
    assert!(top < bot, "first-listed input (+) is emitted on top");
    // Non-inverting output at the apex.
    assert!(
        c.contains("(pin output line (at 7.62 0 180)"),
        "op-amp output is a plain line at the apex:\n{c}"
    );
}

#[tokio::test]
async fn glyph_opamp_with_power_splits_into_a_rect_power_unit() {
    // A single op-amp carrying its own supply: the triangle has no room for
    // power-pin names, so V+/V- go to a dedicated rectangular power unit
    // (unit 2), like KiCAD's multi-unit op-amps.
    let c = make_symbol(
        "opamp",
        json!([
            {"number":"3","name":"+","type":"input"},
            {"number":"2","name":"-","type":"input"},
            {"number":"6","name":"OUT","type":"output"},
            {"number":"7","name":"V+","type":"power_in"},
            {"number":"4","name":"V-","type":"power_in"}
        ]),
    )
    .await;
    // Two units: G_1_1 (triangle) + G_2_1 (rect power). No single _0_1.
    assert!(
        !c.contains("G_0_1"),
        "a split symbol must not use _0_1:\n{c}"
    );
    assert!(c.contains("(symbol \"G_1_1\""), "signal triangle is unit 1");
    assert!(
        c.contains("(symbol \"G_2_1\""),
        "power is a separate unit 2"
    );
    // Exactly one triangle (the op-amp) and one rectangle (the power unit).
    assert_eq!(c.matches("(polyline").count(), 1, "one triangle body:\n{c}");
    assert_eq!(
        c.matches("(rectangle").count(),
        1,
        "one rectangular power unit"
    );
    // The supply pins appear once, and on the power unit at full-size text.
    assert_eq!(c.matches("\"V+\"").count(), 1, "V+ appears exactly once");
    assert_eq!(c.matches("\"V-\"").count(), 1, "V- appears exactly once");
    assert!(
        c.contains("(name \"V+\" (effects (font (size 1.27 1.27))))"),
        "power-unit names use the full 1.27 font (it's a rectangle):\n{c}"
    );
    // The triangle keeps its signal pins at the compact glyph font.
    assert!(c.contains("(name \"+\" (effects (font (size 0.762 0.762))))"));
    assert!(c.contains("(pin output line (at 7.62 0 180)"));
}

#[tokio::test]
async fn glyph_and_nand_share_body_and_differ_by_output_bubble() {
    let pins = json!([
        {"number":"1","name":"A","type":"input"},
        {"number":"2","name":"B","type":"input"},
        {"number":"3","name":"Y","type":"output"}
    ]);
    let and = make_symbol("and", pins.clone()).await;
    let nand = make_symbol("nand", pins).await;
    // Same AND body (an arc), no rectangle.
    for (g, c) in [("and", &and), ("nand", &nand)] {
        assert!(c.contains("(arc"), "{g} has the AND arc:\n{c}");
        assert!(!c.contains("(rectangle"), "{g} must not draw a rectangle");
    }
    // The only difference is the output pin: AND plain, NAND inverted bubble.
    assert!(
        and.contains("(pin output line (at 7.62 0 180)"),
        "AND output line"
    );
    assert!(
        nand.contains("(pin output inverted (at 7.62 0 180)"),
        "NAND output carries the bubble via an inverted pin:\n{nand}"
    );
    assert!(!nand.contains("(pin output line (at 7.62 0 180)"));
}

#[tokio::test]
async fn glyph_buffer_and_inverter_share_triangle() {
    let pins = json!([
        {"number":"1","name":"A","type":"input"},
        {"number":"2","name":"Y","type":"output"}
    ]);
    let buffer = make_symbol("buffer", pins.clone()).await;
    let inverter = make_symbol("inverter", pins).await;
    // Single input centered on the left, plain vs inverted output.
    assert!(
        buffer.contains("(pin input line (at -7.62 0 0)"),
        "buffer input centered"
    );
    assert!(
        buffer.contains("(pin output line (at 7.62 0 180)"),
        "buffer output line"
    );
    assert!(
        inverter.contains("(pin output inverted (at 7.62 0 180)"),
        "inverter output inverted:\n{inverter}"
    );
}

#[tokio::test]
async fn glyph_schmitt_has_hysteresis_mark_and_optional_bubble() {
    let pins = json!([
        {"number":"1","name":"A","type":"input"},
        {"number":"2","name":"Y","type":"output"}
    ]);
    let schmitt = make_symbol("schmitt", pins.clone()).await;
    let schmitt_inv = make_symbol("schmitt_inverter", pins).await;
    // The hysteresis mark (from KiCAD's 74HC14) is present on both.
    for (g, c) in [("schmitt", &schmitt), ("schmitt_inverter", &schmitt_inv)] {
        assert!(
            c.contains("(xy -1.905 -1.27)") && c.contains("(xy -1.905 1.27)"),
            "{g} draws the hysteresis mark:\n{c}"
        );
    }
    // Non-inverting Schmitt keeps a plain output; the inverter adds the bubble.
    assert!(schmitt.contains("(pin output line (at 7.62 0 180)"));
    assert!(schmitt_inv.contains("(pin output inverted (at 7.62 0 180)"));
}

#[tokio::test]
async fn glyph_or_and_xor_differ_by_the_extra_back_arc() {
    let pins = json!([
        {"number":"1","name":"A","type":"input"},
        {"number":"2","name":"B","type":"input"},
        {"number":"3","name":"Y","type":"output"}
    ]);
    let or = make_symbol("or", pins.clone()).await;
    let xor = make_symbol("xor", pins.clone()).await;
    let nor = make_symbol("nor", pins.clone()).await;
    let xnor = make_symbol("xnor", pins).await;
    // Both have the OR concave back arc; XOR/XNOR add a second offset arc.
    for (g, c) in [("or", &or), ("xor", &xor)] {
        assert!(
            c.contains("(start -3.81 3.81)"),
            "{g} has the OR back arc:\n{c}"
        );
    }
    assert!(
        !or.contains("(start -4.4196 3.81)"),
        "OR has no second back arc"
    );
    assert!(
        xor.contains("(start -4.4196 3.81)"),
        "XOR adds the offset back arc:\n{xor}"
    );
    // Inverting variants carry the output bubble.
    assert!(nor.contains("(pin output inverted (at 7.62 0 180)"));
    assert!(xnor.contains("(pin output inverted (at 7.62 0 180)"));
    assert!(or.contains("(pin output line (at 7.62 0 180)"));
}

#[tokio::test]
async fn pin_style_applies_on_glyph_and_rectangle() {
    // A clock input on a buffer glyph emits the clock pin style.
    let c = make_symbol(
        "buffer",
        json!([
            {"number":"1","name":"CLK","type":"input","style":"clock"},
            {"number":"2","name":"Y","type":"output"}
        ]),
    )
    .await;
    assert!(
        c.contains("(pin input clock (at -7.62 0 0)"),
        "clock style on a glyph input:\n{c}"
    );

    // On the rectangle path, a per-pin style is honored too.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("r.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "R",
        "reference_prefix": "U",
        "pins": [
            {"number":"1","name":"~RST","type":"input","style":"inverted","x":-7.62,"y":0.0,"angle":0,"length":2.54}
        ]
    });
    handle_create_symbol(&args, &test_ctx()).await.unwrap();
    let rc = std::fs::read_to_string(&lib).unwrap();
    // Position is not asserted: the body is sized to fit the pin name, which
    // can slide the pin out to meet it (see symbol_body_rect).
    assert!(
        rc.contains("(pin input inverted (at "),
        "inverted style on a rectangle pin:\n{rc}"
    );
}

#[tokio::test]
async fn hidden_pins_stack_under_a_visible_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("h.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "H",
        "reference_prefix": "J",
        "units": [{
            "graphics": [{"type":"rect","start":{"x":-5.08,"y":5.08},"end":{"x":5.08,"y":-5.08},
                          "stroke_width_mm":0.254,"fill":"background"}],
            "pins": [
                {"number":"A1","name":"GND","type":"passive","x":-7.62,"y":0.0},
                {"number":"A2","name":"GND","type":"passive","x":-7.62,"y":0.0,"hide":true},
                {"number":"A3","name":"GND","type":"passive","x":-7.62,"y":0.0,"hide":true},
                {"number":"B1","name":"SIG","type":"passive","x":7.62,"y":0.0,"angle":180,"hide":false}
            ]
        }]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    let c = std::fs::read_to_string(&lib).unwrap();
    let pin_of = |number: &str| -> String {
        let at = c.find(&format!("(number \"{number}\"")).unwrap();
        let start = c[..at].rfind("(pin ").unwrap();
        c[start..at].to_string()
    };
    assert!(
        !pin_of("A1").contains("(hide yes)"),
        "A1 stays visible:\n{c}"
    );
    assert!(
        pin_of("A2").contains("(length 2.54)\n      (hide yes)"),
        "A2 hidden:\n{c}"
    );
    assert!(pin_of("A3").contains("(hide yes)"), "A3 hidden:\n{c}");
    assert!(
        !pin_of("B1").contains("(hide yes)"),
        "hide:false is drawn:\n{c}"
    );
    assert_eq!(
        c.matches("(hide yes)").count(),
        2 + 3,
        "two pins plus the three hidden fields:\n{c}"
    );

    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let v: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    let pins = v["units"][0]["pins"]
        .as_array()
        .expect("units[0].pins reported");
    let hidden: Vec<&str> = pins
        .iter()
        .filter(|p| p["hide"] == json!(true))
        .map(|p| p["number"].as_str().unwrap())
        .collect();
    assert_eq!(hidden, ["A2", "A3"]);
}

#[tokio::test]
async fn delete_symbol_finds_tab_indented_kicad_saved_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("tabs.kicad_sym");
    let body = "(kicad_symbol_lib\r\n\t(version 20251024)\r\n\
        \t(symbol \"A\"\r\n\t\t(property \"Value\" \"x (y)\")\r\n\
        \t\t(symbol \"A_1_1\"\r\n\t\t\t(pin passive line (at 0 0 0) (length 2.54) (name \"P\") (number \"1\"))\r\n\t\t)\r\n\t)\r\n\
        \t(symbol \"B\"\r\n\t\t(symbol \"B_1_1\")\r\n\t)\r\n)\r\n";
    std::fs::write(&lib, body).unwrap();
    let ctx = test_ctx();

    let nested = json!({"library_path": lib.to_string_lossy(), "symbol_name": "A_1_1"});
    assert!(
        handle_delete_symbol(&nested, &ctx).await.is_err(),
        "a unit sub-symbol is not a library symbol"
    );

    let args = json!({"library_path": lib.to_string_lossy(), "symbol_name": "A"});
    let res = handle_delete_symbol(&args, &ctx).await.unwrap();
    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let after = std::fs::read_to_string(&lib).unwrap();
    assert_eq!(top_level_symbol_names(&after).unwrap(), ["B"]);
    assert_eq!(
        after,
        "(kicad_symbol_lib\r\n\t(version 20251024)\r\n\t(symbol \"B\"\r\n\t\t(symbol \"B_1_1\")\r\n\t)\r\n)\r\n"
    );
}

#[tokio::test]
async fn glyph_falls_back_to_rectangle_on_incompatible_pins() {
    // A NAND glyph given 3 inputs can't be drawn as a 2-input gate; it falls
    // back to a rectangle and reports a warning instead of misrepresenting.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("fb.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "FB",
        "reference_prefix": "U",
        "glyph": "nand",
        "pins": [
            {"number":"1","name":"A","type":"input","x":-7.62,"y":2.54,"angle":0,"length":2.54},
            {"number":"2","name":"B","type":"input","x":-7.62,"y":0.0,"angle":0,"length":2.54},
            {"number":"3","name":"C","type":"input","x":-7.62,"y":-2.54,"angle":0,"length":2.54},
            {"number":"4","name":"Y","type":"output","x":7.62,"y":0.0,"angle":180,"length":2.54}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    let c = std::fs::read_to_string(&lib).unwrap();
    assert!(
        c.contains("(rectangle"),
        "fell back to a rectangle body:\n{c}"
    );
    assert!(!c.contains("(arc"), "must not draw the AND arc on fallback");
    let text = result_text(&res);
    assert!(
        text.contains("warnings") && text.contains("rectangle instead"),
        "fallback reports a warning:\n{text}"
    );
}

#[tokio::test]
async fn glyph_default_applies_to_units_and_quad_nand_layout() {
    // Symbol-level glyph "nand" applies to every signal unit that doesn't
    // override it; power pins stay a rectangular power unit.
    let unit = json!({ "pins": [
        {"number":"1","name":"A","type":"input"},
        {"number":"2","name":"B","type":"input"},
        {"number":"3","name":"Y","type":"output"}
    ]});
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("quad.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(),
        "name": "QUAD_NAND",
        "reference_prefix": "U",
        "glyph": "nand",
        "units": [unit.clone(), unit.clone(), unit.clone(), unit.clone()],
        "power_pins": [
            {"number":"14","name":"VCC","type":"power_in","x":0.0,"y":7.62,"angle":270,"length":2.54},
            {"number":"7","name":"GND","type":"power_in","x":0.0,"y":-7.62,"angle":90,"length":2.54}
        ]
    });
    let res = handle_create_symbol(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error);
    let c = std::fs::read_to_string(&lib).unwrap();
    // Four NAND gate units (each an AND arc + inverted output) ...
    assert_eq!(
        c.matches("(arc").count(),
        4,
        "one AND body arc per gate:\n{c}"
    );
    assert_eq!(
        c.matches("(pin output inverted").count(),
        4,
        "four inverted NAND outputs"
    );
    // ... plus a fifth, rectangular power unit.
    assert!(
        c.contains("(symbol \"QUAD_NAND_5_1\""),
        "power unit is unit 5"
    );
    assert!(!c.contains("QUAD_NAND_6_1"), "exactly five units");
    assert_eq!(
        c.matches("(rectangle").count(),
        1,
        "only the power unit is a rectangle"
    );
    assert!(copperpour_sexp::parser::parse_sexp(&c).is_ok());
}

#[tokio::test]
async fn glyph_pin_names_use_the_smaller_font_numbers_stay_default() {
    // Glyph bodies are compact, so pin names use the 0.762 mm text to keep
    // them from overlapping; numbers (outside the body) stay at 1.27 mm.
    let c = make_symbol(
        "nand",
        json!([
            {"number":"1","name":"A","type":"input"},
            {"number":"2","name":"B","type":"input"},
            {"number":"3","name":"Y","type":"output"}
        ]),
    )
    .await;
    assert!(
        c.contains("(name \"A\" (effects (font (size 0.762 0.762))))"),
        "glyph pin names use the compact 0.762 font:\n{c}"
    );
    assert!(
        c.contains("(number \"1\" (effects (font (size 1.27 1.27))))"),
        "glyph pin numbers keep the default 1.27 font"
    );

    // The rectangle path is unchanged (names stay at 1.27).
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("r.kicad_sym");
    let args = json!({
        "library_path": lib.to_string_lossy(), "name": "R", "reference_prefix": "U",
        "pins": [{"number":"1","name":"IN","type":"input","x":-7.62,"y":0.0,"angle":0,"length":2.54}]
    });
    handle_create_symbol(&args, &test_ctx()).await.unwrap();
    let rc = std::fs::read_to_string(&lib).unwrap();
    assert!(
        rc.contains("(name \"IN\" (effects (font (size 1.27 1.27))))"),
        "rectangle pin names keep the default 1.27 font:\n{rc}"
    );
}

mod off_grid_pin_warning_tests {
    use super::super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    async fn serve(
        handler: &McpHandler,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 43,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments
                }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(response["isError"], true, "{name}: {response}");
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    async fn handler() -> McpHandler {
        McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap()
    }

    #[test]
    fn is_on_symbol_grid_checks_1_27_mm_multiples_within_epsilon() {
        for on in [0.0, -0.0, 1.27, -1.27, 2.54, -5.08, 10.16, -27.94, 101.6] {
            assert!(is_on_symbol_grid(on), "{on} must be on the 1.27 mm grid");
        }
        for off in [
            0.635,
            -0.635,
            1.0,
            -5.26,
            -10.34,
            1.2701,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(
                !is_on_symbol_grid(off),
                "{off} must be off the 1.27 mm grid"
            );
        }
    }

    #[tokio::test]
    async fn create_symbol_warns_for_off_grid_pin_and_stays_quiet_on_grid() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("grid.kicad_sym");

        // Explicit off-grid pin: -10.34 is 0.18 mm off the 1.27 mm grid (-10.16),
        // and wide enough that auto-sizing does not slide the pins outward.
        let off = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "OffGrid",
                "reference_prefix": "U",
                "pins": [
                    {"number": "1", "name": "GND", "type": "power_in", "x": -10.34, "y": 0.0, "angle": 0, "length": 2.54},
                    {"number": "2", "name": "OUT", "type": "output", "x": 10.16, "y": 0.0, "angle": 180, "length": 2.54}
                ]
            }),
        )
        .await;
        let warnings = off["warnings"]
            .as_array()
            .expect("off-grid pin must produce a warning");
        assert!(
            warnings.iter().any(|w| {
                let text = w.as_str().unwrap_or("");
                text.contains("unit 1")
                    && text.contains("1.27")
                    && text.contains("pin 1")
                    && text.contains("-10.34")
            }),
            "expected off-grid warning naming unit 1, 1.27 mm, and pin 1 (-10.34, 0); got {warnings:?}"
        );

        // Valid grid-aligned pins: no warning.
        let on = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "OnGrid",
                "reference_prefix": "U",
                "pins": [
                    {"number": "1", "name": "GND", "type": "power_in", "x": -10.16, "y": 0.0, "angle": 0, "length": 2.54},
                    {"number": "2", "name": "OUT", "type": "output", "x": 10.16, "y": 0.0, "angle": 180, "length": 2.54}
                ]
            }),
        )
        .await;
        assert!(
            on.get("warnings").is_none(),
            "grid-aligned pins must not warn: {on}"
        );

        // Glyph ignores caller-supplied off-grid x/y and places pins on-grid: no warning.
        let glyph = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "GlyphIgnored",
                "reference_prefix": "U",
                "glyph": "opamp",
                "pins": [
                    {"number": "1", "name": "+", "type": "input", "x": 1.11, "y": 2.22},
                    {"number": "2", "name": "-", "type": "input", "x": 3.33, "y": 4.44},
                    {"number": "3", "name": "OUT", "type": "output", "x": 5.55, "y": 6.66}
                ]
            }),
        )
        .await;
        assert!(
            glyph.get("warnings").is_none(),
            "glyph auto-placement onto the grid must not warn: {glyph}"
        );
    }

    #[tokio::test]
    async fn create_symbol_warns_for_off_grid_pins_in_drawn_and_multi_unit_symbols() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("drawn_and_multi.kicad_sym");

        // Drawn graphics path writes caller coordinates verbatim.
        let drawn = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "DrawnOff",
                "reference_prefix": "U",
                "graphics": [
                    {"type": "rect", "start": {"x": -2.54, "y": 2.54}, "end": {"x": 2.54, "y": -2.54}, "stroke_width_mm": 0.254, "fill": "background"}
                ],
                "pins": [
                    {"number": "1", "name": "IN", "type": "input", "x": -5.0, "y": 0.0, "angle": 0, "length": 2.54},
                    {"number": "2", "name": "OUT", "type": "output", "x": 5.08, "y": 0.0, "angle": 180, "length": 2.54}
                ]
            }),
        )
        .await;
        let drawn_warnings = drawn["warnings"]
            .as_array()
            .expect("drawn off-grid pin must warn");
        assert!(
            drawn_warnings.iter().any(|w| {
                let text = w.as_str().unwrap_or("");
                text.contains("unit 1") && text.contains("1.27") && text.contains("pin 1")
            }),
            "expected drawn off-grid warning, got {drawn_warnings:?}"
        );

        // Multi-unit path: unit 1 on-grid, unit 2 off-grid on Y, power unit (unit 3) off-grid on X.
        let multi = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "MultiOff",
                "reference_prefix": "U",
                "units": [
                    {
                        "pins": [
                            {"number": "1", "name": "A1", "type": "input", "x": -10.16, "y": 0.0, "angle": 0},
                            {"number": "2", "name": "Y1", "type": "output", "x": 10.16, "y": 0.0, "angle": 180}
                        ]
                    },
                    {
                        "pins": [
                            {"number": "3", "name": "A2", "type": "input", "x": -10.16, "y": 1.0, "angle": 0},
                            {"number": "4", "name": "Y2", "type": "output", "x": 10.16, "y": 0.0, "angle": 180}
                        ]
                    }
                ],
                "power_pins": [
                    {"number": "5", "name": "VCC", "type": "power_in", "x": 0.5, "y": 10.16, "angle": 270},
                    {"number": "6", "name": "GND", "type": "power_in", "x": 0.0, "y": -10.16, "angle": 90}
                ]
            }),
        )
        .await;
        let multi_warnings = multi["warnings"]
            .as_array()
            .expect("multi-unit off-grid pins must warn");
        assert!(
            !multi_warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("unit 1")),
            "on-grid unit 1 must not warn: {multi_warnings:?}"
        );
        assert!(
            multi_warnings.iter().any(|w| {
                let t = w.as_str().unwrap_or("");
                t.contains("unit 2") && t.contains("pin 3") && t.contains("1.27")
            }),
            "off-grid unit 2 must warn: {multi_warnings:?}"
        );
        assert!(
            multi_warnings.iter().any(|w| {
                let t = w.as_str().unwrap_or("");
                t.contains("unit 3") && t.contains("pin 5") && t.contains("1.27")
            }),
            "off-grid power unit 3 must warn: {multi_warnings:?}"
        );
    }

    /// Demonstrates the electrical-connectivity consequence of an off-grid pin
    /// created by `create_symbol`: when placed on the 1.27 mm schematic grid,
    /// a grid-snapped `GND` power symbol connects to the grid-aligned pin but
    /// misses the off-grid pin by 0.18 mm and leaves it unconnected.
    #[tokio::test]
    async fn off_grid_symbol_pin_misses_grid_snapped_power_symbol_while_on_grid_connects() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("power_on_pin_kicad10.kicad_pro");
        std::fs::write(&proj, "{}\n").unwrap();
        // Seed from a committed KiCad 10 fixture that already embeds `power:GND`
        // in `lib_symbols`, so the test runs without an installed KiCad on CI.
        let sch = dir.path().join("power_on_pin_kicad10.kicad_sch");
        std::fs::write(
            &sch,
            include_str!("../../../tests/fixtures/power_on_pin_kicad10.kicad_sch"),
        )
        .unwrap();
        let lib = dir.path().join("local.kicad_sym");

        // 1. Create both symbols: identical except pin 1 X is -10.16 (on-grid) vs -10.34 (0.18 mm off-grid).
        let on = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "OnGrid",
                "reference_prefix": "U",
                "pins": [
                    {"number": "1", "name": "GND", "type": "power_in", "x": -10.16, "y": 0.0, "angle": 0, "length": 2.54},
                    {"number": "2", "name": "OUT", "type": "output", "x": 10.16, "y": 0.0, "angle": 180, "length": 2.54}
                ]
            }),
        )
        .await;
        assert!(on.get("warnings").is_none(), "on-grid must not warn: {on}");

        let off = serve(
            &handler,
            "create_symbol",
            json!({
                "library_path": lib,
                "name": "OffGrid",
                "reference_prefix": "U",
                "pins": [
                    {"number": "1", "name": "GND", "type": "power_in", "x": -10.34, "y": 0.0, "angle": 0, "length": 2.54},
                    {"number": "2", "name": "OUT", "type": "output", "x": 10.16, "y": 0.0, "angle": 180, "length": 2.54}
                ]
            }),
        )
        .await;
        assert!(
            off["warnings"]
                .as_array()
                .is_some_and(|w| w.iter().any(|s| s.as_str().unwrap_or("").contains("1.27"))),
            "off-grid must warn: {off}"
        );

        // 2. Register the project library.
        serve(
            &handler,
            "register_symbol_library",
            json!({
                "nickname": "Local",
                "library_path": lib,
                "scope": "project",
                "project": proj
            }),
        )
        .await;

        // 3. Place both symbols on the 1.27 mm schematic grid via batch_place_components.
        // U10 at (203.2, 101.6) -> pin 1 at (193.04, 101.6) (on-grid).
        // U11 at (254.0, 101.6) -> pin 1 at (243.66, 101.6) (0.18 mm off-grid; nearest grid is 243.84).
        serve(
            &handler,
            "batch_place_components",
            json!({
                "schematic": sch,
                "components": [
                    {"lib_id": "Local:OnGrid", "reference": "U10", "value": "OnGrid", "x": 203.2, "y": 101.6},
                    {"lib_id": "Local:OffGrid", "reference": "U11", "value": "OffGrid", "x": 254.0, "y": 101.6}
                ]
            }),
        )
        .await;

        // 4. Place GND power symbols by coordinate (which snaps to the 1.27 mm grid).
        serve(
            &handler,
            "add_power_symbol",
            json!({ "schematic": sch, "power_net": "GND", "x": 193.04, "y": 101.6 }),
        )
        .await;
        serve(
            &handler,
            "add_power_symbol",
            json!({ "schematic": sch, "power_net": "GND", "x": 243.66, "y": 101.6 }),
        )
        .await;

        // 5. Verify electrical connectivity: U10.1 connects to GND; U11.1 is silently disconnected.
        let u10_net = serve(
            &handler,
            "get_pin_net_name",
            json!({ "schematic": sch, "reference": "U10", "pin_number": "1" }),
        )
        .await;
        assert_eq!(
            u10_net["net"],
            json!("GND"),
            "on-grid U10.1 must connect: {u10_net}"
        );

        let u11_net = serve(
            &handler,
            "get_pin_net_name",
            json!({ "schematic": sch, "reference": "U11", "pin_number": "1" }),
        )
        .await;
        assert!(
            u11_net["net"].is_null(),
            "off-grid U11.1 must miss the grid-snapped GND symbol: {u11_net}"
        );
    }
}
