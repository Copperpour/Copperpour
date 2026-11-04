//! `set_board_page` through served `tools/call` (#257).
//!
//! The file half runs on two KiCad 10.0.8 saves of one board
//! (`tests/fixtures/issue_257_pages.README.md`): at A4 landscape, and after
//! File > Page Settings set A3 portrait. Each write must give the bytes
//! KiCad's own save gave. The gate half runs on KiCad doubles that hold this
//! board, another board or none, and record every command they receive.

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::CallToolResult;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{
    board_document, ctx_talking_to, spawn_kicad_holding_board,
};
use crate::tools::{write_hook, BoardAccess, ServerConfig};
use copperpour_ipc::builders::pack_any;
use copperpour_ipc::gen::kiapi;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const A4: &str = include_str!("../../tests/fixtures/issue_257_page_A4.kicad_pcb");
const A3_PORTRAIT: &str = include_str!("../../tests/fixtures/issue_257_page_A3_portrait.kicad_pcb");

/// The A4 save with its `(paper …)` line removed. Derived, not a KiCad save:
/// KiCad 10 always writes the token, and a board without one is A4 to it.
fn without_paper() -> String {
    let derived = A4.replacen("\t(paper \"A4\")\n", "", 1);
    assert_ne!(derived, A4, "the A4 save holds its paper line");
    derived
}

async fn handler(address: &str) -> McpHandler {
    McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: address.to_string(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds")
}

async fn call_tool(
    handler: &McpHandler,
    tool: &str,
    board: &Path,
    mut arguments: Value,
) -> CallToolResult {
    arguments["board"] = json!(board.to_string_lossy());
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 257, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    assert!(response.error.is_none(), "tool errors are MCP results");
    serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
}

async fn call(handler: &McpHandler, board: &Path, arguments: Value) -> CallToolResult {
    call_tool(handler, "set_board_page", board, arguments).await
}

fn body(result: &CallToolResult) -> Value {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "message": text }))
        }
        other => panic!("expected text content, got {other:?}"),
    }
}

fn board_with(dir: &Path, content: &str) -> PathBuf {
    let board = dir.join("board.kicad_pcb");
    std::fs::write(&board, content).unwrap();
    board
}

fn read(board: &Path) -> String {
    std::fs::read_to_string(board).unwrap()
}

/// A KiCad holding `board` that names it in `GetOpenDocuments`, answers
/// every other command with an empty `AS_OK`, and records each command it
/// receives, `GetOpenDocuments` included.
fn recording_kicad(board: &Path) -> (MockIpcServer, Arc<Mutex<Vec<String>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = received.clone();
    let documents = vec![board_document(&board.to_string_lossy())];
    let server = MockIpcServer::spawn("board-page", move |request| {
        let command = request.message.expect("a command");
        let name = command.type_url.rsplit('.').next().unwrap_or_default();
        log.lock().unwrap().push(name.to_string());
        let message = (name == "GetOpenDocuments").then(|| {
            pack_any(
                &kiapi::common::commands::GetOpenDocumentsResponse {
                    documents: documents.clone(),
                },
                "kiapi.common.commands.GetOpenDocumentsResponse",
            )
        });
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            header: None,
            message,
        }
    });
    (server, received)
}

// ─── File path ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn a4_to_a3_portrait_writes_what_kicad_writes() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let handler = handler("").await;

    let result = call(&handler, &board, json!({ "size": "A3", "portrait": true })).await;

    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(read(&board), A3_PORTRAIT);
    let body = body(&result);
    assert_eq!(body["file"], json!(board.to_string_lossy()));
    assert_eq!(body["paper"], "A3");
    assert_eq!(body["portrait"], true);
    assert_eq!(body["changed"], true);
    assert_eq!(body["source"], "file");
    assert_eq!(body["saved"], true);
    assert_eq!(body["fallback_reason"]["kind"], "transport_unreachable");
    // `paper` and `paper_size_mm` mean what get_board_info means by them:
    // a named size is its own answer, so it reports no dimensions.
    let info = body_of_info(&handler, &board).await;
    assert_eq!(info["source"], "file");
    assert_eq!(body["paper"], info["paper"]);
    assert_eq!(body["paper_size_mm"], info["paper_size_mm"]);
    assert_eq!(body["paper_size_mm"], Value::Null);
}

#[tokio::test]
async fn a3_portrait_to_a4_landscape_writes_what_kicad_writes() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A3_PORTRAIT);

    // `portrait` left out is landscape.
    let result = call(&handler("").await, &board, json!({ "size": "A4" })).await;

    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(read(&board), A4);
    let body = body(&result);
    assert_eq!(
        (&body["paper"], &body["portrait"], &body["changed"]),
        (&json!("A4"), &json!(false), &json!(true))
    );
}

async fn body_of_info(handler: &McpHandler, board: &Path) -> Value {
    let info = call_tool(handler, "get_board_info", board, json!({})).await;
    assert!(!info.is_error, "{}", body(&info));
    body(&info)
}

#[tokio::test]
async fn a_board_without_a_paper_token_gets_one_after_general() {
    let dir = tempfile::tempdir().unwrap();
    let handler = handler("").await;

    let board = board_with(dir.path(), &without_paper());
    let result = call(&handler, &board, json!({ "size": "A4" })).await;
    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(read(&board), A4, "the token goes back where KiCad wrote it");
    assert_eq!(body(&result)["changed"], true);

    let board = board_with(dir.path(), &without_paper());
    let result = call(&handler, &board, json!({ "size": "A3" })).await;
    assert!(!result.is_error, "{}", body(&result));
    let written = read(&board);
    assert!(
        written.contains("\t\t(legacy_teardrops no)\n\t)\n\t(paper \"A3\")\n\t(layers"),
        "the token sits directly after (general …): {written}"
    );
    assert_eq!(
        written,
        A4.replacen("(paper \"A4\")", "(paper \"A3\")", 1),
        "nothing else changes"
    );
    assert_eq!(body(&result)["paper"], "A3");
}

/// A KiCad 5 board names its page `(page …)`; writing `(paper …)` beside it
/// would leave the old node to win in KiCad. Derived from the A4 save.
#[tokio::test]
async fn a_kicad_5_page_node_is_replaced_not_shadowed() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = A4.replacen("\t(paper \"A4\")\n", "\t(page A4)\n", 1);
    assert_ne!(legacy, A4);
    let board = board_with(dir.path(), &legacy);

    let result = call(
        &handler("").await,
        &board,
        json!({ "size": "A3", "portrait": true }),
    )
    .await;

    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(read(&board), A3_PORTRAIT);
}

#[tokio::test]
async fn a_page_the_file_already_holds_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let handler = handler("").await;

    for (saved, arguments, paper, portrait) in [
        (A4, json!({ "size": "A4", "portrait": false }), "A4", false),
        (
            A3_PORTRAIT,
            json!({ "size": "A3", "portrait": true }),
            "A3",
            true,
        ),
    ] {
        let board = board_with(dir.path(), saved);
        let modified = std::fs::metadata(&board).unwrap().modified().unwrap();

        let result = call(&handler, &board, arguments.clone()).await;

        assert!(!result.is_error, "{arguments}: {}", body(&result));
        let body = body(&result);
        assert_eq!(body["changed"], false, "{arguments}: {body}");
        assert_eq!(body["paper"], paper);
        assert_eq!(body["portrait"], portrait);
        assert_eq!(body["paper_size_mm"], Value::Null);
        assert_eq!(read(&board), saved);
        assert_eq!(
            std::fs::metadata(&board).unwrap().modified().unwrap(),
            modified,
            "{arguments}: the file was rewritten"
        );
    }
}

/// The response is the read-back, never the request: a save landing between
/// the write and the read-back makes the outcome uncertain, not a success.
#[tokio::test]
async fn a_save_between_the_write_and_the_read_back_is_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    write_hook::set(A4.to_string());

    let result = call(
        &handler("").await,
        &board,
        json!({ "size": "A3", "portrait": true }),
    )
    .await;

    assert!(!write_hook::armed(), "the save landed");
    assert!(result.is_error, "{}", body(&result));
    let body = body(&result);
    assert_eq!(
        body["error"]["kind"], "mutation_outcome_uncertain",
        "{body}"
    );
    let reason = body["error"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("A4 landscape") && reason.contains("A3 portrait"),
        "{reason}"
    );
    assert_eq!(read(&board), A4);
}

// ─── The live-board gate ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_board_kicad_holds_is_refused_and_nothing_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let (kicad, received) = recording_kicad(&board);

    let result = call(
        &handler(kicad.address()).await,
        &board,
        json!({ "size": "A3", "portrait": true }),
    )
    .await;

    assert!(result.is_error, "{}", body(&result));
    let body = body(&result);
    assert_eq!(body["error"]["kind"], "unsupported_capability", "{body}");
    assert_eq!(body["error"]["capability"], "set_board_page");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("File > Page Settings") && message.contains("close the board"),
        "{message}"
    );
    assert_eq!(read(&board), A4);
    // Identifying the board is all that reached KiCad.
    assert_eq!(*received.lock().unwrap(), ["GetOpenDocuments"]);
}

/// KiCad running with no PCB editor answers `AS_UNHANDLED`. That is the
/// gate's own refusal, passed through, not the page-settings one.
#[tokio::test]
async fn a_kicad_with_no_pcb_editor_gets_the_gate_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let kicad = MockIpcServer::spawn("board-page-no-editor", |request| {
        assert!(request.message.is_some(), "a command");
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsUnhandled as i32,
                error_message: "no handler available for request of type \
                                kiapi.common.commands.GetOpenDocuments"
                    .to_string(),
            }),
            header: None,
            message: None,
        }
    });

    let result = call(
        &handler(kicad.address()).await,
        &board,
        json!({ "size": "A3" }),
    )
    .await;

    assert!(result.is_error, "{}", body(&result));
    let body = body(&result);
    assert_ne!(body["error"]["kind"], "unsupported_capability", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("page settings change")
            && message.contains("The board file was not modified"),
        "{message}"
    );
    assert_eq!(read(&board), A4);
}

#[tokio::test]
async fn a_board_seen_live_and_then_lost_stays_refused() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let kicad = spawn_kicad_holding_board(&board, |_| None);
    let handler = handler(kicad.address()).await;

    let held = call(&handler, &board, json!({ "size": "A3" })).await;
    assert_eq!(body(&held)["error"]["kind"], "unsupported_capability");
    drop(kicad);

    let result = call(&handler, &board, json!({ "size": "A3" })).await;

    assert!(result.is_error, "{}", body(&result));
    let body = body(&result);
    assert_eq!(body["error"]["kind"], "unsafe_file_fallback", "{body}");
    assert_eq!(body["error"]["reason"], "board_previously_observed_live");
    assert_eq!(read(&board), A4);
}

#[tokio::test]
async fn a_kicad_holding_another_board_lets_the_file_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let other = dir.path().join("other.kicad_pcb");
    std::fs::write(&other, "").unwrap();
    let kicad = spawn_kicad_holding_board(&other, |_| None);

    let result = call(
        &handler(kicad.address()).await,
        &board,
        json!({ "size": "A3", "portrait": true }),
    )
    .await;

    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(read(&board), A3_PORTRAIT);
    assert_eq!(body(&result)["fallback_reason"]["kind"], "board_not_open");
}

// ─── Arguments ───────────────────────────────────────────────────────────────

/// Each malformed call and the field its refusal must name.
fn malformed() -> [(Value, &'static str); 5] {
    [
        (json!({}), "size"),
        (json!({ "size": "A9" }), "size"),
        (json!({ "size": "User" }), "size"),
        (json!({ "size": 3 }), "size"),
        (json!({ "size": "A3", "portrait": "yes" }), "portrait"),
    ]
}

/// Refused before KiCad is asked anything or the file is read, through the
/// served schema and through the handler's own checks alike.
#[tokio::test]
async fn malformed_arguments_are_refused_before_kicad_or_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let board = board_with(dir.path(), A4);
    let (kicad, received) = recording_kicad(&board);
    let served = handler(kicad.address()).await;
    let set_board_page = crate::tools::pcb_board::tools()
        .into_iter()
        .find(|tool| tool.name == "set_board_page")
        .expect("set_board_page is registered");
    let ctx = Arc::new(ctx_talking_to(kicad.address().to_string()));

    for (arguments, field) in malformed() {
        let mut direct_arguments = arguments.clone();
        direct_arguments["board"] = json!(board.to_string_lossy());
        for (path, result) in [
            ("served", call(&served, &board, arguments.clone()).await),
            (
                "direct",
                (set_board_page.handler)(&direct_arguments, ctx.clone())
                    .await
                    .unwrap(),
            ),
        ] {
            assert!(result.is_error, "{path} {arguments} was accepted");
            let body = body(&result);
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{path} {arguments}: {body}"
            );
            let named = body["error"]["field"].as_str().unwrap_or_default();
            assert!(named.contains(field), "{path} {arguments} named {named}");
        }
    }
    assert!(received.lock().unwrap().is_empty(), "KiCad was asked");
    assert_eq!(read(&board), A4);
}

#[test]
fn offers_set_schematic_page_sizes_and_needs_a_closed_board() {
    let size_enum = |tool: crate::tools::ToolDef| tool.input_schema["properties"]["size"].clone();
    let board = crate::tools::pcb_board::tools()
        .into_iter()
        .find(|tool| tool.name == "set_board_page")
        .expect("set_board_page is registered");
    let schematic = crate::tools::sch_hierarchy::tools()
        .into_iter()
        .find(|tool| tool.name == "set_schematic_page")
        .expect("set_schematic_page is registered");
    assert_eq!(board.board_access, BoardAccess::ClosedBoardOnly);
    assert_eq!(size_enum(board)["enum"], size_enum(schematic)["enum"]);
}
