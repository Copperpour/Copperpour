//! `modify_trace` changes the track in place, in one commit, and a refused
//! edit leaves it on the board (#366). It used to delete the track, then
//! create its replacement, so a refused create lost the track.
//!
//! Each case runs through served `tools/call` against a KiCad double holding
//! the six-layer board KiCad 10 saved (copper F.Cu, In1.Cu..In4.Cu, B.Cu; see
//! `tests/fixtures/six_layer_power_planes_kicad10.README.md`), with one B.Cu
//! track on it.

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::CallToolResult;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
use crate::tools::ServerConfig;
use copperpour_ipc::builders::{distance, net, pack_any, vec2};
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::gen::kiapi::board::types::{BoardLayer, Track};
use prost::Message;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

const TRACK: &str = "00000000-0000-0000-0000-000000000366";

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

/// How the double answers `UpdateItems`.
#[derive(Clone, Copy)]
enum Update {
    Apply,
    /// What KiCad answers for an item it will not take.
    Refuse,
    /// Report success but keep the track as it was.
    Ignore,
}

struct Scene {
    _dir: tempfile::TempDir,
    board: String,
    _kicad: MockIpcServer,
    store: Arc<Mutex<ItemStore>>,
    /// The commit commands the double received, with an `EndCommit`'s action.
    commits: Arc<Mutex<Vec<String>>>,
    handler: McpHandler,
}

fn original_track() -> Track {
    Track {
        id: Some(kiapi::common::types::Kiid {
            value: TRACK.to_string(),
        }),
        start: Some(vec2(1.0, 1.0)),
        end: Some(vec2(4.0, 1.0)),
        width: Some(distance(0.2)),
        locked: kiapi::common::types::LockedState::LsLocked as i32,
        layer: BoardLayer::BlBCu as i32,
        net: Some(net("GND", 1)),
        parent: None,
        solder_mask: None,
    }
}

impl Scene {
    async fn new(update: Update) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("six_layer.kicad_pcb");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/six_layer_power_planes_kicad10.kicad_pcb"),
            &board,
        )
        .unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: vec![pack_any(&original_track(), "kiapi.board.types.Track")],
            ..ItemStore::default()
        }));
        let commits = Arc::new(Mutex::new(Vec::new()));
        let kicad = spawn_kicad_holding_items(
            &board,
            vec!["", "GND"],
            store.clone(),
            |_| {},
            answer_what_the_store_does_not(update, commits.clone()),
        );
        let handler = McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: kicad.address().to_string(),
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
            _kicad: kicad,
            store,
            commits,
            handler,
        }
    }

    async fn call(&self, changes: Value) -> CallToolResult {
        let mut arguments = json!({
            "board": self.board, "uuid": TRACK, "net_name": "GND", "layer": "In4.Cu",
            "x1": 5.0, "y1": 5.0, "x2": 12.0, "y2": 5.0, "width": 0.3
        });
        for (key, value) in changes.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        let response = self
            .handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 366, "method": "tools/call",
                "params": { "name": "modify_trace", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    fn track(&self) -> Track {
        let store = self.store.lock().unwrap();
        assert_eq!(store.items.len(), 1, "the board holds one item");
        Track::decode(store.items[0].value.as_slice()).unwrap()
    }

    /// The commit commands, and whether any item was created, updated or
    /// deleted.
    fn writes(&self) -> (Vec<String>, usize, usize, usize) {
        let store = self.store.lock().unwrap();
        (
            self.commits.lock().unwrap().clone(),
            store.creates,
            store.updates,
            store.deletes,
        )
    }
}

/// The commands `spawn_kicad_holding_items` does not answer as KiCad 10.0.6
/// does, or at all; `None` leaves a command to the store.
fn answer_what_the_store_does_not(
    update: Update,
    commits: Arc<Mutex<Vec<String>>>,
) -> impl Fn(&prost_types::Any, &mut ItemStore) -> Option<prost_types::Any> + Send + 'static {
    use kiapi::common::commands as cmd;
    let ok = kiapi::common::types::ItemRequestStatus::IrsOk as i32;
    move |command, store| {
        let name = command.type_url.rsplit('.').next().unwrap();
        let bytes = command.value.as_slice();
        match name {
            "GetItemsById" => {
                let asked = cmd::GetItemsById::decode(bytes).unwrap();
                let items = store
                    .items
                    .iter()
                    .map(|item| Track::decode(item.value.as_slice()).unwrap())
                    .filter(|track| asked.items.iter().any(|id| Some(id) == track.id.as_ref()))
                    .map(|mut track| {
                        // KiCad 10.0.6 names a track's net without its code.
                        if let Some(net) = track.net.as_mut() {
                            net.code = None;
                        }
                        pack_any(&track, "kiapi.board.types.Track")
                    })
                    .collect();
                Some(pack_any(
                    &cmd::GetItemsResponse {
                        header: None,
                        status: ok,
                        items,
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ))
            }
            "GetBoardEnabledLayers" => Some(pack_any(
                &kiapi::board::commands::BoardEnabledLayersResponse {
                    copper_layer_count: 6,
                    layers: ENABLED.iter().map(|&l| l as i32).collect(),
                },
                "kiapi.board.commands.BoardEnabledLayersResponse",
            )),
            "BeginCommit" => {
                commits.lock().unwrap().push(name.to_string());
                None
            }
            "EndCommit" => {
                let action = cmd::EndCommit::decode(bytes).unwrap().action();
                commits
                    .lock()
                    .unwrap()
                    .push(format!("EndCommit {}", action.as_str_name()));
                None
            }
            "UpdateItems" => {
                let code = match update {
                    Update::Apply => return None,
                    Update::Refuse => cmd::ItemStatusCode::IscInvalidData,
                    Update::Ignore => cmd::ItemStatusCode::IscOk,
                };
                store.updates += 1;
                let request = cmd::UpdateItems::decode(bytes).unwrap();
                Some(pack_any(
                    &cmd::UpdateItemsResponse {
                        header: None,
                        status: ok,
                        updated_items: request
                            .items
                            .into_iter()
                            .map(|item| cmd::ItemUpdateResult {
                                status: Some(cmd::ItemStatus {
                                    code: code as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(item),
                            })
                            .collect(),
                    },
                    "kiapi.common.commands.UpdateItemsResponse",
                ))
            }
            _ => None,
        }
    }
}

fn body(result: &CallToolResult) -> Value {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => serde_json::from_str(text)
            .unwrap_or_else(|_| panic!("expected a JSON body, got {text}")),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn modify_trace_updates_the_track_in_place_in_one_commit() {
    let scene = Scene::new(Update::Apply).await;

    let result = scene.call(json!({})).await;

    assert!(!result.is_error, "{:?}", result.content);
    let answer = body(&result);
    assert_eq!(answer["modified_uuid"], TRACK);
    assert_eq!(answer["layer"], "In4.Cu");
    assert_eq!(
        scene.writes(),
        (
            vec!["BeginCommit".into(), "EndCommit CMA_COMMIT".into()],
            0,
            1,
            0
        )
    );
    let after = scene.track();
    assert_eq!(after.id, original_track().id, "the track keeps its UUID");
    assert_eq!(after.layer, BoardLayer::BlIn4Cu as i32);
    assert_eq!(after.start, Some(vec2(5.0, 5.0)));
    assert_eq!(after.end, Some(vec2(12.0, 5.0)));
    assert_eq!(after.width, Some(distance(0.3)));
    assert_eq!(after.net, Some(net("GND", 1)));
    assert_eq!(
        after.locked,
        original_track().locked,
        "untouched fields stay"
    );
}

/// The empty name is KiCad's unconnected net, code 0.
#[tokio::test]
async fn modify_trace_can_move_a_track_to_no_net() {
    let scene = Scene::new(Update::Apply).await;

    let result = scene.call(json!({ "net_name": "" })).await;

    assert!(!result.is_error, "{:?}", result.content);
    assert_eq!(scene.track().net, Some(net("", 0)));
}

/// What the issue lists as failing after the delete: a copper layer the board
/// does not have, a net it does not have, and a UUID that is no track; and a
/// width KiCad would store as a degenerate track. Each is refused before
/// KiCad is asked to change anything.
#[tokio::test]
async fn modify_trace_refuses_before_changing_the_board() {
    let cases = [
        (
            json!({ "layer": "In5.Cu" }),
            "invalid_argument",
            Some("layer"),
        ),
        (
            json!({ "layer": "In30.Cu" }),
            "invalid_argument",
            Some("layer"),
        ),
        (
            json!({ "net_name": "NOPE" }),
            "invalid_argument",
            Some("net_name"),
        ),
        (json!({ "width": 0.0 }), "invalid_argument", Some("width")),
        (json!({ "width": -0.2 }), "invalid_argument", Some("width")),
        (
            json!({ "uuid": "00000000-0000-0000-0000-000000000000" }),
            "stale_target",
            None,
        ),
    ];
    for (changes, kind, field) in cases {
        let scene = Scene::new(Update::Apply).await;

        let result = scene.call(changes.clone()).await;

        let case = changes.to_string();
        assert!(result.is_error, "{case}: {:?}", result.content);
        let answer = body(&result);
        let error = &answer["error"];
        assert_eq!(error["kind"], kind, "{case}: {error}");
        if let Some(field) = field {
            assert_eq!(error["field"], field, "{case}");
        }
        // A refusal on what the board holds says the board is unchanged; a
        // bad `width` is refused before KiCad is asked anything.
        let message = answer["message"].as_str().unwrap();
        if field != Some("width") {
            assert!(message.contains("unchanged"), "{case}: {message}");
        }
        assert_eq!(scene.writes(), (Vec::new(), 0, 0, 0), "{case}");
        assert_eq!(scene.track(), original_track(), "{case}");
    }
}

/// KiCad refuses the update itself: the commit is dropped and the track,
/// which was never deleted, is still there.
#[tokio::test]
async fn a_refused_update_drops_the_commit_and_keeps_the_track() {
    let scene = Scene::new(Update::Refuse).await;

    let result = scene.call(json!({})).await;

    assert!(result.is_error, "{:?}", result.content);
    assert!(
        format!("{:?}", result.content).contains("ISC_INVALID_DATA"),
        "{:?}",
        result.content
    );
    assert_eq!(
        scene.writes(),
        (
            vec!["BeginCommit".into(), "EndCommit CMA_DROP".into()],
            0,
            1,
            0
        )
    );
    assert_eq!(scene.track(), original_track());
}

/// KiCad accepts the update, but the track reads back unchanged. The saved
/// board was never touched, so this is a live outcome, not a file one.
#[tokio::test]
async fn an_update_that_does_not_read_back_is_uncertain() {
    let scene = Scene::new(Update::Ignore).await;

    let result = scene.call(json!({})).await;

    assert!(result.is_error, "{:?}", result.content);
    let error = &body(&result)["error"];
    assert_eq!(error["kind"], "ipc_outcome_unknown", "{error}");
    assert_eq!(error["retry_safe"], false);
    let reason = error["reason"].as_str().unwrap();
    assert!(
        reason.contains("committed the modify_trace update") && reason.contains(TRACK),
        "{reason}"
    );
}
