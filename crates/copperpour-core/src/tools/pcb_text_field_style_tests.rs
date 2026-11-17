//! `set_footprint_field_style` on both of its paths (#520).
//!
//! The file path runs on `tests/fixtures/specctra_two_resistors.kicad_pcb`, a
//! board KiCad 10.0.5 saved (see `specctra_two_resistors.README.md`). That
//! file is the oracle for how KiCad writes a field's font: `(size 1 1)` then
//! `(thickness 0.15)` on the next line for both resistors' Reference and
//! Value, and `(size 1.27 1.27)` with no thickness at all for the Datasheet
//! and Description fields. Two boards KiCad derived from it carry the cases
//! it lacks, a reference held by two footprints and a field with no thickness
//! (see `field_style_kicad10.README.md`). The live path runs through served
//! `tools/call` against the KiCad double the other text tools share.

use super::super::tests::{held_footprint, LiveScene};
use super::*;
use crate::tools::pcb_board::board_mock::ctx_talking_to;
use crate::tools::BoardAccess;
use copperpour_ipc::gen::kiapi;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/specctra_two_resistors.kicad_pcb"
));

/// KiCad's save of [`FIXTURE`] with R2's reference changed to R1.
const DUPLICATE_R1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/field_style_duplicate_reference_kicad10.kicad_pcb"
));

/// KiCad's save of [`FIXTURE`] with the thickness of R1's Reference set to 0.
const R1_NO_THICKNESS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/field_style_no_thickness_kicad10.kicad_pcb"
));

/// The fixture's Reference and Value font, exactly as KiCad wrote it. The
/// board was saved on Windows, so its lines end in CRLF.
const KICAD_FONT: &str = "(font\r\n\t\t\t\t\t(size 1 1)\r\n\t\t\t\t\t(thickness 0.15)\r\n\t\t\t\t)";

/// `board` with the font of one field of one resistor replaced by `font`.
/// Panics unless that field still holds [`KICAD_FONT`], so a changed fixture
/// cannot quietly turn these tests into no-ops.
fn with_font(board: &str, reference: &str, property: &str, value: &str, font: &str) -> String {
    let footprint = board
        .find(&format!("(property \"Reference\" \"{reference}\""))
        .expect("the board holds this resistor");
    let field = footprint
        + board[footprint..]
            .find(&format!("(property \"{property}\" \"{value}\""))
            .expect("the resistor holds this field");
    let start = field + board[field..].find("(font").expect("the field has a font");
    assert!(
        board[start..].starts_with(KICAD_FONT),
        "{reference} {property} is not written as {KICAD_FONT:?}"
    );
    format!(
        "{}{font}{}",
        &board[..start],
        &board[start + KICAD_FONT.len()..]
    )
}

/// A board file in a directory of its own and a context whose KiCad endpoint
/// nothing listens on, which is the file path.
struct Closed {
    _dir: tempfile::TempDir,
    board: std::path::PathBuf,
    ctx: ToolContext,
}

impl Closed {
    fn holding(content: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("two_resistors.kicad_pcb");
        std::fs::write(&board, content).unwrap();
        // Not an empty address: that would read `KICAD_API_SOCKET` and could
        // reach a real KiCad.
        let ctx = ctx_talking_to(format!(
            "ipc://{}",
            dir.path().join("no-kicad-here.sock").display()
        ));
        Self {
            _dir: dir,
            board,
            ctx,
        }
    }

    async fn call(&self, mut arguments: Value) -> (bool, Value) {
        arguments["board"] = json!(self.board.to_string_lossy());
        let result = handle_set_footprint_field_style(&arguments, &self.ctx)
            .await
            .expect("the handler reports failures as results");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        let body = serde_json::from_str(text)
            .unwrap_or_else(|_| panic!("expected a JSON body, got {text}"));
        (result.is_error, body)
    }

    fn content(&self) -> String {
        std::fs::read_to_string(&self.board).unwrap()
    }
}

/// A field's reported values. The fixture's and the KiCad double's fields are
/// all visible and unmirrored, each Reference on `F.SilkS` and each Value on
/// `F.Fab`.
fn style_on(
    layer: &str,
    visible: bool,
    mirrored: bool,
    size_x: f64,
    size_y: f64,
    thickness: Option<f64>,
) -> Value {
    json!({
        "size_x_mm": size_x, "size_y_mm": size_y, "thickness_mm": thickness,
        "layer": layer, "visible": visible, "mirrored": mirrored,
    })
}

/// A Reference field's reported values; see [`style_on`].
fn style(size_x: f64, size_y: f64, thickness: Option<f64>) -> Value {
    style_on("F.SilkS", true, false, size_x, size_y, thickness)
}

/// A Value field's reported values; see [`style_on`].
fn value_style(size_x: f64, size_y: f64, thickness: Option<f64>) -> Value {
    style_on("F.Fab", true, false, size_x, size_y, thickness)
}

// ─── Saved-file path ──────────────────────────────────────────────────────────

/// The issue's repair: every Reference on the board gets the thickness, and
/// nothing else in the KiCad-saved file moves.
#[tokio::test]
async fn the_file_path_sets_every_reference_and_changes_nothing_else() {
    let closed = Closed::holding(FIXTURE);
    let (is_error, body) = closed.call(json!({ "thickness": 0.2 })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "applied");
    assert_eq!(body["source"], "file");
    assert_eq!(body["count"], 2);
    assert_eq!(body["changed_count"], 2);
    assert_eq!(body["undo"], Value::Null);
    assert_eq!(body["fallback_reason"]["kind"], "transport_unreachable");
    assert!(body["warning"].is_string(), "{body}");
    assert_eq!(
        body["fields"],
        json!([
            { "footprint": "R1", "field": "reference", "changed": true,
              "before": style(1.0, 1.0, Some(0.15)), "after": style(1.0, 1.0, Some(0.2)) },
            { "footprint": "R2", "field": "reference", "changed": true,
              "before": style(1.0, 1.0, Some(0.15)), "after": style(1.0, 1.0, Some(0.2)) },
        ])
    );

    let thicker = KICAD_FONT.replace("0.15", "0.2");
    let expected = with_font(FIXTURE, "R1", "Reference", "R1", &thicker);
    let expected = with_font(&expected, "R2", "Reference", "R2", &thicker);
    assert_eq!(closed.content(), expected);
}

/// A field saved with no thickness (KiCad then draws size/8, which is what
/// DRC flags) gets one where KiCad writes it. Both ends are KiCad's: its save
/// of R1's Reference at thickness 0, which leaves the thickness line out, and
/// its save at 0.15, the fixture. Asking for 0.15 must turn the first into
/// the second byte for byte.
#[tokio::test]
async fn a_missing_thickness_is_written_where_kicad_writes_it() {
    // KiCad's own zero-thickness save differs from the fixture in that line
    // alone, so the comparison below rests on nothing but that line.
    assert_eq!(
        R1_NO_THICKNESS,
        with_font(
            FIXTURE,
            "R1",
            "Reference",
            "R1",
            "(font\r\n\t\t\t\t\t(size 1 1)\r\n\t\t\t\t)",
        )
    );
    let closed = Closed::holding(R1_NO_THICKNESS);
    let (is_error, body) = closed
        .call(json!({ "references": ["R1"], "thickness": 0.15 }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["fields"],
        json!([{
            "footprint": "R1", "field": "reference", "changed": true,
            "before": style(1.0, 1.0, None), "after": style(1.0, 1.0, Some(0.15)),
        }])
    );
    assert_eq!(closed.content(), FIXTURE);
}

#[tokio::test]
async fn the_file_path_sets_size_on_named_fields_and_keeps_the_thickness() {
    let closed = Closed::holding(FIXTURE);
    let (is_error, body) = closed
        .call(json!({
            "references": ["R2"], "fields": ["value", "reference"],
            "size_x": 0.8, "size_y": 0.9
        }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["count"], 2);
    // In the order the request named the fields.
    assert_eq!(body["fields"][0]["field"], "value");
    assert_eq!(body["fields"][1]["field"], "reference");
    assert_eq!(
        body["fields"][0]["before"],
        value_style(1.0, 1.0, Some(0.15))
    );
    assert_eq!(
        body["fields"][0]["after"],
        value_style(0.8, 0.9, Some(0.15))
    );
    assert_eq!(body["fields"][1]["before"], style(1.0, 1.0, Some(0.15)));
    assert_eq!(body["fields"][1]["after"], style(0.8, 0.9, Some(0.15)));
    for field in body["fields"].as_array().unwrap() {
        assert_eq!(field["footprint"], "R2");
    }
    // KiCad writes the height first: `(size <height> <width>)`.
    let resized = KICAD_FONT.replace("(size 1 1)", "(size 0.9 0.8)");
    let expected = with_font(FIXTURE, "R2", "Reference", "R2", &resized);
    let expected = with_font(&expected, "R2", "Value", "4.7k", &resized);
    assert_eq!(closed.content(), expected);
    // R1 was not named.
    assert!(closed
        .content()
        .contains(&FIXTURE[..FIXTURE.find("(property \"Reference\" \"R2\"").unwrap()]));
}

/// A saved size is read height first, as KiCad writes it
/// (`common/eda_text.cpp:1050-1051`): a field saved as `(size 2 1)` is 1 mm
/// wide and 2 mm tall, and changing only its width keeps the height first.
#[tokio::test]
async fn a_saved_size_is_read_and_written_height_first() {
    let tall = KICAD_FONT.replace("(size 1 1)", "(size 2 1)");
    let board = with_font(FIXTURE, "R1", "Reference", "R1", &tall);
    let closed = Closed::holding(&board);
    let (is_error, body) = closed
        .call(json!({ "references": ["R1"], "size_x": 0.5 }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["fields"][0]["before"], style(1.0, 2.0, Some(0.15)));
    assert_eq!(body["fields"][0]["after"], style(0.5, 2.0, Some(0.15)));
    let narrow = KICAD_FONT.replace("(size 1 1)", "(size 2 0.5)");
    assert_eq!(
        closed.content(),
        with_font(FIXTURE, "R1", "Reference", "R1", &narrow)
    );
}

#[tokio::test]
async fn a_board_that_already_holds_the_style_is_not_rewritten() {
    let closed = Closed::holding(FIXTURE);
    let (is_error, body) = closed
        .call(json!({ "fields": ["reference", "value"], "size_x": 1.0, "thickness": 0.15 }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "unchanged");
    assert_eq!(body["applied"], false);
    assert_eq!(body["count"], 4);
    assert_eq!(body["changed_count"], 0);
    assert_eq!(closed.content(), FIXTURE);
}

#[tokio::test]
async fn an_unknown_reference_refuses_the_whole_file_edit() {
    let closed = Closed::holding(FIXTURE);
    let (is_error, body) = closed
        .call(json!({ "references": ["R1", "R99"], "thickness": 0.2 }))
        .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "stale_target");
    assert_eq!(body["error"]["target"], "R99");
    assert_eq!(closed.content(), FIXTURE, "R1 must not be edited either");
}

/// A reference two footprints carry names neither: the call is refused with
/// the number of matches and the file is not written. Taking the first match
/// would restyle one of the two and report success.
#[tokio::test]
async fn an_ambiguous_reference_refuses_the_whole_file_edit() {
    assert_eq!(
        DUPLICATE_R1
            .matches("(property \"Reference\" \"R1\"")
            .count(),
        2
    );
    let closed = Closed::holding(DUPLICATE_R1);
    let (is_error, body) = closed
        .call(json!({ "references": ["R1"], "thickness": 0.2 }))
        .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
    assert_eq!(body["error"]["target"], "R1");
    assert_eq!(
        body["error"]["candidates"].as_array().map(Vec::len),
        Some(2),
        "{body}"
    );
    assert_eq!(closed.content(), DUPLICATE_R1, "neither R1 may be edited");
}

/// A save that lands between the read and the write is kept, and the call
/// says so instead of overwriting it with an edit of the older read.
#[tokio::test]
async fn a_board_saved_between_read_and_write_is_refused_and_kept() {
    let closed = Closed::holding(FIXTURE);
    let newer = with_font(
        FIXTURE,
        "R2",
        "Value",
        "4.7k",
        &KICAD_FONT.replace("(size 1 1)", "(size 2 2)"),
    );
    crate::tools::write_hook::set(newer.clone());
    let (is_error, body) = closed.call(json!({ "thickness": 0.2 })).await;
    assert!(
        !crate::tools::write_hook::armed(),
        "the write bypassed the revision check"
    );
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "conflict");
    assert_eq!(closed.content(), newer);
}

#[test]
fn a_font_written_on_one_line_gets_its_thickness_on_that_line() {
    let board = "(kicad_pcb\n\t(footprint \"R\"\n\t\t(property \"Reference\" \"R7\" (at 0 0) \
                 (effects (font (size 1 1)) (justify left)))\n\t)\n)\n";
    let request = parse_request(&json!({ "thickness": 0.12 })).unwrap();
    let mut plan = plan_file(board, &request).unwrap();
    let edit = plan.fields[0].edit.take().expect("an edit is planned");
    let edited = copperpour_sexp::writer::apply_edits(board.to_string(), vec![edit]);
    assert_eq!(
        edited,
        board.replace("(size 1 1)", "(size 1 1) (thickness 0.12)")
    );
}

#[test]
fn a_whole_board_run_skips_what_it_cannot_edit_and_a_named_one_refuses() {
    // The second footprint has no Value property; the third's font has no
    // size at all.
    let board = "(kicad_pcb\n\
        \t(footprint \"A\" (property \"Reference\" \"R1\" (effects (font (size 1 1)))) \
        (property \"Value\" \"1k\" (effects (font (size 1 1)))))\n\
        \t(footprint \"B\" (property \"Reference\" \"TP1\" (effects (font (size 1 1)))))\n\
        \t(footprint \"C\" (property \"Reference\" \"H1\" (effects (font (size 1 1)))) \
        (property \"Value\" \"hole\" (effects (font (bold yes)))))\n\
        )\n";
    let whole = parse_request(&json!({ "fields": ["value"], "thickness": 0.2 })).unwrap();
    let plan = plan_file(board, &whole).unwrap();
    let planned: Vec<&str> = plan
        .fields
        .iter()
        .map(|field| field.footprint.as_str())
        .collect();
    assert_eq!(planned, ["R1"]);
    let skipped: Vec<(&str, &str)> = plan
        .skipped
        .iter()
        .map(|skip| {
            (
                skip["footprint"].as_str().unwrap(),
                skip["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert_eq!(skipped[0].0, "TP1");
    assert!(skipped[0].1.contains("no value field"), "{skipped:?}");
    assert_eq!(skipped[1].0, "H1");
    assert!(skipped[1].1.contains("no (size"), "{skipped:?}");

    let named = parse_request(&json!({
        "references": ["R1", "TP1"], "fields": ["value"], "thickness": 0.2
    }))
    .unwrap();
    let error = plan_file(board, &named).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TextTargetError>(),
            Some(TextTargetError::MissingField { reference, .. }) if reference == "TP1"
        ),
        "{error:#}"
    );
}

#[test]
fn lengths_are_written_as_kicad_writes_them() {
    for (nm, text) in [
        (1_000_000, "1"),
        (150_000, "0.15"),
        (1_270_000, "1.27"),
        (125_000, "0.125"),
        (1, "0.000001"),
        (12_000_000, "12"),
    ] {
        assert_eq!(mm_text(nm), text);
    }
}

// ─── Arguments and registration ───────────────────────────────────────────────

#[test]
fn the_request_must_name_a_property_and_distinct_targets() {
    let field = |args: Value| {
        let refused = parse_request(&args).unwrap_err();
        let crate::mcp::protocol::ToolContent::Text { text } = &refused.content[0] else {
            panic!("expected text content");
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument", "{args}");
        body["error"]["field"].as_str().unwrap().to_string()
    };
    assert_eq!(field(json!({})), "edit");
    assert_eq!(field(json!({ "references": ["R1"] })), "edit");
    assert_eq!(field(json!({ "thickness": 0.0 })), "thickness");
    assert_eq!(field(json!({ "size_x": 251.0 })), "size_x");
    assert_eq!(
        field(json!({ "references": [], "thickness": 0.2 })),
        "references"
    );
    assert_eq!(
        field(json!({ "references": "R1", "thickness": 0.2 })),
        "references"
    );
    assert_eq!(
        field(json!({ "references": ["R1", " "], "thickness": 0.2 })),
        "references"
    );
    assert_eq!(
        field(json!({ "references": ["R1", "R1"], "thickness": 0.2 })),
        "references"
    );
    assert_eq!(
        field(json!({ "fields": ["datasheet"], "thickness": 0.2 })),
        "fields"
    );
    assert_eq!(
        field(json!({ "fields": ["value", "value"], "thickness": 0.2 })),
        "fields"
    );
    assert_eq!(field(json!({ "layer": "Q.Cu" })), "layer");
    assert_eq!(field(json!({ "layer": 3 })), "layer");
    assert_eq!(field(json!({ "visible": "no" })), "visible");
    assert_eq!(field(json!({ "mirror": "flip" })), "mirror");
    // 'keep' is the default, so it alone names nothing to change.
    assert_eq!(field(json!({ "mirror": "keep" })), "edit");

    let request = parse_request(&json!({ "thickness": 0.15 })).unwrap();
    assert_eq!(request.references, None);
    assert_eq!(request.slots, [IpcFootprintFieldSlot::Reference]);
    assert_eq!(request.thickness_nm, Some(150_000));
    assert!(!request.live_only());

    for live in [
        json!({ "layer": "F.Fab" }),
        json!({ "visible": false }),
        json!({ "mirror": "auto" }),
        json!({ "thickness": 0.15, "visible": true }),
    ] {
        assert!(parse_request(&live).unwrap().live_only(), "{live}");
    }
}

/// The layer is named as on a front-side footprint and flipped as KiCad's
/// `FlipLayer` flips it (`copperpour_sexp::layers::flip_layer`), through the
/// board's copper stack; with no layer there is nothing to flip.
#[test]
fn the_back_side_edit_flips_only_the_layer() {
    let two = CopperStack::new(2).unwrap();
    let request = parse_request(&json!({ "layer": "F.Fab", "thickness": 0.2 })).unwrap();
    let back = request.back_edit(two).unwrap().unwrap();
    assert_eq!(back.layer.as_deref(), Some("B.Fab"));
    assert_eq!(
        IpcTextEdit {
            layer: None,
            ..back
        },
        IpcTextEdit {
            layer: None,
            ..request.edit()
        }
    );
    let user = parse_request(&json!({ "layer": "User.3" })).unwrap();
    assert_eq!(
        user.back_edit(two).unwrap().unwrap().layer.as_deref(),
        Some("User.3")
    );
    let inner = parse_request(&json!({ "layer": "In1.Cu" })).unwrap();
    assert_eq!(
        inner
            .back_edit(CopperStack::new(4).unwrap())
            .unwrap()
            .unwrap()
            .layer
            .as_deref(),
        Some("In2.Cu")
    );
    assert!(inner.back_edit(two).is_err());
    let size_only = parse_request(&json!({ "thickness": 0.2 })).unwrap();
    assert_eq!(size_only.back_edit(two).unwrap(), None);
}

#[test]
fn the_tool_prefers_the_live_board_and_requires_only_the_board() {
    let tools = super::super::tools();
    let def = tools
        .iter()
        .find(|tool| tool.name == OPERATION)
        .expect("set_footprint_field_style is registered");
    assert_eq!(def.board_access, BoardAccess::LivePreferredWithFallback);
    assert_eq!(def.input_schema["required"], json!(["board"]));
}

// ─── Live path, through served `tools/call` ───────────────────────────────────

async fn two_resistor_scene() -> LiveScene {
    let scene = LiveScene::new(false).await;
    scene
        .held
        .lock()
        .unwrap()
        .footprints
        .push(held_footprint("fp-r2", "R2"));
    scene
}

#[tokio::test]
async fn the_live_path_sets_every_reference_in_one_undo_step() {
    let scene = two_resistor_scene().await;
    let (is_error, body) = scene.call(OPERATION, json!({ "thickness": 0.2 })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "applied");
    assert_eq!(body["source"], "ipc");
    assert_eq!(body["undo"], "one KiCad undo step");
    assert_eq!(body["count"], 2);
    assert_eq!(body["changed_count"], 2);
    assert_eq!(body["skipped"], json!([]));
    assert_eq!(body["fallback_reason"], Value::Null, "{body}");
    // Sending a footprint whole has a cost the result itself must name.
    assert_eq!(body["warning"], WHOLE_FOOTPRINT_WARNING, "{body}");
    assert_eq!(
        body["fields"],
        json!([
            { "footprint": "R1", "field": "reference", "changed": true,
              "before": style(1.0, 1.0, Some(0.15)), "after": style(1.0, 1.0, Some(0.2)) },
            { "footprint": "R2", "field": "reference", "changed": true,
              "before": style(1.0, 1.0, Some(0.15)), "after": style(1.0, 1.0, Some(0.2)) },
        ])
    );
    {
        let held = scene.held.lock().unwrap();
        assert_eq!((held.updates, held.commits), (2, 1));
        // Only the reference of each footprint changed.
        for (footprint, (kiid, reference)) in held
            .footprints
            .iter()
            .zip([("fp-r1", "R1"), ("fp-r2", "R2")])
        {
            let pristine = held_footprint(kiid, reference);
            assert_eq!(footprint.value_field, pristine.value_field);
            assert_eq!(footprint.position, pristine.position);
            assert_ne!(footprint.reference_field, pristine.reference_field);
        }
    }
    // The saved file was not touched: the board is live.
    assert_eq!(
        std::fs::read_to_string(&scene.board).unwrap(),
        "(kicad_pcb\n\t(version 20250114)\n)\n"
    );

    // Asking again sends nothing.
    let (is_error, body) = scene.call(OPERATION, json!({ "thickness": 0.2 })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "unchanged");
    assert_eq!(body["changed_count"], 0);
    assert_eq!(body["undo"], Value::Null);
    // Nothing was sent, so there is nothing to warn about.
    assert_eq!(body["warning"], Value::Null, "{body}");
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits), (2, 1));
}

/// One shape whichever path runs (#520): the same keys in the body and in
/// each field, with `source` naming the path, so a caller never branches on
/// which one ran. The same request on the same two fields reads the same.
#[tokio::test]
async fn both_paths_answer_with_the_same_shape() {
    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }
    let (is_error, file) = Closed::holding(FIXTURE)
        .call(json!({ "thickness": 0.2 }))
        .await;
    assert!(!is_error, "{file}");
    let scene = two_resistor_scene().await;
    let (is_error, live) = scene.call(OPERATION, json!({ "thickness": 0.2 })).await;
    assert!(!is_error, "{live}");

    assert_eq!(
        (&file["source"], &live["source"]),
        (&json!("file"), &json!("ipc"))
    );
    assert_eq!(keys(&file), keys(&live));
    assert_eq!(keys(&file["fields"][0]), keys(&live["fields"][0]));
    assert_eq!(file["fields"], live["fields"]);
    assert_eq!(
        (&file["skipped"], &live["skipped"]),
        (&json!([]), &json!([]))
    );
}

#[tokio::test]
async fn the_live_path_sends_only_the_footprints_that_change() {
    let scene = two_resistor_scene().await;
    let (is_error, body) = scene
        .call(
            OPERATION,
            json!({ "references": ["R2"], "fields": ["reference", "value"], "size_y": 0.8 }),
        )
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["count"], 2);
    assert_eq!(body["fields"][0]["after"], style(1.0, 0.8, Some(0.15)));
    assert_eq!(
        body["fields"][1]["after"],
        value_style(1.0, 0.8, Some(0.15))
    );
    for field in body["fields"].as_array().unwrap() {
        assert_eq!(field["footprint"], "R2");
    }
    assert_eq!(scene.held.lock().unwrap().updates, 1);

    // R2 already holds it; a whole-board run now sends R1 alone and still
    // reports both.
    let (is_error, body) = scene
        .call(
            OPERATION,
            json!({ "fields": ["reference", "value"], "size_y": 0.8 }),
        )
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["count"], 4);
    assert_eq!(body["changed_count"], 2);
    assert_eq!(scene.held.lock().unwrap().updates, 2);
}

#[tokio::test]
async fn the_live_path_refuses_an_unknown_reference_before_sending_anything() {
    let scene = two_resistor_scene().await;
    let (is_error, body) = scene
        .call(
            OPERATION,
            json!({ "references": ["R1", "R99"], "thickness": 0.2 }),
        )
        .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "stale_target");
    assert_eq!(body["error"]["target"], "R99");
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits), (0, 0));
}

/// A reference two footprints carry is refused with the number of matches
/// before anything is sent, R2 named before it included.
#[tokio::test]
async fn the_live_path_refuses_an_ambiguous_reference_before_sending_anything() {
    let scene = two_resistor_scene().await;
    scene
        .held
        .lock()
        .unwrap()
        .footprints
        .push(held_footprint("fp-r1-again", "R1"));
    let (is_error, body) = scene
        .call(
            OPERATION,
            json!({ "references": ["R2", "R1"], "thickness": 0.2 }),
        )
        .await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
    assert_eq!(body["error"]["target"], "R1");
    assert_eq!(
        body["error"]["candidates"].as_array().map(Vec::len),
        Some(2),
        "{body}"
    );
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits), (0, 0));
}

/// A KiCad that accepts the update and stores none of it is an error naming
/// each field that did not take, never `applied`.
#[tokio::test]
async fn a_kicad_that_ignores_the_update_is_an_error_naming_each_field() {
    let scene = two_resistor_scene().await;
    scene.held.lock().unwrap().ignore_updates = true;
    let (is_error, body) = scene.call(OPERATION, json!({ "thickness": 0.2 })).await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "mutation_outcome_uncertain");
    let reason = body["error"]["reason"].as_str().unwrap();
    for reference in ["R1", "R2"] {
        assert!(
            reason.contains(&format!(
                "{reference}: reference.stroke_width_nm: expected 200000, observed 150000"
            )),
            "{reason}"
        );
    }
}

/// The schema gate refuses malformed targets through the served dispatch,
/// before KiCad is asked anything.
#[tokio::test]
async fn the_served_dispatch_refuses_malformed_arguments_naming_the_field() {
    let scene = two_resistor_scene().await;
    for (arguments, field) in [
        (json!({ "references": [], "thickness": 0.2 }), "references"),
        (
            json!({ "fields": ["datasheet"], "thickness": 0.2 }),
            "fields[0]",
        ),
        (json!({ "thickness": -0.1 }), "thickness"),
        (json!({ "references": ["R1"] }), "edit"),
        (json!({ "visible": "no" }), "visible"),
        (json!({ "mirror": "flip" }), "mirror"),
        (json!({ "layer": "Q.Cu" }), "layer"),
    ] {
        let (is_error, body) = scene.call(OPERATION, arguments.clone()).await;
        assert!(is_error, "{arguments}: {body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{arguments}: {body}");
    }
    assert_eq!(scene.held.lock().unwrap().commits, 0);
}

// ─── Layer, visibility and mirror flag (live only) ────────────────────────────

/// A footprint as KiCad holds one flipped to the back: on `B.Cu`, its
/// Reference on `B.SilkS` and its Value on `B.Fab`, both mirrored.
fn back_footprint(kiid: &str, reference: &str) -> kiapi::board::types::FootprintInstance {
    use kiapi::board::types::BoardLayer;
    let mut footprint = held_footprint(kiid, reference);
    footprint.layer = BoardLayer::BlBCu as i32;
    for (field, layer) in [
        (&mut footprint.reference_field, BoardLayer::BlBSilkS),
        (&mut footprint.value_field, BoardLayer::BlBFab),
    ] {
        let text = field.as_mut().unwrap().text.as_mut().unwrap();
        text.layer = layer as i32;
        set_mirrored(text, true);
    }
    footprint
}

fn set_mirrored(text: &mut kiapi::board::types::BoardText, mirrored: bool) {
    text.text
        .as_mut()
        .unwrap()
        .attributes
        .as_mut()
        .unwrap()
        .mirrored = mirrored;
}

/// R1 on the front and R2 on the back of a board with `copper` copper layers.
async fn front_and_back_scene(copper: u32) -> LiveScene {
    let scene = LiveScene::new(false).await;
    {
        let mut held = scene.held.lock().unwrap();
        held.footprints.push(back_footprint("fp-r2", "R2"));
        held.copper_layers = copper;
    }
    scene
}

/// The plan's case: every Reference onto its own side's Fab layer in one
/// call. R1 gets the layer as named, R2 on the back gets its flip, as KiCad's
/// Flip moves a footprint's texts; each keeps its mirror flag. One undo step.
#[tokio::test]
async fn a_layer_named_for_the_front_puts_a_back_footprints_field_on_the_flipped_layer() {
    let scene = front_and_back_scene(2).await;
    let (is_error, body) = scene.call(OPERATION, json!({ "layer": "F.Fab" })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "applied");
    assert_eq!(body["source"], "ipc");
    assert_eq!(body["undo"], "one KiCad undo step");
    assert_eq!(
        body["fields"],
        json!([
            { "footprint": "R1", "field": "reference", "changed": true,
              "before": style(1.0, 1.0, Some(0.15)),
              "after": style_on("F.Fab", true, false, 1.0, 1.0, Some(0.15)) },
            { "footprint": "R2", "field": "reference", "changed": true,
              "before": style_on("B.SilkS", true, true, 1.0, 1.0, Some(0.15)),
              "after": style_on("B.Fab", true, true, 1.0, 1.0, Some(0.15)) },
        ])
    );
    {
        let held = scene.held.lock().unwrap();
        assert_eq!((held.updates, held.commits, held.layer_queries), (2, 1, 1));
        // The Values were not named and did not move.
        assert_eq!(
            held.footprints[0].value_field,
            held_footprint("fp-r1", "R1").value_field
        );
        assert_eq!(
            held.footprints[1].value_field,
            back_footprint("fp-r2", "R2").value_field
        );
    }

    // Asking again sends nothing.
    let (is_error, body) = scene.call(OPERATION, json!({ "layer": "F.Fab" })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["status"], "unchanged");
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits), (2, 1));
}

/// Changing the layer alone keeps a wrong mirror flag, as KiCad's dialog
/// does; mirror 'auto' sets it from the side each field ends on, and only
/// the footprint whose flag changes is sent.
#[tokio::test]
async fn mirror_auto_sets_each_fields_flag_from_the_side_it_ends_on() {
    let scene = front_and_back_scene(2).await;
    {
        let mut held = scene.held.lock().unwrap();
        let reference = held.footprints[1].reference_field.as_mut().unwrap();
        set_mirrored(reference.text.as_mut().unwrap(), false);
    }
    let (is_error, body) = scene
        .call(OPERATION, json!({ "references": ["R2"], "layer": "F.Fab" }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["fields"][0]["after"],
        style_on("B.Fab", true, false, 1.0, 1.0, Some(0.15))
    );

    let (is_error, body) = scene.call(OPERATION, json!({ "mirror": "auto" })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["changed_count"], 1, "{body}");
    assert_eq!(body["fields"][0]["changed"], false);
    assert_eq!(
        body["fields"][1]["after"],
        style_on("B.Fab", true, true, 1.0, 1.0, Some(0.15))
    );
    let held = scene.held.lock().unwrap();
    // R2 twice, R1 never; the second call named no layer, so it asked for
    // no stack.
    assert_eq!((held.updates, held.commits, held.layer_queries), (2, 2, 1));
}

#[tokio::test]
async fn visible_hides_the_named_fields_in_one_undo_step() {
    let scene = front_and_back_scene(2).await;
    let (is_error, body) = scene
        .call(OPERATION, json!({ "fields": ["value"], "visible": false }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["fields"],
        json!([
            { "footprint": "R1", "field": "value", "changed": true,
              "before": value_style(1.0, 1.0, Some(0.15)),
              "after": style_on("F.Fab", false, false, 1.0, 1.0, Some(0.15)) },
            { "footprint": "R2", "field": "value", "changed": true,
              "before": style_on("B.Fab", true, true, 1.0, 1.0, Some(0.15)),
              "after": style_on("B.Fab", false, true, 1.0, 1.0, Some(0.15)) },
        ])
    );
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits, held.layer_queries), (2, 1, 0));
    assert_eq!(
        held.footprints[0].reference_field,
        held_footprint("fp-r1", "R1").reference_field
    );
}

/// An inner layer flips through the board's own stack, In1.Cu to In2.Cu on
/// four layers. On two layers it has no place on the back footprint, and
/// the call is refused before anything is sent.
#[tokio::test]
async fn an_inner_layer_flips_through_the_boards_copper_stack() {
    let scene = front_and_back_scene(4).await;
    let (is_error, body) = scene.call(OPERATION, json!({ "layer": "In1.Cu" })).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["fields"][0]["after"]["layer"], "In1.Cu");
    assert_eq!(body["fields"][1]["after"]["layer"], "In2.Cu");

    let scene = front_and_back_scene(2).await;
    let (is_error, body) = scene.call(OPERATION, json!({ "layer": "In1.Cu" })).await;
    assert!(is_error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "layer");
    let held = scene.held.lock().unwrap();
    assert_eq!((held.updates, held.commits), (0, 0));
}

/// The layer, visibility and mirror flag have no file path: with no live
/// KiCad they are refused and the saved board is not touched, size and
/// thickness included.
#[tokio::test]
async fn layer_visibility_and_mirror_are_refused_without_a_live_board() {
    for arguments in [
        json!({ "layer": "F.Fab" }),
        json!({ "visible": false }),
        json!({ "mirror": "auto" }),
        json!({ "thickness": 0.2, "visible": true }),
    ] {
        let closed = Closed::holding(FIXTURE);
        let (is_error, body) = closed.call(arguments.clone()).await;
        assert!(is_error, "{arguments}: {body}");
        assert_eq!(body["error"]["kind"], "editor_unavailable", "{arguments}");
        assert_eq!(body["recovery"]["mutation_outcome"], "none", "{arguments}");
        assert_eq!(closed.content(), FIXTURE, "{arguments}");
    }
}

/// The file path reports the layer, visibility and mirror flag as the file
/// holds them, in KiCad 10's form: `(hide yes)` after the layer and `mirror`
/// in the `(justify …)` of the effects.
#[tokio::test]
async fn the_file_path_reports_the_layer_visibility_and_mirror_flag_it_reads() {
    let board = with_font(
        FIXTURE,
        "R1",
        "Reference",
        "R1",
        &format!("{KICAD_FONT} (justify mirror)"),
    );
    let at = board.find("(property \"Reference\" \"R1\"").unwrap();
    let board = format!(
        "{}{}",
        &board[..at],
        board[at..].replacen("(layer \"F.SilkS\")", "(layer \"B.SilkS\") (hide yes)", 1)
    );
    let closed = Closed::holding(&board);
    let (is_error, body) = closed
        .call(json!({ "references": ["R1"], "thickness": 0.2 }))
        .await;
    assert!(!is_error, "{body}");
    assert_eq!(
        body["fields"][0]["before"],
        style_on("B.SilkS", false, true, 1.0, 1.0, Some(0.15))
    );
    assert_eq!(
        body["fields"][0]["after"],
        style_on("B.SilkS", false, true, 1.0, 1.0, Some(0.2))
    );
}
