use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;

/// KiCad 10.0.8's resave of two stock symbols and three small ones; see
/// `symbol_graphics_kicad10.README.md`.
const FIXTURE: &str = include_str!("../../tests/fixtures/symbol_graphics_kicad10.kicad_sym");
/// BOX's body one grid step taller, and CUI_PD-30's first graphic deleted:
/// KiCad resaves it unchanged.
const EDITED: &str = include_str!("../../tests/fixtures/symbol_graphics_kicad10.edited.kicad_sym");
/// A polyline appended to BOX_1_1 and a circle to a new NOTE_0_1: KiCad
/// resaves it unchanged.
const APPENDED: &str =
    include_str!("../../tests/fixtures/symbol_graphics_kicad10.appended.kicad_sym");
/// One item of each kind appended to CUI_PD-30's unit 0, and KiCad's resave
/// of it, which orders that unit's items by position.
const APPENDED_MANY: &str =
    include_str!("../../tests/fixtures/symbol_graphics_kicad10.appended_many.kicad_sym");
const APPENDED_MANY_RESAVED: &str =
    include_str!("../../tests/fixtures/symbol_graphics_kicad10.appended_many.resaved.kicad_sym");
/// KiCad 7's stock Fiber_Optic library, the tool's edit of it, and KiCad
/// 10.0.8's resave of that edit.
const KICAD7: &str = include_str!("../../tests/fixtures/fiber_optic_kicad7.kicad_sym");
const KICAD7_EDITED: &str =
    include_str!("../../tests/fixtures/fiber_optic_kicad7.graphics.kicad_sym");
const KICAD7_RESAVED: &str =
    include_str!("../../tests/fixtures/fiber_optic_kicad7.graphics.resaved.kicad_sym");

const WIDTH: f64 = 0.254;

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn call(handler: &McpHandler, name: &str, arguments: Value) -> Value {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 867, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .unwrap()
        .result
        .unwrap();
    let mut body: Value =
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
    body["is_error"] = response["isError"].clone();
    body
}

/// The JSON body of a result the handler's own parsing returned.
fn body_of(result: &CallToolResult) -> Value {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap()
        }
        other => panic!("expected text, got {other:?}"),
    }
}

fn library_with(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graphics.kicad_sym");
    std::fs::write(&path, source).unwrap();
    (dir, path)
}

async fn info(handler: &McpHandler, path: &Path, symbol: &str) -> Value {
    let body = call(
        handler,
        "get_symbol_info",
        json!({"library_path": path, "symbol_name": symbol}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    body
}

/// `set_symbol_graphics` on `symbol` with `arguments`, at the revision
/// `get_symbol_info` gives now.
async fn edit(handler: &McpHandler, path: &Path, symbol: &str, arguments: Value) -> Value {
    let revision = info(handler, path, symbol).await["revision"].clone();
    let mut arguments = arguments;
    arguments["library_path"] = json!(path);
    arguments["symbol_name"] = json!(symbol);
    arguments["revision"] = revision;
    call(handler, OPERATION, arguments).await
}

fn rect(start: (f64, f64), end: (f64, f64), fill: &str) -> Value {
    json!({"type": "rect", "start": {"x": start.0, "y": start.1}, "end": {"x": end.0, "y": end.1},
           "stroke_width_mm": WIDTH, "fill": fill})
}

/// Every graphic `symbol` shows, without its index: what KiCad reads,
/// whatever order the file holds them in.
fn items_of(source: &str, symbol: &str) -> Vec<String> {
    let (_, graphics) = listed_graphics(source, symbol);
    let mut items: Vec<String> = graphics
        .unwrap()
        .into_iter()
        .map(|mut graphic| {
            graphic.as_object_mut().unwrap().remove("index");
            graphic.to_string()
        })
        .collect();
    items.sort();
    items
}

/// `get_symbol_info` lists each graphic with its unit, body style and index
/// within them, its item in create_symbol's vocabulary, and the library's
/// revision.
#[tokio::test]
async fn get_symbol_info_lists_each_graphic_with_its_unit_and_index() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let box_info = info(&handler, &path, "BOX").await;
    assert_eq!(
        box_info["revision"],
        DocumentRevision::of(FIXTURE).to_string()
    );
    assert_eq!(
        box_info["graphics"],
        json!([{"unit": 0, "body_style": 1, "index": 0,
                "item": rect((-7.62, 3.81), (7.62, -3.81), "background")}])
    );

    let cui = info(&handler, &path, "CUI_PD-30").await;
    let graphics = cui["graphics"].as_array().unwrap();
    let count = |kind: &str| {
        graphics
            .iter()
            .filter(|g| g["unit"] == 0 && g["item"]["type"] == kind)
            .count()
    };
    assert_eq!(graphics.len(), 17, "{cui}");
    assert_eq!(
        ["rect", "line", "poly", "arc", "circle", "text"].map(count),
        [1, 2, 3, 4, 4, 3],
        "{cui}"
    );
    // Indices count within each unit and body style, in file order.
    let indices: Vec<u64> = graphics
        .iter()
        .map(|g| g["index"].as_u64().unwrap())
        .collect();
    assert_eq!(indices, (0..17).collect::<Vec<u64>>(), "{cui}");
    assert_eq!(
        graphics[0]["item"],
        json!({"type": "line", "start": {"x": -7.62, "y": -5.08}, "end": {"x": -5.1816, "y": -5.08},
               "stroke_width_mm": 0.0})
    );
    let text = graphics
        .iter()
        .find(|g| g["item"]["type"] == "text")
        .unwrap();
    assert!(text["item"]["text"].is_string(), "{text}");
    assert!(text["item"]["font_size_mm"]["height"].is_number(), "{text}");
    let arc = graphics
        .iter()
        .find(|g| g["item"]["type"] == "arc")
        .unwrap();
    assert!(arc["item"]["mid"]["x"].is_number(), "{arc}");

    let antenna = info(&handler, &path, "Antenna_Internal").await;
    let bezier = antenna["graphics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["item"]["type"] == "bezier")
        .unwrap()
        .clone();
    assert_eq!(
        bezier["item"]["points"].as_array().unwrap().len(),
        4,
        "{bezier}"
    );
}

/// What create_symbol's vocabulary cannot say is listed beside the item:
/// the private flag, the stroke type and colour, a `color` fill and its
/// colour, while the item's own fill reads `none`. A text's angle is in
/// degrees.
#[tokio::test]
async fn the_list_names_what_the_vocabulary_cannot() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let note = info(&handler, &path, "NOTE").await;
    assert_eq!(
        note["graphics"],
        json!([
            {"unit": 1, "body_style": 1, "index": 0,
             "item": rect((-6.35, 3.81), (6.35, -3.81), "none"),
             "private": true, "stroke_type": "dash", "stroke_color": [255.0, 0.0, 0.0, 1.0],
             "fill": "color", "fill_color": [0.0, 0.0, 255.0, 0.5]},
            // A symbol text's angle is written in tenths of a degree.
            {"unit": 1, "body_style": 1, "index": 1,
             "item": {"type": "text", "text": "UP", "at": {"x": 5.08, "y": 0.0}, "rotation": 90.0,
                      "font_size_mm": {"height": 1.27, "width": 1.27}}},
            {"unit": 1, "body_style": 1, "index": 2,
             "item": {"type": "text_box", "text": "NOTE", "at": {"x": -5.08, "y": 2.54}, "rotation": 0.0,
                      "size": {"x": 10.16, "y": -5.08}, "stroke_width_mm": WIDTH, "fill": "none"}}
        ])
    );
}

/// A derived symbol has no graphics of its own; it shows its root's.
#[tokio::test]
async fn a_derived_symbol_lists_its_root_s_graphics() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let derived = info(&handler, &path, "BOX_DERIVED").await;
    let root = info(&handler, &path, "BOX").await;
    assert_eq!(derived["graphics"], root["graphics"]);
}

/// #867's case, a body one grid step taller, and a deletion: the file is
/// byte for byte what KiCad 10.0.8 resaves unchanged.
#[tokio::test]
async fn a_replaced_and_a_deleted_graphic_write_what_kicad_resaves() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let taller = rect((-7.62, 5.08), (7.62, -5.08), "background");
    let replaced = edit(
        &handler,
        &path,
        "BOX",
        json!({"unit": 0, "body_style": 1, "mode": "replace", "index": 0, "graphics": [taller]}),
    )
    .await;
    assert_ne!(replaced["is_error"], true, "{replaced}");
    assert_eq!(
        replaced["previous"]["item"],
        rect((-7.62, 3.81), (7.62, -3.81), "background")
    );
    assert_eq!(
        replaced["current"],
        json!({"unit": 0, "body_style": 1, "index": 0, "item": taller})
    );
    assert_eq!(replaced["graphics"], json!([replaced["current"]]));
    assert_eq!(replaced["derived_symbols"], json!(["BOX_DERIVED"]));
    assert_eq!(
        replaced["revision"],
        DocumentRevision::of(&std::fs::read_to_string(&path).unwrap()).to_string()
    );
    assert!(replaced.get("warnings").is_none(), "{replaced}");

    let deleted = edit(
        &handler,
        &path,
        "CUI_PD-30",
        json!({"unit": 0, "body_style": 1, "mode": "delete", "index": 0}),
    )
    .await;
    assert_ne!(deleted["is_error"], true, "{deleted}");
    assert_eq!(deleted["removed"]["item"]["type"], "line", "{deleted}");
    assert_eq!(
        deleted["graphics"].as_array().unwrap().len(),
        16,
        "{deleted}"
    );
    assert_eq!(deleted["graphics"][0]["index"], 0, "{deleted}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EDITED);
}

/// Items appended to a unit with only pins, and to a unit block the symbol
/// does not have yet: KiCad 10.0.8 resaves the file unchanged, the
/// polyline's points laid out on lines as KiCad lays them out.
#[tokio::test]
async fn appended_graphics_write_what_kicad_resaves() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let zigzag: Vec<Value> = (0..11)
        .map(|i| json!({"x": -6.35 + 1.27 * f64::from(i), "y": if i % 2 == 1 { 1.27 } else { -1.27 }}))
        .collect();
    let poly = json!({"type": "poly", "points": zigzag, "stroke_width_mm": WIDTH, "fill": "none"});
    let body = edit(
        &handler,
        &path,
        "BOX",
        json!({"unit": 1, "body_style": 1, "mode": "append", "graphics": [poly]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    assert_eq!(body["added"][0]["index"], 0, "{body}");
    assert_eq!(body["added"][0]["unit"], 1, "{body}");
    let circle = json!({"type": "circle", "center": {"x": 0, "y": 0}, "radius_mm": 1.27,
                        "stroke_width_mm": WIDTH, "fill": "none"});
    let body = edit(
        &handler,
        &path,
        "NOTE",
        json!({"unit": 0, "body_style": 1, "mode": "append", "graphics": [circle]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    assert_eq!(body["added"][0]["unit"], 0, "{body}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), APPENDED);
}

/// KiCad orders a unit's items by position when it saves, so appended items
/// may move on its next save; it reads the items written all the same.
#[tokio::test]
async fn kicad_reads_every_item_appended() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let items = json!([
        {"type": "line", "start": {"x": -7.62, "y": 8.89}, "end": {"x": 7.62, "y": 8.89},
         "stroke_width_mm": WIDTH},
        {"type": "arc", "start": {"x": -2.54, "y": 8.89}, "mid": {"x": 0, "y": 11.43},
         "end": {"x": 2.54, "y": 8.89}, "stroke_width_mm": WIDTH},
        {"type": "circle", "center": {"x": 0, "y": -8.89}, "radius_mm": 0.635, "stroke_width_mm": 0,
         "fill": "outline"},
        rect((-8.89, 8.89), (-7.62, 7.62), "none")
    ]);
    let body = edit(
        &handler,
        &path,
        "CUI_PD-30",
        json!({"unit": 0, "body_style": 1, "mode": "append", "graphics": items}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    // After the unit's last shape, before its texts.
    let added: Vec<u64> = body["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["index"].as_u64().unwrap())
        .collect();
    assert_eq!(added, vec![14, 15, 16, 17], "{body}");
    assert_eq!(body["graphics"][18]["item"]["type"], "text", "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, APPENDED_MANY);
    assert_ne!(APPENDED_MANY_RESAVED, APPENDED_MANY);
    assert_eq!(
        items_of(APPENDED_MANY_RESAVED, "CUI_PD-30"),
        items_of(&written, "CUI_PD-30")
    );
}

/// A KiCad 7 library, indented with spaces, takes items KiCad reads as
/// written.
#[tokio::test]
async fn a_kicad7_library_gets_items_kicad_reads() {
    let handler = handler().await;
    let (_dir, path) = library_with(KICAD7);
    let appended = edit(
        &handler,
        &path,
        "AFBR-1624Z",
        json!({"unit": 0, "body_style": 1, "mode": "append",
               "graphics": [rect((-8.89, 8.89), (8.89, -8.89), "none")]}),
    )
    .await;
    assert_ne!(appended["is_error"], true, "{appended}");
    let line = json!({"type": "line", "start": {"x": 6.35, "y": -2.54}, "end": {"x": 2.54, "y": -6.35},
                      "stroke_width_mm": WIDTH});
    let replaced = edit(
        &handler,
        &path,
        "AFBR-1624Z",
        json!({"unit": 0, "body_style": 1, "mode": "replace", "index": 1, "graphics": [line]}),
    )
    .await;
    assert_ne!(replaced["is_error"], true, "{replaced}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, KICAD7_EDITED);
    for symbol in ["AFBR-1624Z", "AFBR-2624Z"] {
        assert_eq!(
            items_of(KICAD7_RESAVED, symbol),
            items_of(&written, symbol),
            "{symbol}"
        );
    }
}

/// A library saved with CRLF line breaks keeps them in what is written.
#[tokio::test]
async fn a_crlf_library_stays_crlf() {
    let handler = handler().await;
    let crlf = FIXTURE.replace('\n', "\r\n");
    let (_dir, path) = library_with(&crlf);
    let body = edit(
        &handler,
        &path,
        "NOTE",
        json!({"unit": 0, "body_style": 1, "mode": "append",
               "graphics": [rect((-1.27, 1.27), (1.27, -1.27), "none")]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_ne!(written, crlf);
    assert_eq!(
        written.matches('\n').count(),
        written.matches("\r\n").count()
    );
}

/// An index listed at another revision is refused, with nothing written.
#[tokio::test]
async fn a_stale_revision_writes_nothing() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let listed = info(&handler, &path, "CUI_PD-30").await["revision"].clone();
    let first = edit(
        &handler,
        &path,
        "CUI_PD-30",
        json!({"unit": 0, "body_style": 1, "mode": "delete", "index": 0}),
    )
    .await;
    assert_ne!(first["is_error"], true, "{first}");
    let after_first = std::fs::read_to_string(&path).unwrap();
    let body = call(
        &handler,
        OPERATION,
        json!({"library_path": path, "symbol_name": "CUI_PD-30", "revision": listed,
               "unit": 0, "body_style": 1, "mode": "delete", "index": 0}),
    )
    .await;
    assert_eq!(body["is_error"], true, "{body}");
    assert_eq!(body["error"]["kind"], "stale_target", "{body}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);
}

/// What the library cannot take is refused before writing, naming the
/// argument.
#[tokio::test]
async fn refusals_leave_the_library_as_it_was() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let a_rect = rect((-1.27, 1.27), (1.27, -1.27), "none");
    for (symbol, arguments, field, reason) in [
        (
            "BOX_DERIVED",
            json!({"unit": 0, "body_style": 1, "mode": "delete", "index": 0}),
            "symbol_name",
            "edit 'BOX'",
        ),
        (
            "BOX",
            json!({"unit": 1, "body_style": 1, "mode": "delete", "index": 0}),
            "index",
            "has 0 graphic",
        ),
        (
            "BOX",
            json!({"unit": 0, "body_style": 1, "mode": "replace", "index": 1, "graphics": [a_rect]}),
            "index",
            "indices 0 to 0",
        ),
        (
            "BOX",
            json!({"unit": 2, "body_style": 1, "mode": "delete", "index": 0}),
            "unit",
            "no block for unit 2",
        ),
        (
            "BOX",
            json!({"unit": 2, "body_style": 1, "mode": "append", "graphics": [a_rect]}),
            "unit",
            "1 unit(s)",
        ),
        (
            "BOX",
            json!({"unit": 1, "body_style": 2, "mode": "append", "graphics": [a_rect]}),
            "body_style",
            "no body style 2",
        ),
        (
            "BOX",
            json!({"unit": 0, "body_style": 1, "mode": "append",
                   "graphics": [{"type": "ellipse", "stroke_width_mm": 0.254}]}),
            "graphics[0].type",
            "is not one of",
        ),
    ] {
        let body = edit(&handler, &path, symbol, arguments.clone()).await;
        assert_eq!(body["is_error"], true, "{arguments}: {body}");
        assert_eq!(
            body["error"]["kind"], "invalid_argument",
            "{arguments}: {body}"
        );
        assert_eq!(body["error"]["field"], field, "{arguments}: {body}");
        assert!(
            body["error"]["reason"].as_str().unwrap().contains(reason),
            "{arguments}: {body}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            FIXTURE,
            "{arguments}"
        );
    }
    let missing = path.with_file_name("missing.kicad_sym");
    let body = call(
        &handler,
        OPERATION,
        json!({"library_path": missing, "symbol_name": "BOX", "revision": "x", "unit": 0,
               "body_style": 1, "mode": "delete", "index": 0}),
    )
    .await;
    assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
}

/// The handler's own parsing, for what the schema leaves open: each mode
/// takes the arguments it needs and no others.
#[test]
fn the_arguments_fit_the_mode() {
    let a_rect = rect((-1.27, 1.27), (1.27, -1.27), "none");
    let base = json!({"library_path": "x.kicad_sym", "symbol_name": "BOX", "revision": "r",
                      "unit": 0, "body_style": 1});
    for (extra, field, reason) in [
        (
            json!({"mode": "append", "index": 0, "graphics": [a_rect]}),
            "index",
            "takes no index",
        ),
        (json!({"mode": "append"}), "graphics", "one or more"),
        (
            json!({"mode": "append", "graphics": []}),
            "graphics",
            "one or more",
        ),
        (
            json!({"mode": "replace", "graphics": [a_rect]}),
            "index",
            "by its index",
        ),
        (
            json!({"mode": "replace", "index": 0, "graphics": [a_rect, a_rect]}),
            "graphics",
            "exactly one",
        ),
        (
            json!({"mode": "delete", "index": 0, "graphics": [a_rect]}),
            "graphics",
            "takes no items",
        ),
        (
            json!({"mode": "delete", "index": -1}),
            "index",
            "whole number",
        ),
        (
            json!({"mode": "move", "index": 0}),
            "mode",
            "not append, replace or delete",
        ),
        (
            json!({"mode": "append", "graphics": [{"type": "ellipse", "stroke_width_mm": 0.254}]}),
            "graphics",
            "unknown type",
        ),
    ] {
        let mut arguments = base.clone();
        for (key, value) in extra.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        let body = body_of(&parse_request(&arguments).unwrap_err());
        assert_eq!(body["error"]["field"], field, "{extra}: {body}");
        assert!(
            body["error"]["reason"].as_str().unwrap().contains(reason),
            "{extra}: {body}"
        );
    }
}

/// A point a pin could be drawn to that is off the 1.27 mm grid is written,
/// with a warning naming it; an arc's middle point is not such a point.
#[tokio::test]
async fn an_off_grid_point_is_written_with_a_warning() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let items = json!([
        rect((-7.62, 3.81), (7.6, -3.81), "none"),
        // An arc's middle point is not one a pin is drawn to.
        {"type": "arc", "start": {"x": -2.54, "y": 0}, "mid": {"x": 0.1, "y": 2.54},
         "end": {"x": 2.54, "y": 0}, "stroke_width_mm": WIDTH},
        {"type": "line", "start": {"x": -2.54, "y": -5.08}, "end": {"x": 2.5, "y": -5.08},
         "stroke_width_mm": WIDTH},
        {"type": "arc", "start": {"x": -2.5, "y": 0}, "mid": {"x": 0, "y": 2.54},
         "end": {"x": 2.54, "y": 0}, "stroke_width_mm": WIDTH},
        {"type": "circle", "center": {"x": 0.1, "y": 0}, "radius_mm": 1.27,
         "stroke_width_mm": WIDTH, "fill": "none"},
        {"type": "poly", "points": [{"x": -2.54, "y": -2.54}, {"x": 0, "y": -2.6},
                                    {"x": 2.54, "y": -2.54}],
         "stroke_width_mm": WIDTH, "fill": "none"}
    ]);
    let body = edit(
        &handler,
        &path,
        "BOX",
        json!({"unit": 0, "body_style": 1, "mode": "append", "graphics": items}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    assert_eq!(
        body["warnings"],
        json!([
            "graphics[0] (rect) end (7.6, -3.81) is off the 1.27 mm symbol grid",
            "graphics[2] (line) end (2.5, -5.08) is off the 1.27 mm symbol grid",
            "graphics[3] (arc) start (-2.5, 0) is off the 1.27 mm symbol grid",
            "graphics[4] (circle) center (0.1, 0) is off the 1.27 mm symbol grid",
            "graphics[5] (poly) points[1] (0, -2.6) is off the 1.27 mm symbol grid"
        ])
    );
}

/// A listed item can be written back as it is, even when its graphic has a
/// fill the vocabulary cannot write, and the replaced graphic's properties
/// the item cannot carry are named in a warning.
#[tokio::test]
async fn a_replace_names_what_it_did_not_keep() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let listed = info(&handler, &path, "NOTE").await["graphics"][0]["item"].clone();
    let body = edit(
        &handler,
        &path,
        "NOTE",
        json!({"unit": 1, "body_style": 1, "mode": "replace", "index": 0,
               "graphics": [listed.clone()]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    assert_eq!(body["current"]["item"], listed, "{body}");
    let warning = body["warnings"][0].as_str().unwrap();
    for name in [
        "private",
        "stroke_type dash",
        "stroke_color",
        "fill color",
        "fill_color",
    ] {
        assert!(warning.contains(name), "{name}: {warning}");
    }
    assert!(body["current"].get("private").is_none(), "{body}");
}

/// The check before writing and after: every byte outside the edit as it
/// was, and the symbol's graphics as planned.
#[test]
fn the_check_wants_the_graphics_as_planned() {
    let request = Request {
        symbol: "BOX",
        revision: "",
        unit: 0,
        body_style: 1,
        edit: Edit::Replace(0, rect((-7.62, 5.08), (7.62, -5.08), "background")),
    };
    let plan = prepare(Path::new("graphics.kicad_sym"), FIXTURE, &request).unwrap();
    let check = |text: &str| readback_mismatch(text, FIXTURE, "BOX", &plan);
    assert_eq!(check(&plan.replacement), None);
    let unchanged = check(FIXTURE).unwrap();
    assert!(unchanged.contains("reads"), "{unchanged}");
    // A change outside the replaced block.
    let outside = plan
        .replacement
        .replacen("(version 20251024)", "(version 20251025)", 1);
    let reason = check(&outside).unwrap();
    assert!(reason.contains("outside"), "{reason}");
    // A change of the same length after the replaced block, in another
    // symbol, which the listing of BOX's graphics does not show.
    assert!(plan.replacement.find("\"UP\"").unwrap() > plan.span.0 + plan.written_len);
    let after = plan.replacement.replacen("\"UP\"", "\"DN\"", 1);
    let reason = check(&after).unwrap();
    assert!(reason.contains("outside"), "{reason}");
    // The right length, the wrong number.
    let wrong = plan
        .replacement
        .replacen("(start -7.62 5.08)", "(start -7.62 5.18)", 1);
    let reason = check(&wrong).unwrap();
    assert!(reason.contains("reads"), "{reason}");
}

/// The list is in file order, so items appended to a unit block that comes
/// first in the file, out of KiCad's order, are listed first.
#[tokio::test]
async fn items_appended_to_an_earlier_block_are_listed_in_file_order() {
    let handler = handler().await;
    let span = |name: &str| {
        let start = FIXTURE.find(&format!("\t\t(symbol \"{name}\"")).unwrap();
        (
            start,
            start + FIXTURE[start..].find("\n\t\t)\n").unwrap() + "\n\t\t)".len(),
        )
    };
    let (body, pins) = (span("BOX_0_1"), span("BOX_1_1"));
    let swapped = [
        &FIXTURE[..body.0],
        &FIXTURE[pins.0..pins.1],
        &FIXTURE[body.1..pins.0],
        &FIXTURE[body.0..body.1],
        &FIXTURE[pins.1..],
    ]
    .concat();
    let (_dir, path) = library_with(&swapped);
    let body = edit(
        &handler,
        &path,
        "BOX",
        json!({"unit": 1, "body_style": 1, "mode": "append",
               "graphics": [rect((-1.27, 1.27), (1.27, -1.27), "none")]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    let listed = info(&handler, &path, "BOX").await["graphics"].clone();
    let units: Vec<&Value> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|g| &g["unit"])
        .collect();
    assert_eq!(units, [&json!(1), &json!(0)], "{listed}");
}

/// A coordinate finer than KiCad's 0.0001 mm is written as KiCad reads it,
/// at that resolution, and listed so.
#[tokio::test]
async fn a_point_is_written_at_kicad_s_resolution() {
    let handler = handler().await;
    let (_dir, path) = library_with(FIXTURE);
    let body = edit(
        &handler,
        &path,
        "BOX",
        json!({"unit": 0, "body_style": 1, "mode": "append",
               "graphics": [rect((-8.890_04, 6.350_049), (8.89, -6.35), "none")]}),
    )
    .await;
    assert_ne!(body["is_error"], true, "{body}");
    assert_eq!(
        body["added"][0]["item"]["start"],
        json!({"x": -8.89, "y": 6.35})
    );
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("(start -8.89 6.35)"), "{written}");
}
