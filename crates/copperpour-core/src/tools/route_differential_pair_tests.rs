//! `route_differential_pair` sends both traces in one `CreateItems` inside one
//! commit, and refuses a pair of one net (#367). It sent one `CreateItems` per
//! trace with no commit around them, so a refused negative trace left the
//! positive one on the board, and a routed pair was two undo steps.
//!
//! Each case runs through served `tools/call` against a KiCad double holding
//! the six-layer board KiCad 10 saved (see
//! `tests/fixtures/six_layer_power_planes_kicad10.README.md`). It answers
//! `CreateItems` as KiCad 10.0.6's `handleCreateUpdateItemsInternal`
//! (`pcbnew/api/api_handler_pcb.cpp`) does: an item it refuses gets that status
//! and is skipped, the rest join the client's commit, and a request outside
//! `BeginCommit` pushes its own commit when it ends. A dropped commit's items
//! never reach the board.

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::CallToolResult;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::board_document;
use crate::tools::ServerConfig;
use copperpour_ipc::builders::{distance, net, pack_any, vec2};
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::gen::kiapi::board::types::{BoardLayer, Track};
use prost::Message;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The six-layer fixture's layers, as KiCad 10.0.6 lists them enabled.
const ENABLED: [BoardLayer; 7] = [
    BoardLayer::BlFCu,
    BoardLayer::BlIn1Cu,
    BoardLayer::BlIn2Cu,
    BoardLayer::BlIn3Cu,
    BoardLayer::BlIn4Cu,
    BoardLayer::BlBCu,
    BoardLayer::BlEdgeCuts,
];

/// How the double answers.
#[derive(Clone, Copy, PartialEq)]
enum Kicad {
    Accepts,
    /// Refuses the `USB_DM` trace, as KiCad refuses an item it will not take.
    RefusesNegative,
    /// As `RefusesNegative`, then answers the drop with KiCad's busy refusal
    /// (`checkForBusy` in `common/api/api_handler_editor.cpp`).
    RefusesNegativeThenTheDrop,
    /// Returns an unrelated but decodable empty protobuf instead of a drop acknowledgement.
    RefusesNegativeThenWrongDropReply,
}

#[derive(Default)]
struct Board {
    /// The traces on the board.
    tracks: Vec<Track>,
    /// The traces created inside the open commit, while one is open.
    staged: Option<Vec<Track>>,
    /// The commit and create commands received, in order.
    sent: Vec<String>,
    /// Every command received.
    asked: usize,
    next_id: usize,
}

struct Scene {
    _dir: tempfile::TempDir,
    board: String,
    _kicad: MockIpcServer,
    held: Arc<Mutex<Board>>,
    handler: McpHandler,
}

impl Scene {
    async fn new(kicad: Kicad) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("six_layer.kicad_pcb");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/six_layer_power_planes_kicad10.kicad_pcb"),
            &board,
        )
        .unwrap();
        let held = Arc::new(Mutex::new(Board::default()));
        let double = kicad_holding(&board, kicad, held.clone());
        let handler = McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: double.address().to_string(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: false,
        })
        .await
        .expect("handler builds");
        Self {
            board: board.to_string_lossy().into_owned(),
            _dir: dir,
            _kicad: double,
            held,
            handler,
        }
    }

    async fn call(&self, changes: Value) -> CallToolResult {
        let mut arguments = json!({
            "board": self.board, "net_pos": "USB_DP", "net_neg": "USB_DM",
            "x1": 10.0, "y1": 10.0, "x2": 20.0, "y2": 10.0
        });
        for (key, value) in changes.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        let response = self
            .handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 367, "method": "tools/call",
                "params": { "name": "route_differential_pair", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    fn sent(&self) -> Vec<String> {
        self.held.lock().unwrap().sent.clone()
    }

    fn tracks(&self) -> Vec<Track> {
        self.held.lock().unwrap().tracks.clone()
    }
}

/// A KiCad holding `board` with the nets `USB_DP` and `USB_DM`.
fn kicad_holding(board: &Path, kicad: Kicad, held: Arc<Mutex<Board>>) -> MockIpcServer {
    use kiapi::common::commands as cmd;
    use kiapi::common::{ApiResponse, ApiResponseStatus, ApiStatusCode};
    let documents = vec![board_document(&board.to_string_lossy())];
    let reply = |status: ApiStatusCode, error: &str, message| ApiResponse {
        status: Some(ApiResponseStatus {
            status: status as i32,
            error_message: error.to_string(),
        }),
        header: None,
        message,
    };
    MockIpcServer::spawn("route-differential-pair", move |request| {
        let command = request.message.expect("a command");
        let name = command.type_url.rsplit('.').next().unwrap();
        let bytes = command.value.as_slice();
        let mut held = held.lock().unwrap();
        held.asked += 1;
        let message = match name {
            "GetOpenDocuments" => pack_any(
                &cmd::GetOpenDocumentsResponse {
                    documents: documents.clone(),
                },
                "kiapi.common.commands.GetOpenDocumentsResponse",
            ),
            "GetNets" => pack_any(
                &kiapi::board::commands::NetsResponse {
                    nets: vec![net("", 0), net("USB_DP", 1), net("USB_DM", 2)],
                },
                "kiapi.board.commands.NetsResponse",
            ),
            "GetBoardEnabledLayers" => pack_any(
                &kiapi::board::commands::BoardEnabledLayersResponse {
                    copper_layer_count: 6,
                    layers: ENABLED.iter().map(|&layer| layer as i32).collect(),
                },
                "kiapi.board.commands.BoardEnabledLayersResponse",
            ),
            "BeginCommit" => {
                held.sent.push(name.to_string());
                held.staged = Some(Vec::new());
                pack_any(
                    &cmd::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "pair-commit".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                )
            }
            "CreateItems" => {
                let request = cmd::CreateItems::decode(bytes).unwrap();
                held.sent
                    .push(format!("CreateItems {}", request.items.len()));
                let mut created = Vec::new();
                let mut results = Vec::new();
                for item in request.items {
                    let mut track = Track::decode(item.value.as_slice()).unwrap();
                    let negative = track.net.as_ref().is_some_and(|n| n.name == "USB_DM");
                    if negative && kicad != Kicad::Accepts {
                        results.push(cmd::ItemCreationResult {
                            status: Some(cmd::ItemStatus {
                                code: cmd::ItemStatusCode::IscInvalidData as i32,
                                error_message: "the double refuses the negative trace".into(),
                            }),
                            item: Some(item),
                        });
                        continue;
                    }
                    held.next_id += 1;
                    track.id = Some(kiapi::common::types::Kiid {
                        value: format!("created-{}", held.next_id),
                    });
                    results.push(cmd::ItemCreationResult {
                        status: Some(cmd::ItemStatus {
                            code: cmd::ItemStatusCode::IscOk as i32,
                            error_message: String::new(),
                        }),
                        item: Some(pack_any(&track, "kiapi.board.types.Track")),
                    });
                    created.push(track);
                }
                match held.staged.as_mut() {
                    Some(staged) => staged.extend(created),
                    None => held.tracks.extend(created),
                }
                pack_any(
                    &cmd::CreateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        created_items: results,
                    },
                    "kiapi.common.commands.CreateItemsResponse",
                )
            }
            "EndCommit" => {
                let action = cmd::EndCommit::decode(bytes).unwrap().action();
                held.sent
                    .push(format!("EndCommit {}", action.as_str_name()));
                if action == cmd::CommitAction::CmaDrop
                    && kicad == Kicad::RefusesNegativeThenTheDrop
                {
                    return reply(
                        ApiStatusCode::AsBusy,
                        "KiCad is busy and cannot respond to API requests right now",
                        None,
                    );
                }
                if action == cmd::CommitAction::CmaDrop
                    && kicad == Kicad::RefusesNegativeThenWrongDropReply
                {
                    return reply(
                        ApiStatusCode::AsOk,
                        "",
                        Some(prost_types::Any {
                            type_url:
                                "type.googleapis.com/kiapi.common.commands.SaveDocumentResponse"
                                    .into(),
                            value: vec![],
                        }),
                    );
                }
                let staged = held.staged.take().unwrap_or_default();
                if action == cmd::CommitAction::CmaCommit {
                    held.tracks.extend(staged);
                }
                pack_any(
                    &cmd::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                )
            }
            _ => return reply(ApiStatusCode::AsOk, "", None),
        };
        reply(ApiStatusCode::AsOk, "", Some(message))
    })
}

fn body(result: &CallToolResult) -> Value {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => serde_json::from_str(text)
            .unwrap_or_else(|_| panic!("expected a JSON body, got {text}")),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn a_pair_is_one_create_in_one_commit() {
    let scene = Scene::new(Kicad::Accepts).await;

    let result = scene.call(json!({})).await;

    assert!(!result.is_error, "{:?}", result.content);
    assert_eq!(
        scene.sent(),
        ["BeginCommit", "CreateItems 2", "EndCommit CMA_COMMIT"]
    );
    // The defaults, width 0.1 and gap 0.1, put each trace 0.1 mm off the line.
    let tracks = scene.tracks();
    let sides: Vec<_> = tracks
        .iter()
        .map(|track| (track.net.clone().unwrap().name, track.start, track.end))
        .collect();
    assert_eq!(
        sides,
        [
            (
                "USB_DP".to_string(),
                Some(vec2(10.0, 10.1)),
                Some(vec2(20.0, 10.1))
            ),
            (
                "USB_DM".to_string(),
                Some(vec2(10.0, 9.9)),
                Some(vec2(20.0, 9.9))
            ),
        ]
    );
    for track in &tracks {
        assert_eq!(track.layer, BoardLayer::BlFCu as i32);
        assert_eq!(track.width, Some(distance(0.1)));
    }
}

/// KiCad refuses the negative trace: the commit is dropped, so the positive
/// trace it accepted never reaches the board.
#[tokio::test]
async fn a_refused_negative_trace_drops_the_commit_and_leaves_no_trace() {
    let scene = Scene::new(Kicad::RefusesNegative).await;

    let result = scene.call(json!({})).await;

    assert!(result.is_error, "{:?}", result.content);
    assert!(scene.tracks().is_empty(), "{:?}", scene.tracks());
    assert_eq!(
        scene.sent(),
        ["BeginCommit", "CreateItems 2", "EndCommit CMA_DROP"]
    );
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("ISC_INVALID_DATA") && text.contains("changes dropped"),
        "{text}"
    );
    assert!(!text.contains("KiCAD must be running"), "{text}");
}

/// KiCad refuses the negative trace and then the drop: the positive trace may
/// still be in the open commit, so the outcome is unknown.
#[tokio::test]
async fn a_drop_kicad_refuses_is_ipc_outcome_unknown() {
    let scene = Scene::new(Kicad::RefusesNegativeThenTheDrop).await;

    let result = scene.call(json!({})).await;

    assert!(result.is_error, "{:?}", result.content);
    let error = &body(&result)["error"];
    assert_eq!(error["kind"], "ipc_outcome_unknown", "{error}");
    assert_eq!(error["retry_safe"], false, "{error}");
    let reason = error["reason"].as_str().unwrap();
    assert!(
        reason.contains("rollback also failed") && reason.contains("busy"),
        "{reason}"
    );
    assert_eq!(
        scene.sent(),
        ["BeginCommit", "CreateItems 2", "EndCommit CMA_DROP"]
    );
}

#[tokio::test]
async fn differential_pair_retains_item_evidence_independently_of_drop_outcome() {
    for (kicad, rollback, outcome, cause) in [
        (
            Kicad::RefusesNegative,
            "confirmed",
            "unknown",
            "kicad_api_rejection",
        ),
        (
            Kicad::RefusesNegativeThenTheDrop,
            "unconfirmed",
            "unknown",
            "ipc_outcome_unknown",
        ),
        (
            Kicad::RefusesNegativeThenWrongDropReply,
            "unconfirmed",
            "unknown",
            "ipc_outcome_unknown",
        ),
    ] {
        let scene = Scene::new(kicad).await;
        let before = std::fs::read(&scene.board).unwrap();
        let response = scene.call(json!({})).await;
        assert!(response.is_error);
        let response = body(&response);
        assert!(response.get("zone_refill").is_none(), "{response}");
        let recovery = &response["recovery"];
        let evidence = &recovery["evidence"];
        assert_eq!(recovery["cause"], cause, "{response}");
        assert_eq!(recovery["mutation_outcome"], outcome);
        assert_eq!(recovery["retry_safe"], false);
        assert_eq!(evidence["rollback"], rollback);
        assert_eq!(evidence["command"], "CreateItems");
        assert_eq!(evidence["requested_count"], 2);
        assert_eq!(evidence["reported_created_count"], 1);
        assert_eq!(
            evidence["rejected_items"],
            json!([{
            "reply_index": 1, "status": "ISC_INVALID_DATA",
                "error_message": "the double refuses the negative trace",
            }])
        );
        let held = scene.held.lock().unwrap();
        assert_eq!(
            held.sent,
            ["BeginCommit", "CreateItems 2", "EndCommit CMA_DROP"]
        );
        assert!(held.tracks.is_empty());
        if rollback == "confirmed" {
            assert_eq!(evidence["commit_id"], "pair-commit");
            assert!(held.staged.is_none());
        } else {
            assert_eq!(response["error"]["kind"], "ipc_outcome_unknown");
            assert_eq!(evidence["reason"], response["error"]["reason"]);
            let staged = held.staged.as_ref().unwrap();
            assert_eq!(staged.len(), 1);
            assert_eq!(staged[0].net.as_ref().unwrap().name, "USB_DP");
        }
        assert_eq!(std::fs::read(&scene.board).unwrap(), before);
    }
}

/// The refusal comes before the board is bound: KiCad is asked nothing.
#[tokio::test]
async fn a_pair_of_one_net_is_refused() {
    let scene = Scene::new(Kicad::Accepts).await;

    let result = scene.call(json!({ "net_neg": "USB_DP" })).await;

    assert!(result.is_error, "{:?}", result.content);
    let answer = body(&result);
    assert_eq!(answer["error"]["kind"], "invalid_argument", "{answer}");
    assert_eq!(answer["error"]["field"], "net_neg", "{answer}");
    let message = answer["message"].as_str().unwrap();
    assert!(
        message.contains("USB_DP") && message.contains("unchanged"),
        "{message}"
    );
    assert_eq!(scene.held.lock().unwrap().asked, 0);
    assert!(scene.tracks().is_empty());
}
