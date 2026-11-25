//! `board_source` contract for `get_layer_list` and `get_netclasses` (#542), and
//! the refusal live PCB writes give in the same KiCad states (#83).
//!
//! Driven through served `tools/call` rather than the handlers, so the
//! advertised schema, the dispatch gate and the response body are all under
//! test, not just the function bodies.
//!
//! Every case runs against `board_source_divergence_kicad10.kicad_pcb` and a
//! KiCad double whose answers share no layer name, copper count or net name
//! with it. A test that reads the wrong adapter therefore fails on a value,
//! not on a coincidence — see the fixture's README for both sides of the
//! divergence and the `kicad-cli` oracles for the saved half.

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::CallToolResult;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{board_document, spawn_kicad_holding_board};
use crate::tools::ServerConfig;
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
use prost::Message;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// ─── What the two sides say ──────────────────────────────────────────────────
//
// Restated here as literals rather than imported from the implementation or
// the fixture: these are the promise the tests hold the code to.

/// The saved stackup, from `kicad-cli pcb export gerbers` on the fixture (one
/// plot per enabled layer, named by the name KiCad shows).
const SAVED_COPPER_LAYER_COUNT: u64 = 4;
const SAVED_ONLY_LAYERS: [&str; 2] = ["In1.Cu", "In2.Cu"];
/// `F.Fab`'s user rename, which the gerber export writes as its filename.
const SAVED_FAB_DISPLAY_NAME: &str = "SavedOnlyFabName";
/// The saved nets, from `kicad-cli pcb export ipcd356` on the fixture.
const SAVED_NETS: [&str; 2] = ["SAVED_ONLY_A", "SAVED_ONLY_B"];

/// What the KiCad double answers instead. Nothing here appears in the file.
const LIVE_COPPER_LAYER_COUNT: u64 = 2;
const LIVE_LAYERS: [&str; 3] = ["F.Cu", "B.Cu", "Edge.Cuts"];
const LIVE_TOP_DISPLAY_NAME: &str = "LiveRenamedTop";
const LIVE_NETS: [&str; 2] = ["LIVE_ONLY_P", "LIVE_ONLY_Q"];

/// Saved vs. live geometry on `board_source_divergence_kicad10.kicad_pcb`:
/// the saved file has R1 at (100, 50) (pad 1 at 99.5, 50 on `SAVED_ONLY_A`),
/// R2 at (110, 50) (anchor distance 10.0 mm, 2 footprints), while the live
/// KiCad double reports R1 at (85, 45) (pad 1 at 84.5, 45 on `LIVE_ONLY_P`),
/// R2 at (109, 45) (anchor distance 24.0 mm), and an extra live footprint R3
/// at (120, 55) (3 footprints total).
const SAVED_R1_PAD1_X: f64 = 99.5;
const LIVE_R1_PAD1_X: f64 = 84.5;
const SAVED_ANCHOR_CLEARANCE_MM: f64 = 10.0;
const LIVE_ANCHOR_CLEARANCE_MM: f64 = 24.0;
const SAVED_FOOTPRINT_COUNT: u64 = 2;
const LIVE_FOOTPRINT_COUNT: u64 = 3;

const LIVE_DOCUMENT_SNAPSHOT: &str = "(kicad_pcb\n\
\t(version 20260206)\n\
\t(generator \"pcbnew\")\n\
\t(generator_version \"10.0\")\n\
\t(layers\n\
\t\t(0 \"F.Cu\" signal)\n\
\t\t(2 \"B.Cu\" signal)\n\
\t\t(25 \"Edge.Cuts\" user)\n\
\t\t(31 \"F.CrtYd\" user \"F.Courtyard\")\n\
\t)\n\
\t(gr_rect (start 80 30) (end 130 70) (stroke (width 0.05) (type default)) (fill none) (layer \"Edge.Cuts\"))\n\
\t(footprint \"Resistor_SMD:R_0402\" (layer \"F.Cu\") (at 85 45)\n\
\t\t(property \"Reference\" \"R1\" (at 0 -1.5 0) (layer \"F.SilkS\"))\n\
\t\t(property \"Value\" \"10k\" (at 0 1.5 0) (layer \"F.Fab\"))\n\
\t\t(fp_rect (start -1 -0.6) (end 1 0.6) (stroke (width 0.05) (type default)) (fill none) (layer \"F.CrtYd\"))\n\
\t\t(pad \"1\" smd rect (at -0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_P\"))\n\
\t\t(pad \"2\" smd rect (at 0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_Q\"))\n\
\t)\n\
\t(footprint \"Resistor_SMD:R_0402\" (layer \"F.Cu\") (at 109 45)\n\
\t\t(property \"Reference\" \"R2\" (at 0 -1.5 0) (layer \"F.SilkS\"))\n\
\t\t(property \"Value\" \"4.7k\" (at 0 1.5 0) (layer \"F.Fab\"))\n\
\t\t(fp_rect (start -1 -0.6) (end 1 0.6) (stroke (width 0.05) (type default)) (fill none) (layer \"F.CrtYd\"))\n\
\t\t(pad \"1\" smd rect (at -0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_P\"))\n\
\t\t(pad \"2\" smd rect (at 0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_Q\"))\n\
\t)\n\
\t(footprint \"Resistor_SMD:R_0402\" (layer \"F.Cu\") (at 120 55)\n\
\t\t(property \"Reference\" \"R3\" (at 0 -1.5 0) (layer \"F.SilkS\"))\n\
\t\t(property \"Value\" \"1k\" (at 0 1.5 0) (layer \"F.Fab\"))\n\
\t\t(fp_rect (start -1 -0.6) (end 1 0.6) (stroke (width 0.05) (type default)) (fill none) (layer \"F.CrtYd\"))\n\
\t\t(pad \"1\" smd rect (at -0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_Q\"))\n\
\t\t(pad \"2\" smd rect (at 0.5 0) (size 0.6 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\") (net \"LIVE_ONLY_P\"))\n\
\t)\n\
)\n";

/// The layers the double enables, as KiCad's own `BoardLayer` enum rather
/// than hand-copied numbers — `board_types.proto` is vendored from KiCad and
/// resynced on upgrades, and a constant that silently stopped meaning `F.Cu`
/// would make the double answer about a different layer.
fn live_layer_ids() -> [i32; 3] {
    [
        BoardLayer::BlFCu as i32,
        BoardLayer::BlBCu as i32,
        BoardLayer::BlEdgeCuts as i32,
    ]
}

// ─── Fixture and server plumbing ─────────────────────────────────────────────

fn fixture_board(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/board_source_divergence_kicad10.kicad_pcb");
    let board = dir.join("board_source_divergence_kicad10.kicad_pcb");
    std::fs::copy(&source, &board).expect("the fixture is committed beside the tests");
    board
}

/// The project file `get_netclasses` reads its definitions and patterns from.
///
/// One class with two exact-name patterns, one naming a live-only net and one
/// naming a saved-only net, so `matched_nets` alone says which net list the
/// answer was derived from — and `unmatched_patterns` names the other one.
fn write_project(board: &Path) {
    let pro = board.with_extension("kicad_pro");
    let settings = json!({
        "board": { "design_settings": { "rules": {} } },
        "net_settings": {
            "classes": [
                {
                    "name": "Default",
                    "priority": 2147483647,
                    "clearance": 0.2,
                    "track_width": 0.2,
                    "via_diameter": 0.6,
                    "via_drill": 0.3,
                    "microvia_diameter": 0.3,
                    "microvia_drill": 0.1,
                    "diff_pair_width": 0.2,
                    "diff_pair_gap": 0.25,
                    "diff_pair_via_gap": 0.25,
                    "wire_width": 6,
                    "bus_width": 12,
                    "line_style": 0,
                    "pcb_color": "rgba(0, 0, 0, 0.000)",
                    "schematic_color": "rgba(0, 0, 0, 0.000)"
                },
                { "name": "Rails", "priority": 1, "clearance": 0.5 }
            ],
            "meta": { "version": 4 },
            "netclass_patterns": [
                { "netclass": "Rails", "pattern": LIVE_NETS[0] },
                { "netclass": "Rails", "pattern": SAVED_NETS[0] }
            ]
        }
    });
    std::fs::write(&pro, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
}

async fn handler_talking_to(address: &str) -> McpHandler {
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

/// A served Copperpour, the fixture board, and whatever KiCad the case needs.
///
/// The temp dir is held for the scene's lifetime, so the board outlives every
/// call made against it.
struct Scene {
    _dir: tempfile::TempDir,
    board: PathBuf,
    kicad: Option<MockIpcServer>,
    handler: McpHandler,
}

impl Scene {
    /// KiCad holds the fixture and answers with the live half.
    async fn live() -> Self {
        Self::build(|board| Some(spawn_live_kicad(board))).await
    }

    /// KiCad holds the fixture and then declines to answer about it.
    async fn rejecting() -> Self {
        Self::build(|board| Some(spawn_kicad_rejecting_after_identification(board))).await
    }

    /// KiCad holds some other project, which is not this board's live source.
    async fn holding_another_board() -> Self {
        Self::build(|board| {
            let elsewhere = board.with_file_name("somebody_elses.kicad_pcb");
            std::fs::write(&elsewhere, "").unwrap();
            Some(spawn_live_kicad(&elsewhere))
        })
        .await
    }

    /// No KiCad is reachable at all.
    async fn offline() -> Self {
        Self::build(|_| None).await
    }

    /// KiCad is up with no PCB editor open.
    async fn without_a_board_editor() -> Self {
        Self::build(|_| Some(spawn_kicad_without_a_board_editor())).await
    }

    async fn not_implementing_open_documents() -> Self {
        Self::build(|_| Some(spawn_kicad_not_implementing_open_documents())).await
    }

    /// KiCad is reachable and refuses the open-document list with `status`,
    /// so no board is named and nothing says whether one is open.
    async fn refusing_before_identification(status: kiapi::common::ApiStatusCode) -> Self {
        Self::build(move |_| Some(spawn_kicad_refusing_everything_with(status))).await
    }

    async fn build(kicad: impl FnOnce(&Path) -> Option<MockIpcServer>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = fixture_board(dir.path());
        write_project(&board);
        let kicad = kicad(&board);
        let handler =
            handler_talking_to(kicad.as_ref().map_or("", |server| server.address())).await;
        Self {
            _dir: dir,
            board,
            kicad,
            handler,
        }
    }

    /// The endpoint this scene's server is configured to talk to. Fixed when
    /// the server was built, so a replacement KiCad has to claim this name.
    fn endpoint(&self) -> String {
        self.kicad
            .as_ref()
            .expect("a KiCad to take the endpoint from")
            .address()
            .to_string()
    }

    /// The editor goes away mid-session, after this process watched it hold
    /// the board. Dropping the guard releases its endpoint.
    fn lose_kicad(&mut self) {
        self.kicad = None;
    }

    /// Call one tool against this scene's board the way a client does: a
    /// `tools/call` JSON-RPC request.
    async fn call(&self, tool: &str, board_source: Option<&str>) -> CallToolResult {
        let mut arguments = json!({ "board": self.board.to_string_lossy() });
        if let Some(mode) = board_source {
            arguments["board_source"] = json!(mode);
        }
        self.call_with(tool, arguments).await
    }

    async fn call_with(&self, tool: &str, arguments: Value) -> CallToolResult {
        let response = self
            .handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result"))
            .expect("result uses the advertised MCP shape")
    }

    /// The parsed body of a call that must succeed.
    async fn body(&self, tool: &str, board_source: Option<&str>) -> Value {
        let result = self.call(tool, board_source).await;
        assert!(!result.is_error, "{:?}", body_of(&result));
        body_of(&result)
    }
}

fn body_of(result: &CallToolResult) -> Value {
    let text = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn kind_of(result: &CallToolResult) -> Option<String> {
    crate::mcp::error::extract_error_kind(result)
}

/// A KiCad holding `open` open and answering the layer and net queries with
/// the live half of the divergence.
fn spawn_live_kicad(open: &Path) -> MockIpcServer {
    spawn_kicad_holding_board(open, live_answer)
}

/// The same KiCad, except it declines the commands `skip` names — an `AS_OK`
/// with no body, which is how KiCad answers a command it will not serve.
fn spawn_live_kicad_skipping(open: &Path, skip: &'static str) -> MockIpcServer {
    spawn_kicad_holding_board(open, move |command| {
        if command.type_url.ends_with(skip) {
            return None;
        }
        live_answer(command)
    })
}

/// KiCad running with no PCB editor: the project manager answers, and refuses
/// `GetOpenDocuments` itself with `AS_UNHANDLED`. This is the state a user is
/// in between launching KiCad and opening a board.
fn spawn_kicad_without_a_board_editor() -> MockIpcServer {
    MockIpcServer::spawn("no-board-editor", no_board_editor_response)
}

/// A KiCad whose build does not implement the open-document command at all.
fn spawn_kicad_not_implementing_open_documents() -> MockIpcServer {
    MockIpcServer::spawn("open-documents-unimplemented", |request| {
        assert!(request.message.is_some(), "a command");
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsUnimplemented as i32,
                error_message: "kiapi.common.commands.GetOpenDocuments is not implemented"
                    .to_string(),
            }),
            header: None,
            message: None,
        }
    })
}

/// A KiCad that answers every command, `GetOpenDocuments` included, with
/// `status`: reachable, and refusing before it has named any board.
fn spawn_kicad_refusing_everything_with(status: kiapi::common::ApiStatusCode) -> MockIpcServer {
    MockIpcServer::spawn("refusing-before-identification", move |request| {
        assert!(request.message.is_some(), "a command");
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: status as i32,
                error_message: format!("the double answers {}", status.as_str_name()),
            }),
            header: None,
            message: None,
        }
    })
}

/// The same KiCad, taking over an endpoint a previous one has released.
fn spawn_kicad_without_a_board_editor_at(address: String) -> MockIpcServer {
    MockIpcServer::spawn_at(address, no_board_editor_response)
}

/// How KiCad answers a board command with no PCB editor behind the endpoint:
/// a completed round trip carrying `AS_UNHANDLED`, not a transport failure.
fn no_board_editor_response(request: kiapi::common::ApiRequest) -> kiapi::common::ApiResponse {
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
}

/// The same KiCad, refusing everything except the open-document list.
///
/// `spawn_kicad_holding_board` always answers `AS_OK`, so the one case that
/// needs a status is its own server: a board KiCad has positively identified
/// and then declines to answer about.
fn spawn_kicad_rejecting_after_identification(open: &Path) -> MockIpcServer {
    let documents = vec![board_document(&open.to_string_lossy())];
    MockIpcServer::spawn("board-source-busy", move |request| {
        let command = request.message.expect("a command");
        if command.type_url.ends_with("GetOpenDocuments") {
            return kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                header: None,
                message: Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: documents.clone(),
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                )),
            };
        }
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsBusy as i32,
                error_message: "the board is busy".to_string(),
            }),
            header: None,
            message: None,
        }
    })
}

fn live_answer(command: &prost_types::Any) -> Option<prost_types::Any> {
    if command.type_url.ends_with("GetBoardEnabledLayers") {
        return Some(copperpour_ipc::builders::pack_any(
            &kiapi::board::commands::BoardEnabledLayersResponse {
                copper_layer_count: LIVE_COPPER_LAYER_COUNT as u32,
                layers: live_layer_ids().to_vec(),
            },
            "kiapi.board.commands.BoardEnabledLayersResponse",
        ));
    }
    if command.type_url.ends_with("GetBoardLayerName") {
        let asked = kiapi::board::commands::GetBoardLayerName::decode(&command.value[..])
            .expect("a layer-name request");
        // Only the top copper layer is renamed, so a response that ignores the
        // requested layer cannot produce the expected answer.
        let [front, back, edge] = live_layer_ids();
        let name = match asked.layer {
            layer if layer == front => LIVE_TOP_DISPLAY_NAME,
            layer if layer == back => "B.Cu",
            layer if layer == edge => "Edge.Cuts",
            other => panic!("the double was asked for an unexpected layer {other}"),
        };
        return Some(copperpour_ipc::builders::pack_any(
            &kiapi::board::commands::BoardLayerNameResponse {
                name: name.to_string(),
            },
            "kiapi.board.commands.BoardLayerNameResponse",
        ));
    }
    if command.type_url.ends_with("GetNets") {
        return Some(copperpour_ipc::builders::pack_any(
            &kiapi::board::commands::NetsResponse {
                nets: LIVE_NETS
                    .iter()
                    .enumerate()
                    .map(|(index, name)| kiapi::board::types::Net {
                        code: Some(kiapi::board::types::NetCode {
                            value: index as i32 + 1,
                        }),
                        name: (*name).to_string(),
                    })
                    // KiCad also reports the unconnected pseudo-net, whose name
                    // is empty. It is not a net a user has.
                    .chain(std::iter::once(kiapi::board::types::Net {
                        code: Some(kiapi::board::types::NetCode { value: 0 }),
                        name: String::new(),
                    }))
                    .collect(),
            },
            "kiapi.board.commands.NetsResponse",
        ));
    }
    if command.type_url.ends_with("SaveDocumentToString") {
        let asked = kiapi::common::commands::SaveDocumentToString::decode(&command.value[..])
            .expect("SaveDocumentToString request");
        assert!(
            asked.document.is_some(),
            "SaveDocumentToString must name the bound document"
        );
        return Some(copperpour_ipc::builders::pack_any(
            &kiapi::common::commands::SavedDocumentResponse {
                document: asked.document,
                contents: LIVE_DOCUMENT_SNAPSHOT.to_string(),
            },
            "kiapi.common.commands.SavedDocumentResponse",
        ));
    }
    if command.type_url.ends_with("GetItems") {
        let asked = kiapi::common::commands::GetItems::decode(&command.value[..])
            .expect("GetItems request");
        assert!(
            asked
                .header
                .as_ref()
                .and_then(|h| h.document.as_ref())
                .is_some(),
            "GetItems must name the bound document"
        );
        let items = if asked
            .types
            .contains(&(kiapi::common::types::KiCadObjectType::KotPcbFootprint as i32))
        {
            vec![live_r1_footprint()]
        } else {
            Vec::new()
        };
        return Some(copperpour_ipc::builders::pack_any(
            &kiapi::common::commands::GetItemsResponse {
                header: asked.header,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                items,
            },
            "kiapi.common.commands.GetItemsResponse",
        ));
    }
    None
}

fn live_r1_footprint() -> prost_types::Any {
    let pad = |number: &str, x: f64, y: f64, net: &str, uuid: &str| {
        copperpour_ipc::builders::pack_any(
            &kiapi::board::types::Pad {
                id: Some(kiapi::common::types::Kiid {
                    value: uuid.to_string(),
                }),
                number: number.to_string(),
                r#type: kiapi::board::types::PadType::PtSmd as i32,
                position: Some(copperpour_ipc::builders::vec2(x, y)),
                net: Some(kiapi::board::types::Net {
                    code: None,
                    name: net.to_string(),
                }),
                pad_stack: Some(kiapi::board::types::PadStack {
                    layers: vec![
                        BoardLayer::BlFCu as i32,
                        BoardLayer::BlFMask as i32,
                        BoardLayer::BlFPaste as i32,
                    ],
                    copper_layers: vec![kiapi::board::types::PadStackLayer {
                        layer: BoardLayer::BlFCu as i32,
                        shape: kiapi::board::types::PadStackShape::PssRectangle as i32,
                        size: Some(copperpour_ipc::builders::vec2(0.6, 0.5)),
                        ..Default::default()
                    }],
                    angle: Some(kiapi::common::types::Angle { value_degrees: 0.0 }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.Pad",
        )
    };
    copperpour_ipc::builders::pack_any(
        &kiapi::board::types::FootprintInstance {
            position: Some(copperpour_ipc::builders::vec2(85.0, 45.0)),
            reference_field: Some(kiapi::board::types::Field {
                name: "Reference".to_string(),
                text: Some(kiapi::board::types::BoardText {
                    text: Some(kiapi::common::types::Text {
                        text: "R1".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            definition: Some(kiapi::board::types::Footprint {
                items: vec![
                    pad(
                        "1",
                        LIVE_R1_PAD1_X,
                        45.0,
                        LIVE_NETS[0],
                        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
                    ),
                    pad(
                        "2",
                        85.5,
                        45.0,
                        LIVE_NETS[1],
                        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
        "kiapi.board.types.FootprintInstance",
    )
}

fn layer_names(body: &Value) -> Vec<String> {
    body["layers"]
        .as_array()
        .expect("a layers array")
        .iter()
        .map(|layer| layer["name"].as_str().expect("a name").to_string())
        .collect()
}

/// The one netclass the fixture project defines beside `Default`.
fn rails_class(body: &Value) -> &Value {
    body["netclasses"]
        .as_array()
        .expect("netclasses")
        .iter()
        .find(|class| class["name"] == json!("Rails"))
        .unwrap_or_else(|| panic!("no Rails class in {body}"))
}

fn display_name_of(body: &Value, layer: &str) -> String {
    body["layers"]
        .as_array()
        .expect("a layers array")
        .iter()
        .find(|entry| entry["name"] == json!(layer))
        .unwrap_or_else(|| panic!("no layer '{layer}' in {body}"))["display_name"]
        .as_str()
        .expect("a display name")
        .to_string()
}

// ─── get_layer_list ──────────────────────────────────────────────────────────

/// The defect #542 names: a board open with an unsaved stackup change was
/// answered from its file. Under `auto` the live board wins outright.
#[tokio::test]
async fn auto_answers_the_live_stackup_over_a_divergent_saved_one() {
    let scene = Scene::live().await;

    let body = scene.body("get_layer_list", None).await;

    assert_eq!(layer_names(&body), LIVE_LAYERS, "{body}");
    assert_eq!(
        body["copper_layer_count"],
        json!(LIVE_COPPER_LAYER_COUNT),
        "the saved file declares {SAVED_COPPER_LAYER_COUNT}: {body}"
    );
    assert_eq!(display_name_of(&body, "F.Cu"), LIVE_TOP_DISPLAY_NAME);
    let rendered = body.to_string();
    for saved_only in SAVED_ONLY_LAYERS {
        assert!(
            !rendered.contains(saved_only),
            "a live answer must not carry the saved-only layer {saved_only}: {body}"
        );
    }
    assert!(!rendered.contains(SAVED_FAB_DISPLAY_NAME), "{body}");

    assert_eq!(body["sources"]["enabled_layers"], json!("ipc"), "{body}");
    assert_eq!(body["sources"]["layer_names"], json!("ipc"), "{body}");
    assert_eq!(
        body["sources"]["copper_layer_count"],
        json!("ipc"),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["board_state"],
        json!("ipc"),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["excludes_unsaved_editor_state"],
        json!(false),
        "{body}"
    );
}

/// The half of a live answer KiCad's IPC API cannot supply is reported as
/// file-backed instead of being passed off as live or dropped.
#[tokio::test]
async fn a_live_answer_reports_its_file_backed_fields_as_file_backed() {
    let scene = Scene::live().await;

    let body = scene.body("get_layer_list", None).await;

    assert_eq!(
        body["sources"]["layer_types"],
        json!("saved_board"),
        "{body}"
    );
    assert_eq!(body["sources"]["layer_ids"], json!("saved_board"), "{body}");
    // Joined on the canonical name from the fixture's own `(layers …)` table.
    let layers = body["layers"].as_array().unwrap();
    let edge = layers
        .iter()
        .find(|l| l["name"] == json!("Edge.Cuts"))
        .unwrap();
    assert_eq!(edge["type"], json!("user"), "{body}");
    assert_eq!(edge["id"], json!(25), "{body}");
    let front = layers.iter().find(|l| l["name"] == json!("F.Cu")).unwrap();
    assert_eq!(front["type"], json!("signal"), "{body}");
    assert_eq!(front["id"], json!(0), "{body}");
}

/// `saved` is an instruction, not a fallback: KiCad is right there holding the
/// board and is still not consulted, and the answer says what it excludes.
#[tokio::test]
async fn saved_inspects_the_file_even_while_kicad_holds_the_board() {
    let scene = Scene::live().await;

    let body = scene.body("get_layer_list", Some("saved")).await;

    assert_eq!(
        body["copper_layer_count"],
        json!(SAVED_COPPER_LAYER_COUNT),
        "{body}"
    );
    let names = layer_names(&body);
    for saved_only in SAVED_ONLY_LAYERS {
        assert!(names.iter().any(|name| name == saved_only), "{body}");
    }
    assert_eq!(display_name_of(&body, "F.Fab"), SAVED_FAB_DISPLAY_NAME);
    assert!(
        !body.to_string().contains(LIVE_TOP_DISPLAY_NAME),
        "the live editor must not have been consulted: {body}"
    );

    assert_eq!(
        body["sources"]["enabled_layers"],
        json!("saved_board"),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["reason"],
        json!("explicitly_requested"),
        "{body}"
    );
}

/// A reachable KiCad holding some other board is not this board's live source.
/// `live` says so, naming the boards KiCad does hold; `auto` reads the file and
/// explains why it was allowed to.
#[tokio::test]
async fn a_kicad_holding_another_board_refuses_live_and_explains_auto() {
    let scene = Scene::holding_another_board().await;

    let refused = scene.call("get_layer_list", Some("live")).await;
    assert!(refused.is_error);
    assert_eq!(kind_of(&refused).as_deref(), Some("wrong_document"));
    let refusal = body_of(&refused).to_string();
    for saved_only in SAVED_ONLY_LAYERS {
        assert!(
            !refusal.contains(saved_only),
            "a refusal must not leak the saved stackup: {refusal}"
        );
    }

    let body = scene.body("get_layer_list", None).await;
    assert_eq!(
        body["copper_layer_count"],
        json!(SAVED_COPPER_LAYER_COUNT),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["reason"],
        json!("board_not_open_in_kicad"),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{body}"
    );
}

/// `live` with no KiCad at all is a refusal, not a quiet file read.
#[tokio::test]
async fn live_refuses_when_no_kicad_is_reachable() {
    let scene = Scene::offline().await;

    let result = scene.call("get_layer_list", Some("live")).await;

    assert!(result.is_error);
    assert_eq!(kind_of(&result).as_deref(), Some("editor_unavailable"));
    let refusal = body_of(&result);
    assert!(
        !refusal.to_string().contains(SAVED_ONLY_LAYERS[0]),
        "{refusal}"
    );
}

/// The non-negotiable rule: once KiCad has identified the board as live, a
/// failed query is a failure. It never becomes a saved-file success.
#[tokio::test]
async fn a_failure_after_identification_is_not_answered_from_the_file() {
    let scene = Scene::rejecting().await;

    for tool in ["get_layer_list", "get_board_info"] {
        for mode in [Some("auto"), Some("live")] {
            let result = scene.call(tool, mode).await;
            assert!(result.is_error, "{mode:?}: {:?}", body_of(&result));
            assert_eq!(
                kind_of(&result).as_deref(),
                Some("editor_unavailable"),
                "{mode:?}"
            );
            let refusal = body_of(&result).to_string();
            assert!(
                !refusal.contains(SAVED_ONLY_LAYERS[0])
                    && !refusal.contains(SAVED_FAB_DISPLAY_NAME),
                "{mode:?}: the saved stackup must not be reported as this board's state: {refusal}"
            );
        }
    }
}

/// A board this server watched KiCad hold, whose editor then vanished: the
/// saved file may be older than state that was lost, so `auto` refuses. An
/// explicit `saved` request may still inspect it, and must say so.
#[tokio::test]
async fn a_board_observed_live_then_lost_refuses_auto_and_discloses_under_saved() {
    let mut scene = Scene::live().await;
    scene.body("get_layer_list", None).await;

    scene.lose_kicad();

    let refused = scene.call("get_layer_list", None).await;
    assert!(refused.is_error, "{:?}", body_of(&refused));
    assert_eq!(kind_of(&refused).as_deref(), Some("unsafe_file_fallback"));

    let inspected = scene.body("get_layer_list", Some("saved")).await;
    assert_eq!(
        inspected["copper_layer_count"],
        json!(SAVED_COPPER_LAYER_COUNT),
        "{inspected}"
    );
    assert_eq!(
        inspected["source_evidence"]["reason"],
        json!("explicitly_requested_after_live_observation"),
        "{inspected}"
    );
    assert_eq!(
        inspected["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{inspected}"
    );
}

/// A layer whose name KiCad declines to give is one cosmetic lookup, not a
/// reason to fail a whole live read — it falls back to the canonical name.
/// The enabled set KiCad did give is still the answer.
#[tokio::test]
async fn a_declined_layer_name_does_not_fail_the_live_read() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    let server = spawn_live_kicad_skipping(&board, "GetBoardLayerName");
    let handler = handler_talking_to(server.address()).await;
    let scene = Scene {
        _dir: dir,
        board,
        kicad: Some(server),
        handler,
    };

    let body = scene.body("get_layer_list", None).await;

    assert_eq!(layer_names(&body), LIVE_LAYERS, "{body}");
    assert_eq!(
        body["copper_layer_count"],
        json!(LIVE_COPPER_LAYER_COUNT),
        "{body}"
    );
    // The rename is what was lost, so the canonical spelling stands in.
    assert_eq!(display_name_of(&body, "F.Cu"), "F.Cu");
    assert_eq!(
        body["source_evidence"]["board_state"],
        json!("ipc"),
        "{body}"
    );
}

/// No board has zero enabled layers — Edge.Cuts and the courtyards cannot be
/// disabled. An `AS_OK` carrying no layer set is KiCad declining to answer,
/// and reporting it as a live stackup of nothing would be a confident lie
/// about a board whose real stackup is sitting in the file.
#[tokio::test]
async fn an_empty_enabled_layer_reply_is_not_reported_as_a_live_stackup() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    let server = spawn_live_kicad_skipping(&board, "GetBoardEnabledLayers");
    let handler = handler_talking_to(server.address()).await;
    let scene = Scene {
        _dir: dir,
        board,
        kicad: Some(server),
        handler,
    };

    let result = scene.call("get_layer_list", None).await;

    assert!(result.is_error, "{:?}", body_of(&result));
    assert_eq!(kind_of(&result).as_deref(), Some("editor_unavailable"));
    let refusal = body_of(&result);
    assert!(
        !refusal.to_string().contains("\"count\": 0"),
        "an empty stackup must not be handed back as the answer: {refusal}"
    );
}

/// `AS_UNHANDLED` and `AS_UNIMPLEMENTED` both leave Copperpour with no live
/// board, and both fall back to the saved file — but only the first is
/// evidence about KiCad's editors. A build that does not implement the
/// open-document command may well have a board open with unsaved changes, so
/// the answer must not carry the "no PCB editor at this endpoint" claim.
#[tokio::test]
async fn an_unimplemented_open_document_command_claims_nothing_about_editors() {
    let scene = Scene::not_implementing_open_documents().await;

    for tool in ["get_layer_list", "get_netclasses"] {
        let body = scene.body(tool, None).await;
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("open_documents_unimplemented_at_endpoint"),
            "{tool}: {body}"
        );
        let detail = body["source_evidence"]["detail"]
            .as_str()
            .unwrap_or_default();
        assert!(
            detail.contains("does not implement"),
            "{tool}: the detail has to say what KiCad actually answered: {detail}"
        );
        assert!(
            !detail.contains("no PCB editor"),
            "{tool}: an unimplemented command is not evidence that no editor is open: {detail}"
        );
        // The file answer still excludes whatever an editor might hold; that
        // is a property of reading the file, not a claim about KiCad.
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }

    // `live` still refuses: nothing was identified, whichever way KiCad said so.
    let refused = scene.call("get_layer_list", Some("live")).await;
    assert!(refused.is_error, "{:?}", body_of(&refused));
    assert_eq!(kind_of(&refused).as_deref(), Some("editor_unavailable"));
}

/// The other side of the classifier. Only `AS_UNHANDLED` and
/// `AS_UNIMPLEMENTED` on the open-document list mean that no board was served.
/// Any other refusal of that list comes from a KiCad that may hold this board:
/// busy with an operation, or still starting with the board open. The default
/// therefore refuses rather than answering from a file that may be older than
/// the editor, exactly as a refusal after identification does. `saved` still
/// answers, because it asked for the file.
///
/// Every other failure status in KiCad's `envelope.proto` is covered, so a
/// classifier widened to "any status refusing the list means no editor"
/// fails here rather than reading a busy editor's board from disk.
#[tokio::test]
async fn any_other_refusal_of_the_open_document_list_is_not_read_as_no_editor() {
    use kiapi::common::ApiStatusCode as Status;

    for status in [
        Status::AsBusy,
        Status::AsNotReady,
        Status::AsTimeout,
        Status::AsBadRequest,
        Status::AsTokenMismatch,
    ] {
        let scene = Scene::refusing_before_identification(status).await;

        for tool in ["get_layer_list", "get_netclasses"] {
            for mode in [None, Some("auto"), Some("live")] {
                let refused = scene.call(tool, mode).await;
                assert!(
                    refused.is_error,
                    "{status:?} {tool} {mode:?} was answered: {}",
                    body_of(&refused)
                );
                assert_eq!(
                    kind_of(&refused).as_deref(),
                    Some("editor_unavailable"),
                    "{status:?} {tool} {mode:?}: {}",
                    body_of(&refused)
                );
            }

            let saved = scene.body(tool, Some("saved")).await;
            assert_eq!(
                saved["source_evidence"]["reason"],
                json!("explicitly_requested"),
                "{status:?} {tool}: {saved}"
            );
        }
    }
}

/// The regression the maintainer caught on a real KiCad: with only the project
/// manager running, `GetOpenDocuments` is refused outright, so no board was
/// ever identified — and #574's rule is scoped to *after* identification.
/// Refusing here would take the default answer away from every user who has
/// launched KiCad and not yet opened a board.
#[tokio::test]
async fn a_kicad_with_no_board_editor_still_answers_by_default() {
    let scene = Scene::without_a_board_editor().await;

    for tool in ["get_layer_list", "get_netclasses"] {
        for mode in [None, Some("auto")] {
            let body = scene.body(tool, mode).await;
            assert_eq!(
                body["source_evidence"]["reason"],
                json!("no_pcb_editor_at_endpoint"),
                "{tool} / {mode:?}: {body}"
            );
            assert_eq!(
                body["source_evidence"]["excludes_unsaved_editor_state"],
                json!(true),
                "{tool} / {mode:?}: {body}"
            );
        }
    }

    let body = scene.body("get_layer_list", None).await;
    assert_eq!(
        body["copper_layer_count"],
        json!(SAVED_COPPER_LAYER_COUNT),
        "{body}"
    );
}

/// `live` is still a request for a live board, and there is not one.
#[tokio::test]
async fn a_kicad_with_no_board_editor_refuses_a_live_request() {
    let scene = Scene::without_a_board_editor().await;

    let result = scene.call("get_layer_list", Some("live")).await;

    assert!(result.is_error, "{:?}", body_of(&result));
    assert_eq!(kind_of(&result).as_deref(), Some("editor_unavailable"));
}

/// The exception: an editor this session watched hold the board is gone, and
/// the saved file may be older than what it held.
#[tokio::test]
async fn a_board_seen_live_before_the_editor_closed_still_refuses() {
    let mut scene = Scene::live().await;
    scene.body("get_layer_list", None).await;

    // The PCB editor closes and the project manager takes over the same
    // endpoint. The server is the one that saw the board live, so its session
    // memory is the thing under test — a fresh handler would not have it.
    let endpoint = scene.endpoint();
    scene.lose_kicad();
    scene.kicad = Some(spawn_kicad_without_a_board_editor_at(endpoint));

    let refused = scene.call("get_layer_list", None).await;

    assert!(refused.is_error, "{:?}", body_of(&refused));
    assert_eq!(kind_of(&refused).as_deref(), Some("unsafe_file_fallback"));
}

// ─── get_netclasses ──────────────────────────────────────────────────────────

/// The mixed-source answer: definitions and patterns are project-file facts,
/// the nets are live, and the matches are derived from them. Each is named
/// separately rather than compressed into one source string.
#[tokio::test]
async fn netclasses_report_live_nets_beside_project_file_definitions() {
    let scene = Scene::live().await;

    let body = scene.body("get_netclasses", None).await;

    assert_eq!(
        body["sources"],
        json!({
            "definitions": "project_file",
            "patterns": "project_file",
            "board_nets": "ipc",
            "matched_nets": "derived",
            "unmatched_patterns": "derived",
        }),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["board_state"],
        json!("ipc"),
        "{body}"
    );
    // The empty-named unconnected pseudo-net KiCad also reports is not a net.
    assert_eq!(body["nets_on_board"], json!(LIVE_NETS.len()), "{body}");

    let rails = rails_class(&body);
    // Both patterns are present; only the live-named net can match.
    assert_eq!(
        rails["patterns"],
        json!([LIVE_NETS[0], SAVED_NETS[0]]),
        "{body}"
    );
    assert_eq!(rails["matched_nets"], json!([LIVE_NETS[0]]), "{body}");
    assert_eq!(rails["clearance"], json!(0.5), "{body}");
    // The pattern the live nets leave idle is the saved-only one.
    assert_eq!(
        body["unmatched_patterns"],
        json!([{ "netclass": "Rails", "pattern": SAVED_NETS[0] }]),
        "{body}"
    );
}

/// The same call against the saved snapshot matches the other pattern, so the
/// derived field alone says which net list produced it.
#[tokio::test]
async fn netclasses_saved_mode_matches_the_saved_nets_and_discloses_it() {
    let scene = Scene::live().await;

    let body = scene.body("get_netclasses", Some("saved")).await;

    assert_eq!(
        body["sources"]["board_nets"],
        json!("saved_board"),
        "{body}"
    );
    assert_eq!(
        body["sources"]["definitions"],
        json!("project_file"),
        "{body}"
    );
    assert_eq!(body["nets_on_board"], json!(SAVED_NETS.len()), "{body}");
    assert_eq!(
        rails_class(&body)["matched_nets"],
        json!([SAVED_NETS[0]]),
        "{body}"
    );
    assert_eq!(
        body["unmatched_patterns"],
        json!([{ "netclass": "Rails", "pattern": LIVE_NETS[0] }]),
        "{body}"
    );
    assert_eq!(
        body["sources"]["unmatched_patterns"],
        json!("derived"),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{body}"
    );
}

/// A saved board that cannot be parsed leaves the nets unknown. The patterns
/// still come from the project file, but which of them fit no net cannot be
/// answered: the field is null and its source says so, rather than an empty
/// list claiming every pattern matched.
#[tokio::test]
async fn netclasses_report_unmatched_patterns_as_unavailable_without_nets() {
    let scene = Scene::offline().await;
    std::fs::write(&scene.board, "(kicad_pcb (version 20260206").unwrap();

    let body = scene.body("get_netclasses", Some("saved")).await;

    assert!(body["unmatched_patterns"].is_null(), "{body}");
    assert_eq!(
        body["sources"],
        json!({
            "definitions": "project_file",
            "patterns": "project_file",
            "board_nets": "unavailable",
            "matched_nets": "derived",
            "unmatched_patterns": "unavailable",
        }),
        "{body}"
    );
    assert_eq!(body["nets_on_board"], json!(0), "{body}");
}

/// `get_netclasses` obeys the same live-identification rule as its sibling:
/// the project file is still readable, and that is not licence to report a
/// stale net list as the board's.
#[tokio::test]
async fn netclasses_refuse_after_a_failed_live_query() {
    let scene = Scene::rejecting().await;

    let result = scene.call("get_netclasses", None).await;

    assert!(result.is_error, "{:?}", body_of(&result));
    assert_eq!(kind_of(&result).as_deref(), Some("editor_unavailable"));
    let refusal = body_of(&result);
    assert!(!refusal.to_string().contains(SAVED_NETS[0]), "{refusal}");
}

// ─── The selector itself ─────────────────────────────────────────────────────

/// The advertised schema refuses a value neither tool can honour, before any
/// board is touched.
#[tokio::test]
async fn an_unknown_board_source_is_refused_by_name() {
    let scene = Scene::offline().await;

    for (tool, extra) in [
        ("get_layer_list", json!({})),
        ("get_netclasses", json!({})),
        ("score_placement", json!({})),
        ("get_placement_overview", json!({})),
        ("check_clearance", json!({ "ref1": "R1", "ref2": "R2" })),
        ("get_component_pads", json!({ "reference": "R1" })),
        (
            "get_pad_position",
            json!({ "reference": "R1", "pad_number": "1" }),
        ),
    ] {
        for value in [json!("ipc"), json!("file"), json!(true)] {
            let mut args = extra.clone();
            args["board"] = json!(scene.board.to_string_lossy());
            args["board_source"] = value.clone();
            let result = scene.call_with(tool, args).await;
            assert!(result.is_error, "{tool} accepted {value}");
            assert_eq!(
                kind_of(&result).as_deref(),
                Some("invalid_argument"),
                "{tool} / {value}"
            );
            assert_eq!(
                body_of(&result)["error"]["field"],
                json!("board_source"),
                "{tool} / {value}"
            );
        }
    }
}

/// Stage 4 readers (`score_placement`, `get_placement_overview`,
/// `check_clearance`, `get_component_pads`, `get_pad_position`) share the
/// exact-board `board_source` contract: live board wins under `auto`,
/// `saved` inspects the file even when KiCad holds the board open,
/// wrong-board refuses `live` (`wrong_document`) and explains `auto`
/// (`board_not_open_in_kicad`), post-identification failure refuses without
/// substituting saved state, and a board observed live then lost refuses
/// `auto` (`unsafe_file_fallback`) while disclosing `saved`.
#[tokio::test]
async fn stage4_readers_prefer_live_under_auto_and_inspect_file_under_saved() {
    let scene = Scene::live().await;

    let call_reader = |tool: &'static str, extra: Value, mode: Option<&'static str>| {
        let scene = &scene;
        async move {
            let mut args = extra;
            args["board"] = json!(scene.board.to_string_lossy());
            if let Some(m) = mode {
                args["board_source"] = json!(m);
            }
            let result = scene.call_with(tool, args).await;
            assert!(
                !result.is_error,
                "{tool} ({mode:?}): {:?}",
                body_of(&result)
            );
            body_of(&result)
        }
    };

    // 1. score_placement
    let score_live = call_reader("score_placement", json!({}), None).await;
    assert_eq!(score_live["source"], json!("ipc"), "{score_live}");
    assert_eq!(
        score_live["sources"],
        json!({ "placement": "ipc" }),
        "{score_live}"
    );
    assert_eq!(
        score_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{score_live}"
    );
    assert_eq!(
        score_live["source_evidence"]["excludes_unsaved_editor_state"],
        json!(false),
        "{score_live}"
    );
    assert_eq!(
        score_live["footprints_scored"],
        json!(LIVE_FOOTPRINT_COUNT),
        "{score_live}"
    );

    let score_saved = call_reader("score_placement", json!({}), Some("saved")).await;
    assert_eq!(score_saved["source"], json!("saved_file"), "{score_saved}");
    assert_eq!(
        score_saved["sources"],
        json!({ "placement": "saved_board" }),
        "{score_saved}"
    );
    assert_eq!(
        score_saved["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{score_saved}"
    );
    assert_eq!(
        score_saved["footprints_scored"],
        json!(SAVED_FOOTPRINT_COUNT),
        "{score_saved}"
    );

    // 2. get_placement_overview (mixed source: placement + net_groups + groups)
    let overview_live = call_reader("get_placement_overview", json!({}), None).await;
    assert_eq!(overview_live["source"], json!("ipc"), "{overview_live}");
    assert_eq!(
        overview_live["sources"],
        json!({
            "placement": "ipc",
            "net_groups": "project_file",
            "groups": "derived",
        }),
        "{overview_live}"
    );
    assert_eq!(
        overview_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{overview_live}"
    );
    assert_eq!(
        overview_live["footprints"],
        json!(LIVE_FOOTPRINT_COUNT),
        "{overview_live}"
    );
    assert_eq!(
        overview_live["groups"][0]["nets"],
        json!([LIVE_NETS[0]]),
        "{overview_live}"
    );

    let overview_saved = call_reader("get_placement_overview", json!({}), Some("saved")).await;
    assert_eq!(
        overview_saved["source"],
        json!("saved_file"),
        "{overview_saved}"
    );
    assert_eq!(
        overview_saved["sources"],
        json!({
            "placement": "saved_board",
            "net_groups": "project_file",
            "groups": "derived",
        }),
        "{overview_saved}"
    );
    assert_eq!(
        overview_saved["footprints"],
        json!(SAVED_FOOTPRINT_COUNT),
        "{overview_saved}"
    );
    assert_eq!(
        overview_saved["groups"][0]["nets"],
        json!([SAVED_NETS[0]]),
        "{overview_saved}"
    );

    let overview_arg = call_reader(
        "get_placement_overview",
        json!({ "net_groups": { "Custom": [LIVE_NETS[1]] } }),
        Some("live"),
    )
    .await;
    assert_eq!(
        overview_arg["sources"]["net_groups"],
        json!("argument"),
        "{overview_arg}"
    );

    // 3. check_clearance (anchor and courtyard modes)
    let clearance_live = call_reader(
        "check_clearance",
        json!({ "ref1": "R1", "ref2": "R2" }),
        None,
    )
    .await;
    assert_eq!(clearance_live["source"], json!("ipc"), "{clearance_live}");
    assert_eq!(
        clearance_live["sources"],
        json!({ "clearance": "ipc" }),
        "{clearance_live}"
    );
    assert_eq!(
        clearance_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{clearance_live}"
    );
    assert_eq!(
        clearance_live["source_evidence"]["excludes_unsaved_editor_state"],
        json!(false),
        "{clearance_live}"
    );
    assert_eq!(
        clearance_live["anchor_distance_mm"],
        json!(LIVE_ANCHOR_CLEARANCE_MM),
        "{clearance_live}"
    );

    let clearance_saved = call_reader(
        "check_clearance",
        json!({ "ref1": "R1", "ref2": "R2" }),
        Some("saved"),
    )
    .await;
    assert_eq!(
        clearance_saved["source"],
        json!("saved_file"),
        "{clearance_saved}"
    );
    assert_eq!(
        clearance_saved["sources"],
        json!({ "clearance": "saved_board" }),
        "{clearance_saved}"
    );
    assert_eq!(
        clearance_saved["anchor_distance_mm"],
        json!(SAVED_ANCHOR_CLEARANCE_MM),
        "{clearance_saved}"
    );

    let courtyard_live = call_reader(
        "check_clearance",
        json!({ "ref1": "R1", "ref2": "R2", "mode": "courtyard" }),
        None,
    )
    .await;
    assert_eq!(courtyard_live["source"], json!("ipc"), "{courtyard_live}");
    assert_eq!(
        courtyard_live["sources"],
        json!({ "clearance": "ipc" }),
        "{courtyard_live}"
    );
    assert_eq!(
        courtyard_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{courtyard_live}"
    );
    assert_eq!(courtyard_live["available"], json!(true), "{courtyard_live}");
    // Live R1 is at x=85 (hull [84, 86]) and R2 is at x=109 (hull [108, 110]) -> 22.0 mm gap.
    assert_eq!(
        courtyard_live["courtyard_clearance_mm"],
        json!(22.0),
        "{courtyard_live}"
    );

    let courtyard_saved = call_reader(
        "check_clearance",
        json!({ "ref1": "R1", "ref2": "R2", "mode": "courtyard" }),
        Some("saved"),
    )
    .await;
    assert_eq!(
        courtyard_saved["source"],
        json!("saved_file"),
        "{courtyard_saved}"
    );
    assert_eq!(
        courtyard_saved["sources"],
        json!({ "clearance": "saved_board" }),
        "{courtyard_saved}"
    );
    assert_eq!(
        courtyard_saved["source_evidence"]["excludes_unsaved_editor_state"],
        json!(true),
        "{courtyard_saved}"
    );
    assert_eq!(
        courtyard_saved["available"],
        json!(false),
        "{courtyard_saved}"
    );
    assert_eq!(
        courtyard_saved["reason_code"],
        json!("authored_courtyard_missing"),
        "{courtyard_saved}"
    );

    // 4. get_component_pads
    let pads_live = call_reader("get_component_pads", json!({ "reference": "R1" }), None).await;
    assert_eq!(pads_live["source"], json!("ipc"), "{pads_live}");
    assert_eq!(
        pads_live["sources"],
        json!({ "pads": "ipc" }),
        "{pads_live}"
    );
    assert_eq!(
        pads_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{pads_live}"
    );
    assert_eq!(
        pads_live["source_evidence"]["excludes_unsaved_editor_state"],
        json!(false),
        "{pads_live}"
    );
    assert_eq!(
        pads_live["pads"][0]["x"],
        json!(LIVE_R1_PAD1_X),
        "{pads_live}"
    );
    assert_eq!(
        pads_live["pads"][0]["net"],
        json!(LIVE_NETS[0]),
        "{pads_live}"
    );

    let pads_saved = call_reader(
        "get_component_pads",
        json!({ "reference": "R1" }),
        Some("saved"),
    )
    .await;
    assert_eq!(pads_saved["source"], json!("file"), "{pads_saved}");
    assert_eq!(
        pads_saved["sources"],
        json!({ "pads": "saved_board" }),
        "{pads_saved}"
    );
    assert_eq!(
        pads_saved["pads"][0]["x"],
        json!(SAVED_R1_PAD1_X),
        "{pads_saved}"
    );
    assert_eq!(
        pads_saved["pads"][0]["net"],
        json!(SAVED_NETS[0]),
        "{pads_saved}"
    );

    // 5. get_pad_position
    let pad_live = call_reader(
        "get_pad_position",
        json!({ "reference": "R1", "pad_number": "1" }),
        None,
    )
    .await;
    assert_eq!(pad_live["source"], json!("ipc"), "{pad_live}");
    assert_eq!(pad_live["sources"], json!({ "pad": "ipc" }), "{pad_live}");
    assert_eq!(
        pad_live["source_evidence"]["board_state"],
        json!("ipc"),
        "{pad_live}"
    );
    assert_eq!(
        pad_live["source_evidence"]["excludes_unsaved_editor_state"],
        json!(false),
        "{pad_live}"
    );
    assert_eq!(pad_live["x"], json!(LIVE_R1_PAD1_X), "{pad_live}");
    assert_eq!(pad_live["net"], json!(LIVE_NETS[0]), "{pad_live}");

    let pad_saved = call_reader(
        "get_pad_position",
        json!({ "reference": "R1", "pad_number": "1" }),
        Some("saved"),
    )
    .await;
    assert_eq!(pad_saved["source"], json!("file"), "{pad_saved}");
    assert_eq!(
        pad_saved["sources"],
        json!({ "pad": "saved_board" }),
        "{pad_saved}"
    );
    assert_eq!(pad_saved["x"], json!(SAVED_R1_PAD1_X), "{pad_saved}");
    assert_eq!(pad_saved["net"], json!(SAVED_NETS[0]), "{pad_saved}");
}

#[tokio::test]
async fn stage4_readers_enforce_refusal_and_staleness_across_all_five_tools() {
    let readers = [
        ("score_placement", json!({})),
        ("get_placement_overview", json!({})),
        ("check_clearance", json!({ "ref1": "R1", "ref2": "R2" })),
        ("get_component_pads", json!({ "reference": "R1" })),
        (
            "get_pad_position",
            json!({ "reference": "R1", "pad_number": "1" }),
        ),
    ];

    // Post-identification failure refuses under auto and live without falling back.
    let rejecting = Scene::rejecting().await;
    for (tool, extra) in &readers {
        for mode in [None, Some("auto"), Some("live")] {
            let mut args = extra.clone();
            args["board"] = json!(rejecting.board.to_string_lossy());
            if let Some(m) = mode {
                args["board_source"] = json!(m);
            }
            let refused = rejecting.call_with(tool, args).await;
            assert!(
                refused.is_error,
                "{tool} ({mode:?}) must refuse after live identification failure: {}",
                body_of(&refused)
            );
            assert_eq!(
                kind_of(&refused).as_deref(),
                Some("editor_unavailable"),
                "{tool} ({mode:?}): {}",
                body_of(&refused)
            );
            let text = body_of(&refused).to_string();
            assert!(
                !text.contains(SAVED_NETS[0]),
                "{tool} ({mode:?}) leaked saved board state: {text}"
            );
        }
    }

    // Wrong board open in KiCad refuses `live` as `wrong_document` and falls back under `auto`.
    let other_board = Scene::holding_another_board().await;
    for (tool, extra) in &readers {
        let mut live_args = extra.clone();
        live_args["board"] = json!(other_board.board.to_string_lossy());
        live_args["board_source"] = json!("live");
        let refused = other_board.call_with(tool, live_args).await;
        assert!(refused.is_error, "{tool} live on wrong board must refuse");
        assert_eq!(
            kind_of(&refused).as_deref(),
            Some("wrong_document"),
            "{tool}"
        );

        let mut auto_args = extra.clone();
        auto_args["board"] = json!(other_board.board.to_string_lossy());
        let ok = other_board.call_with(tool, auto_args).await;
        assert!(!ok.is_error, "{tool} auto: {:?}", body_of(&ok));
        let body = body_of(&ok);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("board_not_open_in_kicad"),
            "{tool}: {body}"
        );
    }

    // Previously observed live then lost: refuses `auto` with `unsafe_file_fallback`,
    // allows `saved` with `explicitly_requested_after_live_observation`.
    let mut lost = Scene::live().await;
    let _ = lost.body("score_placement", None).await;
    lost.lose_kicad();

    for (tool, extra) in &readers {
        let mut auto_args = extra.clone();
        auto_args["board"] = json!(lost.board.to_string_lossy());
        let refused = lost.call_with(tool, auto_args).await;
        assert!(refused.is_error, "{tool} must refuse stale fallback");
        assert_eq!(
            kind_of(&refused).as_deref(),
            Some("unsafe_file_fallback"),
            "{tool}"
        );

        let mut saved_args = extra.clone();
        saved_args["board"] = json!(lost.board.to_string_lossy());
        saved_args["board_source"] = json!("saved");
        let ok = lost.call_with(tool, saved_args).await;
        assert!(!ok.is_error, "{tool} saved: {:?}", body_of(&ok));
        let body = body_of(&ok);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("explicitly_requested_after_live_observation"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }
}

/// With two boards open in KiCad simultaneously, both the snapshot reader
/// (`score_placement` via `SaveDocumentToString`) and the item reader
/// (`get_component_pads` via `GetItems`) bind and address the exact requested
/// document rather than KiCad's default/first open board.
#[tokio::test]
async fn stage4_readers_with_two_open_boards_address_the_requested_document() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first_open.kicad_pcb");
    std::fs::write(&first, "").unwrap();
    let requested = fixture_board(dir.path());
    write_project(&requested);

    let first_doc = board_document(&first.to_string_lossy());
    let requested_doc = board_document(&requested.to_string_lossy());
    let server = crate::tools::pcb_board::board_mock::spawn_kicad_reporting_documents(
        vec![first_doc.clone(), requested_doc.clone()],
        move |command| {
            if command.type_url.ends_with("SaveDocumentToString") {
                let asked =
                    kiapi::common::commands::SaveDocumentToString::decode(&command.value[..])
                        .expect("SaveDocumentToString request");
                assert_eq!(
                    asked.document,
                    Some(requested_doc.clone()),
                    "must request the second (requested) board document, not the first"
                );
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::SavedDocumentResponse {
                        document: asked.document,
                        contents: LIVE_DOCUMENT_SNAPSHOT.to_string(),
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                ));
            }
            if command.type_url.ends_with("GetItems") {
                let asked = kiapi::common::commands::GetItems::decode(&command.value[..])
                    .expect("GetItems request");
                assert_eq!(
                    asked.header.as_ref().and_then(|h| h.document.clone()),
                    Some(requested_doc.clone()),
                    "GetItems must address the requested board document, not the first"
                );
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        header: asked.header,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items: vec![live_r1_footprint()],
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ));
            }
            None
        },
    );
    let handler = handler_talking_to(server.address()).await;
    let scene = Scene {
        _dir: dir,
        board: requested,
        kicad: Some(server),
        handler,
    };

    let score = scene.body("score_placement", None).await;
    assert_eq!(score["source"], json!("ipc"), "{score}");
    assert_eq!(
        score["footprints_scored"],
        json!(LIVE_FOOTPRINT_COUNT),
        "{score}"
    );

    let pads_res = scene
        .call_with(
            "get_component_pads",
            json!({ "board": scene.board.to_string_lossy(), "reference": "R1" }),
        )
        .await;
    assert!(!pads_res.is_error, "{:?}", body_of(&pads_res));
    let pads = body_of(&pads_res);
    assert_eq!(pads["source"], json!("ipc"), "{pads}");
    assert_eq!(pads["pads"][0]["x"], json!(LIVE_R1_PAD1_X), "{pads}");
}

/// Without a readable `.kicad_pro`, `get_placement_overview` discloses
/// `sources.net_groups: "unavailable"` rather than claiming project-file data.
#[tokio::test]
async fn placement_overview_without_project_file_reports_net_groups_unavailable() {
    let scene = Scene::offline().await;
    std::fs::remove_file(scene.board.with_extension("kicad_pro")).unwrap();

    let body = scene.body("get_placement_overview", Some("saved")).await;
    assert_eq!(
        body["sources"],
        json!({
            "placement": "saved_board",
            "net_groups": "unavailable",
            "groups": "derived",
        }),
        "{body}"
    );
    assert_eq!(body["groups"], json!([]), "{body}");
}

/// Omitting the selector keeps every existing caller working, and means
/// `auto`: with no KiCad reachable that is the saved file, disclosed.
#[tokio::test]
async fn omitting_the_selector_reads_the_saved_board_and_says_so() {
    let scene = Scene::offline().await;

    let body = scene.body("get_layer_list", None).await;

    assert_eq!(
        body["copper_layer_count"],
        json!(SAVED_COPPER_LAYER_COUNT),
        "{body}"
    );
    assert_eq!(
        body["source_evidence"]["reason"],
        json!("kicad_ipc_unreachable"),
        "{body}"
    );
    // The request is never echoed back as evidence that a source was used.
    assert!(body.get("board_source").is_none(), "{body}");
}

// ─── Writes in the same states (#83) ─────────────────────────────────────────
//
// A write refuses where a read answers from the saved file; that boundary is
// mixelpixx/Konnect#577's to move, not these tests'. What they pin is the
// refusal's explanation: it must state what KiCad said, not more.

/// The text of a result, which for a plain refusal is prose rather than JSON.
fn text_of(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

/// Two writers that share the live-write gate: one naming its document
/// (`set_board_title_block`) and one that does not (`add_board_text`).
fn writes_through_the_live_gate(board: &Path) -> [(&'static str, Value); 2] {
    let board = board.to_string_lossy();
    [
        (
            "set_board_title_block",
            json!({ "board": board, "title": "Never written" }),
        ),
        (
            "add_board_text",
            json!({ "board": board, "text": "Never written", "x": 5.0, "y": 5.0 }),
        ),
    ]
}

/// The issue: with only the project manager running, every live PCB write
/// said KiCad "may hold this board open". `AS_UNHANDLED` on the open-document
/// list says there is no PCB editor at all, so the refusal now says that and
/// how to get the write through, and the board file is left alone.
#[tokio::test]
async fn a_write_with_no_board_editor_says_so_and_leaves_the_file() {
    let scene = Scene::without_a_board_editor().await;
    let before = std::fs::read(&scene.board).unwrap();

    for (tool, arguments) in writes_through_the_live_gate(&scene.board) {
        let refused = scene.call_with(tool, arguments).await;

        assert!(refused.is_error, "{tool}: {}", text_of(&refused));
        let text = text_of(&refused);
        assert!(
            text.contains("KiCad is running but reports no PCB editor at this endpoint"),
            "{tool} must say what KiCad answered: {text}"
        );
        assert!(
            text.contains("Open this board in the PCB Editor and retry"),
            "{tool} must say how to get the write through: {text}"
        );
        assert!(
            !text.contains("quit KiCad"),
            "{tool} must not offer a file path the live-only writers never take: {text}"
        );
        assert!(
            !text.contains("may hold this"),
            "{tool} must not claim an editor might hold the board: {text}"
        );
        assert!(text.contains("not modified"), "{tool}: {text}");
    }
    assert_eq!(
        std::fs::read(&scene.board).unwrap(),
        before,
        "the refusal still never falls back to the file"
    );
}

/// `AS_UNIMPLEMENTED` establishes nothing about the editors, so a write there
/// keeps the cautious explanation.
#[tokio::test]
async fn a_write_to_a_kicad_not_naming_its_documents_keeps_the_cautious_refusal() {
    let scene = Scene::not_implementing_open_documents().await;
    let before = std::fs::read(&scene.board).unwrap();

    for (tool, arguments) in writes_through_the_live_gate(&scene.board) {
        let refused = scene.call_with(tool, arguments).await;

        assert!(refused.is_error, "{tool}: {}", text_of(&refused));
        let text = text_of(&refused);
        assert!(text.contains("may hold this"), "{tool}: {text}");
        assert!(
            !text.contains("reports no PCB editor"),
            "{tool} must not claim more than the status carried: {text}"
        );
    }
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}

/// A live-only writer never edits the file, so quitting KiCad would only lead
/// to its own file-path refusal ("is live-IPC-only and never edits the board
/// file directly"). It gets the same advice as every other writer: open the
/// board in the PCB Editor.
#[tokio::test]
async fn a_live_only_write_with_no_board_editor_offers_only_the_pcb_editor() {
    let scene = Scene::without_a_board_editor().await;
    let before = std::fs::read(&scene.board).unwrap();

    let result = scene
        .call_with(
            "update_footprints_from_library",
            json!({ "board": scene.board.to_string_lossy(), "dry_run": true }),
        )
        .await;

    let body = body_of(&result);
    assert_eq!(body["status"], "conflict", "{body}");
    let message = body["diagnostics"][0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a diagnostic message: {body}"));
    assert!(
        message.contains("KiCad is running but reports no PCB editor at this endpoint")
            && message.contains("Open this board in the PCB Editor and retry"),
        "{message}"
    );
    assert!(
        !message.contains("quit KiCad"),
        "quitting KiCad would only reach the live-only refusal: {message}"
    );
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}

// ─── Stage 4b: saved-snapshot boundary disclosure (#58) ──────────────────────

/// A symbol-free, KiCad-saved sheet for the `run_design_review` contract
/// tests, copied beside the board under its stem.
fn write_minimal_schematic(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/schematic_page_fit_custom_sheet_kicad10.kicad_sch");
    let sch = dir.join("board_source_divergence_kicad10.kicad_sch");
    std::fs::copy(&source, &sch).expect("the fixture is committed beside the tests");
    sch
}

#[cfg(unix)]
fn write_fake_clean_drc_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-kicad-cli.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         out=\"\"\n\
         while [ \"$#\" -gt 0 ]; do\n\
           if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\n\
         done\n\
         printf '{\"violations\":[],\"unconnected_items\":[],\"schematic_parity\":[]}\\n' > \"$out\"\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

/// Every Stage 4b saved-snapshot tool reports per-domain `sources` and
/// `source_evidence` disclosing that unsaved KiCad editor changes are excluded,
/// both offline and when a sibling editor lock file is present on disk.
#[tokio::test]
async fn stage4b_saved_snapshot_tools_disclose_boundary_offline_and_with_lock() {
    let scene = Scene::offline().await;
    let board = scene.board.to_string_lossy().to_string();
    let sch = write_minimal_schematic(scene._dir.path());
    let sch_str = sch.to_string_lossy().to_string();

    // Baseline: offline, no lock, no .kicad_dru.
    let audit = body_of(
        &scene
            .call_with("audit_manufacturing", json!({ "board": board }))
            .await,
    );
    assert_eq!(
        audit["sources"],
        json!({ "board": "saved_board" }),
        "{audit}"
    );
    assert_eq!(audit["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        audit["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(audit["source_evidence"]["reason"], "saved_snapshot_only");

    let models = body_of(
        &scene
            .call_with("check_3d_models", json!({ "board": board }))
            .await,
    );
    assert_eq!(
        models["sources"],
        json!({ "footprints": "saved_board" }),
        "{models}"
    );
    assert_eq!(models["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        models["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(models["source_evidence"]["reason"], "saved_snapshot_only");

    let cost_default = body_of(
        &scene
            .call_with("estimate_cost", json!({ "board": board }))
            .await,
    );
    assert_eq!(
        cost_default["sources"],
        json!({
            "board": "saved_board",
            "copper_layers": "saved_board",
            "cost_estimate": "derived",
        }),
        "{cost_default}"
    );
    assert_eq!(
        cost_default["source_evidence"]["board_state"],
        "saved_board"
    );
    assert_eq!(
        cost_default["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        cost_default["source_evidence"]["reason"],
        "saved_snapshot_only"
    );

    let cost_override = body_of(
        &scene
            .call_with("estimate_cost", json!({ "board": board, "layers": 6 }))
            .await,
    );
    assert_eq!(
        cost_override["sources"]["copper_layers"],
        json!("argument"),
        "{cost_override}"
    );

    let rules = body_of(
        &scene
            .call_with("get_design_rules", json!({ "board": board }))
            .await,
    );
    assert_eq!(
        rules["sources"],
        json!({ "rules": "project_file" }),
        "{rules}"
    );
    assert_eq!(rules["source_evidence"]["board_state"], "project_file");
    assert_eq!(
        rules["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(rules["source_evidence"]["reason"], "saved_snapshot_only");

    let sizes = body_of(
        &scene
            .call_with("get_predefined_sizes", json!({ "board": board }))
            .await,
    );
    assert_eq!(
        sizes["sources"],
        json!({
            "track_widths": "project_file",
            "via_dimensions": "project_file",
        }),
        "{sizes}"
    );
    assert_eq!(sizes["source_evidence"]["board_state"], "project_file");
    assert_eq!(
        sizes["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(sizes["source_evidence"]["reason"], "saved_snapshot_only");

    // list_custom_rules without a .kicad_dru file reports rules as unavailable.
    let custom_missing = body_of(
        &scene
            .call_with("list_custom_rules", json!({ "board": board }))
            .await,
    );
    assert_eq!(custom_missing["exists"], false, "{custom_missing}");
    assert_eq!(
        custom_missing["sources"],
        json!({
            "rules": "unavailable",
            "user_layers": "saved_board",
        }),
        "{custom_missing}"
    );
    assert_eq!(
        custom_missing["source_evidence"]["board_state"],
        "saved_board"
    );
    assert_eq!(
        custom_missing["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        custom_missing["source_evidence"]["reason"],
        "saved_snapshot_only"
    );

    // With a .kicad_dru file present, rules provenance is custom_rules_file.
    std::fs::write(
        scene.board.with_extension("kicad_dru"),
        "(version 1)\n(rule \"HV\" (constraint clearance (min 0.5mm)))\n",
    )
    .unwrap();
    let custom_present = body_of(
        &scene
            .call_with("list_custom_rules", json!({ "board": board }))
            .await,
    );
    assert_eq!(custom_present["exists"], true, "{custom_present}");
    assert_eq!(
        custom_present["sources"],
        json!({
            "rules": "custom_rules_file",
            "user_layers": "saved_board",
        }),
        "{custom_present}"
    );

    // validate_for_manufacturing with no kicad-cli: DRC is unavailable, output file also carries provenance.
    let report_path = scene._dir.path().join("mfg-validation.json");
    let validation = body_of(
        &scene
            .call_with(
                "validate_for_manufacturing",
                json!({ "board": board, "output": report_path.to_string_lossy() }),
            )
            .await,
    );
    assert_eq!(
        validation["sources"],
        json!({
            "board": "saved_board",
            "drc": "unavailable",
        }),
        "{validation}"
    );
    assert_eq!(validation["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        validation["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        validation["source_evidence"]["reason"],
        "saved_snapshot_only"
    );
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(written["sources"], validation["sources"]);
    assert_eq!(written["source_evidence"], validation["source_evidence"]);

    // run_design_review without a board reports schematic-only provenance and keeps outcome.
    let review_sch = body_of(
        &scene
            .call_with("run_design_review", json!({ "schematic": sch_str }))
            .await,
    );
    assert_eq!(
        review_sch["sources"],
        json!({ "schematic": "saved_schematic" }),
        "{review_sch}"
    );
    assert_eq!(review_sch["source_evidence"]["board_state"], Value::Null);
    assert_eq!(
        review_sch["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(review_sch["source_evidence"]["reason"], "no_board_supplied");
    assert!(review_sch.get("outcome").is_some(), "{review_sch}");

    // run_design_review with a board (and no kicad-cli) discloses saved board + unavailable DRC.
    let review_both = body_of(
        &scene
            .call_with(
                "run_design_review",
                json!({ "schematic": sch_str, "board": board }),
            )
            .await,
    );
    assert_eq!(
        review_both["sources"],
        json!({
            "schematic": "saved_schematic",
            "board": "saved_board",
            "drc": "unavailable",
        }),
        "{review_both}"
    );
    assert_eq!(review_both["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        review_both["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        review_both["source_evidence"]["reason"],
        "saved_snapshot_only"
    );

    // Now place a sibling editor lock beside the board: all 8 tools still succeed
    // and disclose `saved_snapshot_only_with_editor_lock`.
    let lock_path = copperpour_sexp::writer::kicad_editor_lock_path(&scene.board).unwrap();
    std::fs::write(&lock_path, "locked").unwrap();
    for (tool, args) in [
        ("audit_manufacturing", json!({ "board": board })),
        ("check_3d_models", json!({ "board": board })),
        ("estimate_cost", json!({ "board": board })),
        ("get_design_rules", json!({ "board": board })),
        ("get_predefined_sizes", json!({ "board": board })),
        ("list_custom_rules", json!({ "board": board })),
        ("validate_for_manufacturing", json!({ "board": board })),
        (
            "run_design_review",
            json!({ "schematic": sch_str, "board": board }),
        ),
    ] {
        let result = scene.call_with(tool, args).await;
        assert!(
            !result.is_error,
            "{tool} should not refuse on editor lock: {}",
            text_of(&result)
        );
        let body = body_of(&result);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("saved_snapshot_only_with_editor_lock"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }
}

/// After the server has observed a board live in KiCad, every Stage 4b saved-snapshot
/// tool discloses `saved_snapshot_only_after_live_observation` without consulting IPC
/// or refusing the read-only snapshot.
#[tokio::test]
async fn stage4b_saved_snapshot_tools_disclose_after_live_observation() {
    let scene = Scene::live().await;
    let board = scene.board.to_string_lossy().to_string();
    let sch = write_minimal_schematic(scene._dir.path());
    let sch_str = sch.to_string_lossy().to_string();

    // First observe the board live via an interactive reader on the same handler.
    let live = scene.body("get_layer_list", Some("live")).await;
    assert_eq!(live["source_evidence"]["board_state"], "ipc", "{live}");

    for (tool, args) in [
        ("audit_manufacturing", json!({ "board": board })),
        ("check_3d_models", json!({ "board": board })),
        ("estimate_cost", json!({ "board": board })),
        ("get_design_rules", json!({ "board": board })),
        ("get_predefined_sizes", json!({ "board": board })),
        ("list_custom_rules", json!({ "board": board })),
        ("validate_for_manufacturing", json!({ "board": board })),
        (
            "run_design_review",
            json!({ "schematic": sch_str, "board": board }),
        ),
    ] {
        let result = scene.call_with(tool, args).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        let body = body_of(&result);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("saved_snapshot_only_after_live_observation"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }
}

/// When `kicad-cli` completes DRC, both `validate_for_manufacturing` and
/// `run_design_review` report `sources.drc == "kicad_cli"` alongside
/// `sources.board == "saved_board"`.
#[cfg(unix)]
#[tokio::test]
async fn stage4b_cli_drc_analysis_tools_report_kicad_cli_source_when_drc_runs() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    write_project(&board);
    let sch = write_minimal_schematic(dir.path());
    let fake_cli = write_fake_clean_drc_cli(dir.path());

    let handler = McpHandler::new(ServerConfig {
        kicad_cli: fake_cli.to_string_lossy().into_owned(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: None,
        handler,
    };
    let board_str = scene.board.to_string_lossy().to_string();
    let sch_str = sch.to_string_lossy().to_string();

    let validation = body_of(
        &scene
            .call_with("validate_for_manufacturing", json!({ "board": board_str }))
            .await,
    );
    assert_eq!(
        validation["sources"],
        json!({
            "board": "saved_board",
            "drc": "kicad_cli",
        }),
        "{validation}"
    );
    assert_eq!(
        validation["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );

    let review = body_of(
        &scene
            .call_with(
                "run_design_review",
                json!({ "schematic": sch_str, "board": board_str }),
            )
            .await,
    );
    assert_eq!(
        review["sources"],
        json!({
            "schematic": "saved_schematic",
            "board": "saved_board",
            "drc": "kicad_cli",
        }),
        "{review}"
    );
    assert_eq!(
        review["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
}

/// `get_design_rules` and `get_predefined_sizes` also disclose a sibling
/// `.kicad_pro` lock when no board lock is present.
#[tokio::test]
async fn stage4b_project_file_readers_disclose_project_lock() {
    let scene = Scene::offline().await;
    let board = scene.board.to_string_lossy().to_string();
    let pro_lock =
        copperpour_sexp::writer::kicad_project_lock_path(&scene.board.with_extension("kicad_pro"))
            .unwrap();
    std::fs::write(&pro_lock, "locked").unwrap();

    for tool in ["get_design_rules", "get_predefined_sizes"] {
        let body = body_of(&scene.call_with(tool, json!({ "board": board })).await);
        assert_eq!(
            body["source_evidence"]["board_state"],
            json!("project_file"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("saved_snapshot_only_with_editor_lock"),
            "{tool}: {body}"
        );
    }
}

/// When `run_design_review` is given a missing/unreadable board path (and DRC
/// cannot run), both `sources.board` and `source_evidence.board_state` report
/// `"unavailable"` rather than claiming a saved board was read.
#[tokio::test]
async fn stage4b_design_review_with_unreadable_board_reports_unavailable_state() {
    let scene = Scene::offline().await;
    let sch = write_minimal_schematic(scene._dir.path());
    let missing_board = scene._dir.path().join("does_not_exist.kicad_pcb");

    let review = body_of(
        &scene
            .call_with(
                "run_design_review",
                json!({
                    "schematic": sch.to_string_lossy(),
                    "board": missing_board.to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        review["sources"],
        json!({
            "schematic": "saved_schematic",
            "board": "unavailable",
            "drc": "unavailable",
        }),
        "{review}"
    );
    assert_eq!(review["source_evidence"]["board_state"], "unavailable");
    assert_eq!(
        review["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
}

/// When a saved board declares no `(layers ...)` table (`board_copper_layers == 0`)
/// and the caller omits `layers`, `estimate_cost` reports `sources.copper_layers`
/// as `"unavailable"` rather than `"saved_board"`.
#[tokio::test]
async fn stage4b_estimate_cost_without_layers_table_reports_copper_layers_unavailable() {
    let scene = Scene::offline().await;
    let no_layers_board = scene._dir.path().join("no_layers.kicad_pcb");
    std::fs::write(&no_layers_board, "(kicad_pcb (version 20260206))\n").unwrap();

    let cost = body_of(
        &scene
            .call_with(
                "estimate_cost",
                json!({ "board": no_layers_board.to_string_lossy() }),
            )
            .await,
    );
    assert_eq!(
        cost["sources"],
        json!({
            "board": "saved_board",
            "copper_layers": "unavailable",
            "cost_estimate": "derived",
        }),
        "{cost}"
    );
}

// ─── Stage 5: CLI/render/export provenance disclosure (#58) ──────────────────

/// Fake `kicad-cli` stub that creates verified non-empty output artifacts for
/// every Stage 5 CLI export, render, BOM, and netlist subcommand.
#[cfg(unix)]
fn write_fake_export_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-kicad-export-cli.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         set -eu\n\
         sub1=\"${1:-}\"\n\
         sub2=\"${2:-}\"\n\
         sub3=\"${3:-}\"\n\
         out=\"\"\n\
         layers=\"\"\n\
         last=\"\"\n\
         while [ \"$#\" -gt 0 ]; do\n\
           if [ \"$1\" = \"--output\" ] || [ \"$1\" = \"-o\" ]; then\n\
             out=\"$2\"; shift 2\n\
           elif [ \"$1\" = \"--layers\" ]; then\n\
             layers=\"$2\"; shift 2\n\
           else\n\
             last=\"$1\"; shift\n\
           fi\n\
         done\n\
         stem=\"$(basename \"$last\" .kicad_pcb)\"\n\
         case \"$sub1:$sub2:$sub3\" in\n\
           pcb:export:gerbers)\n\
             mkdir -p \"$out\"\n\
             if [ -n \"$layers\" ]; then\n\
               IFS=','; for l in $layers; do\n\
                 safe=\"$(printf '%s' \"$l\" | tr '.' '_')\"\n\
                 printf 'G04 fake gerber*\\nM02*\\n' > \"$out/${stem}-${safe}.gbr\"\n\
               done\n\
             else\n\
               printf 'G04 fake gerber*\\nM02*\\n' > \"$out/${stem}-F_Cu.gbr\"\n\
             fi\n\
             ;;\n\
           pcb:export:drill)\n\
             mkdir -p \"$out\"\n\
             printf 'M48\\nM30\\n' > \"$out/${stem}-PTH.drl\"\n\
             ;;\n\
           pcb:export:dxf)\n\
             mkdir -p \"$out\"\n\
             printf '0\\nEOF\\n' > \"$out/${stem}-F_Cu.dxf\"\n\
             ;;\n\
           pcb:drc:*)\n\
             printf '{\"violations\":[],\"unconnected_items\":[],\"schematic_parity\":[]}\\n' > \"$out\"\n\
             ;;\n\
           *)\n\
             mkdir -p \"$(dirname \"$out\")\"\n\
             printf 'fake-kicad-cli-output\\n' > \"$out\"\n\
             ;;\n\
         esac\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

/// Every Stage 5 `CliSaved` export tool and `get_board_2d_view` reports
/// per-domain `sources` and `source_evidence` disclosing that unsaved KiCad
/// editor changes are excluded, both offline and with a sibling lock file.
#[cfg(unix)]
#[tokio::test]
async fn stage5_cli_export_and_render_tools_disclose_saved_snapshot_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    write_project(&board);
    let sch = write_minimal_schematic(dir.path());
    let fake_cli = write_fake_export_cli(dir.path());

    let handler = McpHandler::new(ServerConfig {
        kicad_cli: fake_cli.to_string_lossy().into_owned(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: None,
        handler,
    };
    let board_str = scene.board.to_string_lossy().to_string();
    let out = scene._dir.path().join("stage5-out");
    std::fs::create_dir_all(&out).unwrap();

    // export_gerber: default layers + drill_file=true
    let gerber_default = body_of(
        &scene
            .call_with(
                "export_gerber",
                json!({
                    "board": board_str,
                    "output_dir": out.join("gerbers-default").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        gerber_default["sources"],
        json!({
            "board": "saved_board",
            "layers": "saved_board",
            "gerbers": "kicad_cli",
            "drill": "kicad_cli",
        }),
        "{gerber_default}"
    );
    assert_eq!(
        gerber_default["source_evidence"]["board_state"],
        "saved_board"
    );
    assert_eq!(
        gerber_default["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        gerber_default["source_evidence"]["reason"],
        "saved_snapshot_only"
    );

    // export_gerber: explicit layers + drill_file=false
    let gerber_explicit = body_of(
        &scene
            .call_with(
                "export_gerber",
                json!({
                    "board": board_str,
                    "output_dir": out.join("gerbers-explicit").to_string_lossy(),
                    "layers": ["F.Cu"],
                    "drill_file": false,
                }),
            )
            .await,
    );
    assert_eq!(
        gerber_explicit["sources"],
        json!({
            "board": "saved_board",
            "layers": "argument",
            "gerbers": "kicad_cli",
        }),
        "{gerber_explicit}"
    );

    // export_pdf: default vs explicit layers
    let pdf_default = body_of(
        &scene
            .call_with(
                "export_pdf",
                json!({
                    "board": board_str,
                    "output": out.join("board-default.pdf").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        pdf_default["sources"],
        json!({
            "board": "saved_board",
            "layers": "saved_board",
            "pdf": "kicad_cli",
        }),
        "{pdf_default}"
    );
    let pdf_explicit = body_of(
        &scene
            .call_with(
                "export_pdf",
                json!({
                    "board": board_str,
                    "output": out.join("board-explicit.pdf").to_string_lossy(),
                    "layers": ["F.Cu"],
                }),
            )
            .await,
    );
    assert_eq!(
        pdf_explicit["sources"]["layers"],
        json!("argument"),
        "{pdf_explicit}"
    );

    // export_svg: default vs explicit layers
    let svg_default = body_of(
        &scene
            .call_with(
                "export_svg",
                json!({
                    "board": board_str,
                    "output": out.join("board-default.svg").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        svg_default["sources"],
        json!({
            "board": "saved_board",
            "layers": "saved_board",
            "svg": "kicad_cli",
        }),
        "{svg_default}"
    );
    let svg_explicit = body_of(
        &scene
            .call_with(
                "export_svg",
                json!({
                    "board": board_str,
                    "output": out.join("board-explicit.svg").to_string_lossy(),
                    "layers": ["F.Cu"],
                }),
            )
            .await,
    );
    assert_eq!(
        svg_explicit["sources"]["layers"],
        json!("argument"),
        "{svg_explicit}"
    );

    // export_3d
    let step = body_of(
        &scene
            .call_with(
                "export_3d",
                json!({
                    "board": board_str,
                    "output": out.join("board.step").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        step["sources"],
        json!({ "board": "saved_board", "model_3d": "kicad_cli" }),
        "{step}"
    );

    // export_netlist: IPC-D-356 (board) vs KiCad (schematic)
    let net_ipc = body_of(
        &scene
            .call_with(
                "export_netlist",
                json!({
                    "board": board_str,
                    "output": out.join("board.d356").to_string_lossy(),
                    "format": "ipc",
                }),
            )
            .await,
    );
    assert_eq!(
        net_ipc["sources"],
        json!({ "board": "saved_board", "netlist": "kicad_cli" }),
        "{net_ipc}"
    );
    assert_eq!(net_ipc["source_evidence"]["board_state"], "saved_board");
    assert_eq!(net_ipc["source_evidence"]["reason"], "saved_snapshot_only");

    let net_sch = body_of(
        &scene
            .call_with(
                "export_netlist",
                json!({
                    "board": board_str,
                    "output": out.join("board.net").to_string_lossy(),
                    "format": "kicad",
                }),
            )
            .await,
    );
    assert_eq!(
        net_sch["sources"],
        json!({ "schematic": "saved_schematic", "netlist": "kicad_cli" }),
        "{net_sch}"
    );
    assert_eq!(net_sch["source_evidence"]["board_state"], "saved_schematic");
    assert_eq!(
        net_sch["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(net_sch["source_evidence"]["reason"], "saved_snapshot_only");

    // export_position_file, export_dxf, export_gencad, export_ipc2581, export_odb
    let pos = body_of(
        &scene
            .call_with(
                "export_position_file",
                json!({
                    "board": board_str,
                    "output": out.join("board.pos.csv").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        pos["sources"],
        json!({ "board": "saved_board", "positions": "kicad_cli" }),
        "{pos}"
    );

    let dxf = body_of(
        &scene
            .call_with(
                "export_dxf",
                json!({
                    "board": board_str,
                    "output_dir": out.join("dxf").to_string_lossy(),
                    "layers": ["Edge.Cuts"],
                }),
            )
            .await,
    );
    assert_eq!(
        dxf["sources"],
        json!({ "board": "saved_board", "dxf": "kicad_cli" }),
        "{dxf}"
    );

    let gencad = body_of(
        &scene
            .call_with(
                "export_gencad",
                json!({
                    "board": board_str,
                    "output": out.join("board.cad").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        gencad["sources"],
        json!({ "board": "saved_board", "gencad": "kicad_cli" }),
        "{gencad}"
    );

    let ipc2581 = body_of(
        &scene
            .call_with(
                "export_ipc2581",
                json!({
                    "board": board_str,
                    "output": out.join("board.xml").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        ipc2581["sources"],
        json!({ "board": "saved_board", "ipc2581": "kicad_cli" }),
        "{ipc2581}"
    );

    let odb = body_of(
        &scene
            .call_with(
                "export_odb",
                json!({
                    "board": board_str,
                    "output": out.join("board.zip").to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        odb["sources"],
        json!({ "board": "saved_board", "odb": "kicad_cli" }),
        "{odb}"
    );

    // get_board_2d_view returns [Text { provenance metadata }, Image { png }]
    let view_result = scene
        .call_with("get_board_2d_view", json!({ "board": board_str }))
        .await;
    assert!(!view_result.is_error, "{}", text_of(&view_result));
    assert_eq!(view_result.content.len(), 2, "{:?}", view_result.content);
    let view_meta = body_of(&view_result);
    assert_eq!(
        view_meta["sources"],
        json!({ "board": "saved_board", "render": "kicad_cli" }),
        "{view_meta}"
    );
    assert_eq!(view_meta["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        view_meta["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(
        view_meta["source_evidence"]["reason"],
        "saved_snapshot_only"
    );
    match &view_result.content[1] {
        crate::mcp::protocol::ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            assert!(!data.is_empty());
        }
        other => panic!("expected image block second, got {other:?}"),
    }

    // Sibling board lock is disclosed on every board-backed CLI export/render tool.
    let board_lock = copperpour_sexp::writer::kicad_editor_lock_path(&scene.board).unwrap();
    std::fs::write(&board_lock, "locked").unwrap();
    for (tool, args) in [
        (
            "export_gerber",
            json!({ "board": board_str, "output_dir": out.join("lock-gerber").to_string_lossy() }),
        ),
        (
            "export_pdf",
            json!({ "board": board_str, "output": out.join("lock.pdf").to_string_lossy() }),
        ),
        (
            "export_svg",
            json!({ "board": board_str, "output": out.join("lock.svg").to_string_lossy() }),
        ),
        (
            "export_3d",
            json!({ "board": board_str, "output": out.join("lock.step").to_string_lossy() }),
        ),
        (
            "export_netlist",
            json!({ "board": board_str, "output": out.join("lock.d356").to_string_lossy(), "format": "ipc" }),
        ),
        (
            "export_position_file",
            json!({ "board": board_str, "output": out.join("lock.pos.csv").to_string_lossy() }),
        ),
        (
            "export_dxf",
            json!({ "board": board_str, "output_dir": out.join("lock-dxf").to_string_lossy(), "layers": ["Edge.Cuts"] }),
        ),
        (
            "export_gencad",
            json!({ "board": board_str, "output": out.join("lock.cad").to_string_lossy() }),
        ),
        (
            "export_ipc2581",
            json!({ "board": board_str, "output": out.join("lock.xml").to_string_lossy() }),
        ),
        (
            "export_odb",
            json!({ "board": board_str, "output": out.join("lock.zip").to_string_lossy() }),
        ),
        ("get_board_2d_view", json!({ "board": board_str })),
    ] {
        let result = scene.call_with(tool, args).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        let body = body_of(&result);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("saved_snapshot_only_with_editor_lock"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }

    // Sibling schematic lock is disclosed on schematic netlist export.
    let sch_lock = copperpour_sexp::writer::kicad_editor_lock_path(&sch).unwrap();
    std::fs::write(&sch_lock, "locked").unwrap();
    let net_sch_locked = body_of(
        &scene
            .call_with(
                "export_netlist",
                json!({
                    "board": board_str,
                    "output": out.join("lock.net").to_string_lossy(),
                    "format": "kicad",
                }),
            )
            .await,
    );
    assert_eq!(
        net_sch_locked["source_evidence"]["board_state"],
        "saved_schematic"
    );
    assert_eq!(
        net_sch_locked["source_evidence"]["reason"],
        "saved_snapshot_only_with_editor_lock"
    );
}

/// After the server has observed a board live in KiCad, Stage 5 CLI export and
/// render tools disclose `saved_snapshot_only_after_live_observation`.
#[cfg(unix)]
#[tokio::test]
async fn stage5_cli_export_and_render_tools_disclose_after_live_observation() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    write_project(&board);
    let fake_cli = write_fake_export_cli(dir.path());
    let kicad = spawn_live_kicad(&board);
    let handler = McpHandler::new(ServerConfig {
        kicad_cli: fake_cli.to_string_lossy().into_owned(),
        kicad_binary: String::new(),
        ipc_address: kicad.address().to_string(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: Some(kicad),
        handler,
    };
    let board_str = scene.board.to_string_lossy().to_string();
    let out = scene._dir.path().join("stage5-live");
    std::fs::create_dir_all(&out).unwrap();

    // Observe the board live first on the same handler.
    let live = scene.body("get_layer_list", Some("live")).await;
    assert_eq!(live["source_evidence"]["board_state"], "ipc", "{live}");

    for (tool, args) in [
        (
            "export_gerber",
            json!({ "board": board_str, "output_dir": out.join("gerbers").to_string_lossy() }),
        ),
        (
            "export_pdf",
            json!({ "board": board_str, "output": out.join("board.pdf").to_string_lossy() }),
        ),
        (
            "export_netlist",
            json!({ "board": board_str, "output": out.join("board.d356").to_string_lossy(), "format": "ipc" }),
        ),
        (
            "export_position_file",
            json!({ "board": board_str, "output": out.join("board.pos.csv").to_string_lossy() }),
        ),
        ("get_board_2d_view", json!({ "board": board_str })),
    ] {
        let result = scene.call_with(tool, args).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));
        let body = body_of(&result);
        assert_eq!(
            body["source_evidence"]["reason"],
            json!("saved_snapshot_only_after_live_observation"),
            "{tool}: {body}"
        );
        assert_eq!(
            body["source_evidence"]["excludes_unsaved_editor_state"],
            json!(true),
            "{tool}: {body}"
        );
    }
}

/// `export_manufacturing_package` reports per-domain `sources` and
/// `source_evidence` alongside its existing `board_source` and
/// `project_settings_source` fields.
#[cfg(unix)]
#[tokio::test]
async fn stage5_export_manufacturing_package_discloses_mixed_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    write_project(&board);
    let sch = write_minimal_schematic(dir.path());
    let fake_cli = write_fake_export_cli(dir.path());

    let handler = McpHandler::new(ServerConfig {
        kicad_cli: fake_cli.to_string_lossy().into_owned(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: None,
        handler,
    };
    let board_str = scene.board.to_string_lossy().to_string();
    let sch_str = sch.to_string_lossy().to_string();
    let out = scene._dir.path().join("mfg-pkg");

    // Generic fab house with assembly + schematic + .kicad_pro present.
    let pkg = body_of(
        &scene
            .call_with(
                "export_manufacturing_package",
                json!({
                    "board": board_str,
                    "schematic": sch_str,
                    "output_dir": out.join("with-asm").to_string_lossy(),
                    "fab_house": "generic",
                    "include_assembly": true,
                }),
            )
            .await,
    );
    assert_eq!(pkg["board_source"], "saved_file", "{pkg}");
    assert_eq!(
        pkg["project_settings_source"], "saved_kicad_pro_if_present",
        "{pkg}"
    );
    assert_eq!(
        pkg["sources"],
        json!({
            "board": "saved_board",
            "project_settings": "project_file",
            "gerber_layers": "saved_board",
            "gerbers": "kicad_cli",
            "drill": "kicad_cli",
            "positions": "kicad_cli",
            "schematic": "saved_schematic",
            "bom": "kicad_cli",
        }),
        "{pkg}"
    );
    assert_eq!(pkg["source_evidence"]["board_state"], "saved_board");
    assert_eq!(
        pkg["source_evidence"]["excludes_unsaved_editor_state"],
        true
    );
    assert_eq!(pkg["source_evidence"]["reason"], "saved_snapshot_only");
    assert_eq!(
        pkg["source_evidence"]["schematic"]["schematic_state"], "saved_schematic",
        "{pkg}"
    );
    assert_eq!(
        pkg["source_evidence"]["schematic"]["excludes_unsaved_editor_state"], true,
        "{pkg}"
    );
    assert_eq!(
        pkg["source_evidence"]["schematic"]["reason"], "saved_snapshot_only",
        "{pkg}"
    );

    // A KiCad schematic lock is reported in per-domain `source_evidence.schematic`.
    let sch_lock = copperpour_sexp::writer::kicad_editor_lock_path(&sch).unwrap();
    std::fs::write(&sch_lock, "locked").unwrap();
    let pkg_sch_locked = body_of(
        &scene
            .call_with(
                "export_manufacturing_package",
                json!({
                    "board": board_str,
                    "schematic": sch_str,
                    "output_dir": out.join("with-asm-sch-locked").to_string_lossy(),
                    "fab_house": "generic",
                    "include_assembly": true,
                }),
            )
            .await,
    );
    assert_eq!(
        pkg_sch_locked["source_evidence"]["schematic"]["reason"],
        "saved_snapshot_only_with_editor_lock",
        "{pkg_sch_locked}"
    );
    assert_eq!(
        pkg_sch_locked["source_evidence"]["reason"], "saved_snapshot_only",
        "{pkg_sch_locked}"
    );
    std::fs::remove_file(&sch_lock).unwrap();

    // Remove .kicad_pro, pass explicit gerber_layers, disable assembly, add board lock.
    std::fs::remove_file(scene.board.with_extension("kicad_pro")).unwrap();
    let lock_path = copperpour_sexp::writer::kicad_editor_lock_path(&scene.board).unwrap();
    std::fs::write(&lock_path, "locked").unwrap();
    let pkg_fab_only = body_of(
        &scene
            .call_with(
                "export_manufacturing_package",
                json!({
                    "board": board_str,
                    "output_dir": out.join("fab-only").to_string_lossy(),
                    "fab_house": "jlcpcb",
                    "include_assembly": false,
                    "schematic": sch_str,
                    "gerber_layers": ["F.Cu", "Edge.Cuts"],
                }),
            )
            .await,
    );
    assert_eq!(
        pkg_fab_only["sources"],
        json!({
            "board": "saved_board",
            "project_settings": "unavailable",
            "gerber_layers": "argument",
            "gerbers": "kicad_cli",
            "drill": "kicad_cli",
        }),
        "{pkg_fab_only}"
    );
    assert_eq!(
        pkg_fab_only["source_evidence"]["reason"],
        "saved_snapshot_only_with_editor_lock"
    );
    assert!(
        pkg_fab_only["source_evidence"].get("schematic").is_none(),
        "fabrication-only export must not attach schematic evidence: {pkg_fab_only}"
    );

    // Assembly enabled without a schematic reports schematic and bom as unavailable.
    let pkg_no_sch = body_of(
        &scene
            .call_with(
                "export_manufacturing_package",
                json!({
                    "board": board_str,
                    "output_dir": out.join("asm-no-sch").to_string_lossy(),
                    "fab_house": "generic",
                    "include_assembly": true,
                }),
            )
            .await,
    );
    assert_eq!(
        pkg_no_sch["sources"]["schematic"],
        json!("unavailable"),
        "{pkg_no_sch}"
    );
    assert_eq!(
        pkg_no_sch["sources"]["bom"],
        json!("unavailable"),
        "{pkg_no_sch}"
    );
    assert_eq!(
        pkg_no_sch["sources"]["positions"],
        json!("kicad_cli"),
        "{pkg_no_sch}"
    );
    assert!(
        pkg_no_sch["source_evidence"].get("schematic").is_none(),
        "assembly without schematic must not attach schematic evidence: {pkg_no_sch}"
    );
}

/// `run_design_review` discloses per-domain `source_evidence.schematic`
/// including any KiCad schematic editor lock, both with and without `board`.
#[tokio::test]
async fn stage6_run_design_review_discloses_schematic_lock_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    let sch = write_minimal_schematic(dir.path());
    let sch_lock = copperpour_sexp::writer::kicad_editor_lock_path(&sch).unwrap();
    std::fs::write(&sch_lock, "locked").unwrap();

    let handler = McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: None,
        handler,
    };

    let sch_only = body_of(
        &scene
            .call_with(
                "run_design_review",
                json!({ "schematic": sch.to_string_lossy() }),
            )
            .await,
    );
    assert_eq!(
        sch_only["source_evidence"]["schematic"]["schematic_state"], "saved_schematic",
        "{sch_only}"
    );
    assert_eq!(
        sch_only["source_evidence"]["schematic"]["excludes_unsaved_editor_state"], true,
        "{sch_only}"
    );
    assert_eq!(
        sch_only["source_evidence"]["schematic"]["reason"], "saved_snapshot_only_with_editor_lock",
        "{sch_only}"
    );

    let with_board = body_of(
        &scene
            .call_with(
                "run_design_review",
                json!({
                    "schematic": sch.to_string_lossy(),
                    "board": scene.board.to_string_lossy(),
                }),
            )
            .await,
    );
    assert_eq!(
        with_board["source_evidence"]["board_state"], "saved_board",
        "{with_board}"
    );
    assert_eq!(
        with_board["source_evidence"]["reason"], "saved_snapshot_only",
        "{with_board}"
    );
    assert_eq!(
        with_board["source_evidence"]["schematic"]["reason"],
        "saved_snapshot_only_with_editor_lock",
        "{with_board}"
    );
}

/// A package whose kicad-cli exports fail names those domains unavailable,
/// not `kicad_cli`.
#[tokio::test]
async fn stage5_export_manufacturing_package_reports_failed_exports_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let board = fixture_board(dir.path());
    let failing_cli = crate::tools::cli::test_support::write_script(
        dir.path(),
        "failing-cli",
        "#!/bin/sh\nexit 1\n",
        "@exit /b 1\r\n",
    );
    let handler = McpHandler::new(ServerConfig {
        kicad_cli: failing_cli.to_string_lossy().into_owned(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds");
    let scene = Scene {
        _dir: dir,
        board,
        kicad: None,
        handler,
    };
    let pkg = body_of(
        &scene
            .call_with(
                "export_manufacturing_package",
                json!({
                    "board": scene.board.to_string_lossy(),
                    "output_dir": scene._dir.path().join("failed").to_string_lossy(),
                    "fab_house": "generic",
                    "include_assembly": false,
                }),
            )
            .await,
    );
    assert_eq!(pkg["complete"], false, "{pkg}");
    assert_eq!(
        pkg["sources"],
        json!({
            "board": "saved_board",
            "project_settings": "unavailable",
            "gerber_layers": "saved_board",
            "gerbers": "unavailable",
            "drill": "unavailable",
        }),
        "{pkg}"
    );
}
