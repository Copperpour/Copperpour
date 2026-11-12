//! Caller-selected nets/layers are checked on the bound live board before creation (#421).
use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::pcb_board::board_mock::{
    board_document, spawn_kicad_holding_board, spawn_kicad_holding_items, ItemStore,
};
use copperpour_ipc::{builders::pack_any, gen::kiapi};
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum NetReply {
    Normal,
    Malformed,
    Empty,
    Missing,
}

fn metadata_reply(
    command: &prost_types::Any,
    document: &kiapi::common::types::DocumentSpecifier,
    net_reply: NetReply,
    empty_layers: bool,
) -> Option<prost_types::Any> {
    let bytes = command.value.as_slice();
    let target = if command.type_url.ends_with("GetNets") {
        kiapi::board::commands::GetNets::decode(bytes)
            .unwrap()
            .board
    } else if command.type_url.ends_with("GetBoardEnabledLayers") {
        kiapi::board::commands::GetBoardEnabledLayers::decode(bytes)
            .unwrap()
            .board
    } else if command.type_url.ends_with("CreateItems") {
        kiapi::common::commands::CreateItems::decode(bytes)
            .unwrap()
            .header
            .and_then(|h| h.document)
    } else {
        return None;
    };
    assert_eq!(
        target,
        Some(document.clone()),
        "{} names the bound board",
        command.type_url
    );
    if command.type_url.ends_with("GetNets") && matches!(net_reply, NetReply::Malformed) {
        return Some(prost_types::Any {
            type_url: "malformed.net.evidence".into(),
            value: vec![0xff],
        });
    }
    if command.type_url.ends_with("GetNets") && matches!(net_reply, NetReply::Empty) {
        return Some(pack_any(
            &kiapi::board::commands::NetsResponse { nets: vec![] },
            "kiapi.board.commands.NetsResponse",
        ));
    }
    if command.type_url.ends_with("GetBoardEnabledLayers") {
        use kiapi::board::types::BoardLayer::*;
        // KiCad's six-layer fixture: In5.Cu is syntactically copper but disabled.
        return Some(pack_any(
            &kiapi::board::commands::BoardEnabledLayersResponse {
                copper_layer_count: if empty_layers { 0 } else { 6 },
                layers: if empty_layers {
                    vec![]
                } else {
                    vec![
                        BlFCu as i32,
                        BlIn1Cu as i32,
                        BlIn2Cu as i32,
                        BlIn3Cu as i32,
                        BlIn4Cu as i32,
                        BlBCu as i32,
                        BlEdgeCuts as i32,
                    ]
                },
            },
            "kiapi.board.commands.BoardEnabledLayersResponse",
        ));
    }
    None
}

struct Scene {
    _dir: tempfile::TempDir,
    board: std::path::PathBuf,
    _server: crate::test_support::MockIpcServer,
    store: Arc<Mutex<ItemStore>>,
    handler: McpHandler,
}
impl Scene {
    async fn new() -> Self {
        Self::with_evidence(NetReply::Normal, false).await
    }
    async fn with_evidence(net_reply: NetReply, empty_layers: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("six_layer.kicad_pcb");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/six_layer_power_planes_kicad10.kicad_pcb"),
            &board,
        )
        .unwrap();
        // Unsaved live pads: the existing #700 oracle, independent of saved-file coordinates.
        let store = Arc::new(Mutex::new(ItemStore {
            items: super::route_pad_to_pad_tests::moved_r1(),
            ..Default::default()
        }));
        let document = board_document(&board.to_string_lossy());
        let server = if matches!(net_reply, NetReply::Missing) {
            let recorded = store.clone();
            spawn_kicad_holding_board(&board, move |command| {
                let mut store = recorded.lock().unwrap();
                if command.type_url.ends_with("CreateItems") {
                    store.creates += 1;
                } else if command.type_url.ends_with("UpdateItems") {
                    store.updates += 1;
                } else if command.type_url.ends_with("DeleteItems") {
                    store.deletes += 1;
                } else if command.type_url.ends_with("GetItemsById") {
                    let query =
                        kiapi::common::commands::GetItemsById::decode(command.value.as_slice())
                            .unwrap();
                    assert_eq!(query.header.unwrap().document, Some(document.clone()));
                    assert_eq!(query.items.len(), 1);
                    assert_eq!(query.items[0].value, "net-reader-track");
                    return Some(pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items: store
                                .items
                                .iter()
                                .filter(|item| item.type_url.ends_with("Track"))
                                .cloned()
                                .collect(),
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ));
                }
                // Unlike the item-store helper, this double can return AS_OK with no body.
                metadata_reply(command, &document, net_reply, empty_layers)
            })
        } else {
            spawn_kicad_holding_items(
                &board,
                vec!["", "VCC", "GND"],
                store.clone(),
                |_| {},
                move |command, _| metadata_reply(command, &document, net_reply, empty_layers),
            )
        };
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            board,
            _server: server,
            store,
            handler,
        }
    }
    async fn call(&self, tool: &str, changes: Value) -> Value {
        let mut arguments = json!({"board":self.board, "net_name":"VCC", "layer":"In4.Cu", "x1":10.0,"y1":10.0,"x2":20.0,"y2":10.0});
        if matches!(tool, "get_nets_list" | "get_netclasses" | "apply_copper") {
            arguments = json!({"board":self.board});
        } else if tool == "route_differential_pair" {
            arguments.as_object_mut().unwrap().remove("net_name");
            arguments["net_pos"] = json!("VCC");
            arguments["net_neg"] = json!("GND");
        } else if tool == "route_pad_to_pad" {
            for key in ["x1", "y1", "x2", "y2"] {
                arguments.as_object_mut().unwrap().remove(key);
            }
            for (key, value) in [("ref1", "R1"), ("pad1", "1"), ("ref2", "R2"), ("pad2", "2")] {
                arguments[key] = json!(value);
            }
        } else if tool == "add_via" {
            for key in ["x1", "y1", "x2", "y2", "layer"] {
                arguments.as_object_mut().unwrap().remove(key);
            }
            arguments["x"] = json!(15.0);
            arguments["y"] = json!(15.0);
        }
        for (key, value) in changes.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        let response = self.handler.handle_message(json!({"jsonrpc":"2.0","id":421,"method":"tools/call","params":{"name":tool,"arguments":arguments}})).await.unwrap();
        let response = response.result.unwrap();
        let body: Value = serde_json::from_str(response["content"][0]["text"].as_str().unwrap())
            .unwrap_or_else(|_| json!({"message":response["content"][0]["text"]}));
        if tool == "get_nets_list" {
            assert_eq!(
                response["isError"],
                body.get("error").is_some(),
                "{response}"
            );
        }
        if matches!(tool, "modify_trace" | "apply_copper") {
            assert_eq!(response["isError"], true, "{response}");
        }
        body
    }
}
async fn refused(tool: &str, changes: Value, field: &str, value: &str) {
    let scene = Scene::new().await;
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let body = scene.call(tool, changes).await;
    assert_eq!(body["error"]["kind"], "invalid_argument", "{tool}: {body}");
    assert_eq!(body["error"]["field"], field, "{tool}: {body}");
    assert!(
        body["message"].as_str().unwrap().contains(value),
        "{tool}: {body}"
    );
    let store = scene.store.lock().unwrap();
    assert_eq!(
        (store.creates, store.updates, store.deletes),
        (0, 0, 0),
        "{tool}"
    );
    assert_eq!(store.items, items, "{tool}");
    assert_eq!(std::fs::read(&scene.board).unwrap(), before, "{tool}");
}
macro_rules! refusal_case {
    ($name:ident,$tool:literal,$field:literal,$value:literal) => {
        #[tokio::test] async fn $name() { refused($tool,json!({$field:$value}),$field,$value).await; }
    };
}
refusal_case!(
    routing_preflight_trace_net,
    "route_trace",
    "net_name",
    "NOPE"
);
refusal_case!(
    routing_preflight_pad_net,
    "route_pad_to_pad",
    "net_name",
    "NOPE"
);
refusal_case!(
    routing_preflight_positive_net,
    "route_differential_pair",
    "net_pos",
    "NOPE"
);
refusal_case!(
    routing_preflight_negative_net,
    "route_differential_pair",
    "net_neg",
    "NOPE"
);
refusal_case!(routing_preflight_via_net, "add_via", "net_name", "NOPE");
refusal_case!(
    routing_preflight_trace_layer,
    "route_trace",
    "layer",
    "In5.Cu"
);
refusal_case!(
    routing_preflight_pad_layer,
    "route_pad_to_pad",
    "layer",
    "In5.Cu"
);
refusal_case!(
    routing_preflight_pair_layer,
    "route_differential_pair",
    "layer",
    "In5.Cu"
);

#[tokio::test]
async fn routing_preflight_valid_routes_still_create_requested_copper() {
    for (tool, count) in [
        ("route_trace", 1),
        ("route_pad_to_pad", 2),
        ("route_differential_pair", 2),
        ("add_via", 1),
    ] {
        let scene = Scene::new().await;
        let before = std::fs::read(&scene.board).unwrap();
        let body = scene.call(tool, json!({})).await;
        assert!(body.get("error").is_none(), "{tool}: {body}");
        let store = scene.store.lock().unwrap();
        assert_eq!(store.items.len(), 2 + count, "{tool}");
        let mut nets = Vec::new();
        for item in &store.items[2..] {
            if tool == "add_via" {
                let via = kiapi::board::types::Via::decode(item.value.as_slice()).unwrap();
                nets.push(via.net.unwrap().name);
            } else {
                let track = kiapi::board::types::Track::decode(item.value.as_slice()).unwrap();
                assert_eq!(track.layer, kiapi::board::types::BoardLayer::BlIn4Cu as i32);
                nets.push(track.net.unwrap().name);
            }
        }
        assert_eq!(
            nets,
            if tool == "route_differential_pair" {
                vec!["VCC", "GND"]
            } else {
                vec!["VCC"; count]
            }
        );
        assert_eq!(std::fs::read(&scene.board).unwrap(), before);
    }
}

#[tokio::test]
async fn routing_preflight_unreadable_net_evidence_is_not_a_missing_net() {
    let scene = Scene::with_evidence(NetReply::Malformed, false).await;
    let body = scene.call("route_trace", json!({})).await;
    assert_ne!(body["error"]["kind"], "invalid_argument", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("IPC error"),
        "{body}"
    );
    assert_eq!(scene.store.lock().unwrap().creates, 0);
}

async fn evidence_failure(net_reply: NetReply, empty_layers: bool, message: &str) {
    let scene = Scene::with_evidence(net_reply, empty_layers).await;
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let body = scene.call("route_trace", json!({})).await;
    assert_ne!(body["error"]["kind"], "invalid_argument", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains(message),
        "{body}"
    );
    let store = scene.store.lock().unwrap();
    assert_eq!((store.creates, store.updates, store.deletes), (0, 0, 0));
    assert_eq!(store.items, items);
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}
#[tokio::test]
async fn routing_preflight_requires_a_net_response_body() {
    evidence_failure(NetReply::Missing, false, "no GetNets response payload").await;
}
#[tokio::test]
async fn routing_preflight_refuses_empty_layer_evidence() {
    evidence_failure(NetReply::Normal, true, "empty enabled-layer set").await;
}
#[tokio::test]
async fn routing_preflight_explicit_empty_net_list_proves_a_missing_net() {
    let scene = Scene::with_evidence(NetReply::Empty, false).await;
    let body = scene.call("route_trace", json!({})).await;
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "net_name", "{body}");
    assert_eq!(scene.store.lock().unwrap().creates, 0);
}
#[tokio::test]
async fn net_reader_requires_a_response_body() {
    let scene = Scene::with_evidence(NetReply::Missing, false).await;
    let before = std::fs::read(&scene.board).unwrap();
    let body = scene.call("get_nets_list", json!({})).await;
    assert_eq!(body["error"]["kind"], "editor_unavailable", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("no GetNets response payload"), "{body}");
    assert!(message.contains(scene.board.to_str().unwrap()), "{body}");
    assert!(body.get("count").is_none(), "{body}");
    assert!(body.get("nets").is_none(), "{body}");
    assert_eq!(scene.store.lock().unwrap().creates, 0);
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}

#[tokio::test]
async fn net_reader_accepts_explicit_empty_response() {
    let scene = Scene::with_evidence(NetReply::Empty, false).await;
    let body = scene.call("get_nets_list", json!({})).await;
    assert_eq!(body["count"], 0, "{body}");
    assert_eq!(body["nets"], json!([]), "{body}");
    assert!(body.get("error").is_none(), "{body}");
}

#[tokio::test]
async fn net_reader_reports_observed_nets() {
    let scene = Scene::new().await;
    let body = scene.call("get_nets_list", json!({})).await;
    assert_eq!(body["count"], 3, "{body}");
    assert_eq!(
        body["nets"],
        json!([
            {"name":"", "netcode":0},
            {"name":"VCC", "netcode":1},
            {"name":"GND", "netcode":2}
        ]),
        "{body}"
    );
}

#[tokio::test]
async fn net_reader_sibling_netclasses_requires_response() {
    let scene = Scene::with_evidence(NetReply::Missing, false).await;
    std::fs::write(
        scene.board.with_extension("kicad_pro"),
        r#"{"net_settings":{"classes":[],"netclass_patterns":[]}}"#,
    )
    .unwrap();
    let body = scene
        .call("get_netclasses", json!({"board_source":"live"}))
        .await;
    assert_eq!(body["error"]["kind"], "editor_unavailable", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("no GetNets response payload"),
        "{body}"
    );
    assert!(body.get("netclasses").is_none(), "{body}");
}

#[tokio::test]
async fn net_reader_malformed_response_is_unavailable() {
    let scene = Scene::with_evidence(NetReply::Malformed, false).await;
    let body = scene.call("get_nets_list", json!({})).await;
    assert_eq!(body["error"]["kind"], "editor_unavailable", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(scene.board.to_str().unwrap()),
        "{body}"
    );
    assert!(body.get("count").is_none(), "{body}");
}

#[tokio::test]
async fn net_reader_preserves_wrong_document_refusal() {
    let scene = Scene::new().await;
    let other = scene._dir.path().join("other.kicad_pcb");
    std::fs::copy(&scene.board, &other).unwrap();
    let body = scene.call("get_nets_list", json!({"board":other})).await;
    assert_eq!(body["error"]["kind"], "wrong_document", "{body}");
    assert_eq!(
        body["error"]["requested"],
        other.to_str().unwrap(),
        "{body}"
    );
}

#[tokio::test]
async fn net_reader_sibling_netclasses_accepts_explicit_empty() {
    let scene = Scene::with_evidence(NetReply::Empty, false).await;
    std::fs::write(
        scene.board.with_extension("kicad_pro"),
        r#"{"net_settings":{"classes":[],"netclass_patterns":[]}}"#,
    )
    .unwrap();
    let body = scene
        .call("get_netclasses", json!({"board_source":"live"}))
        .await;
    assert_eq!(body["nets_on_board"], 0, "{body}");
    assert_eq!(body["sources"]["board_nets"], "ipc", "{body}");
    assert!(body.get("error").is_none(), "{body}");
}

async fn net_reader_writer_failure(tool: &str, changes: Value) {
    let scene = Scene::with_evidence(NetReply::Missing, false).await;
    let mut track =
        copperpour_ipc::builders::build_track("VCC", 1, "B.Cu", 0.25, 10.0, 10.0, 20.0, 10.0);
    track.id = Some(kiapi::common::types::Kiid {
        value: "net-reader-track".into(),
    });
    scene
        .store
        .lock()
        .unwrap()
        .items
        .push(pack_any(&track, "kiapi.board.types.Track"));
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let body = scene.call(tool, changes).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("no GetNets response payload"),
        "{tool}: {body}"
    );
    assert_ne!(body["error"]["kind"], "invalid_argument", "{tool}: {body}");
    assert!(body.get("plan_revision").is_none(), "{body}");
    let store = scene.store.lock().unwrap();
    assert_eq!((store.creates, store.updates, store.deletes), (0, 0, 0));
    assert_eq!(store.items, items);
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}

#[tokio::test]
async fn net_reader_modify_trace_requires_response_before_update() {
    net_reader_writer_failure("modify_trace", json!({"uuid":"net-reader-track"})).await;
}

#[tokio::test]
async fn net_reader_apply_copper_requires_response_before_planning_or_creation() {
    for dry_run in [true, false] {
        net_reader_writer_failure("apply_copper", json!({
            "tracks":[{"start":{"x":10.0,"y":10.0},"end":{"x":20.0,"y":10.0},"width":0.25,"layer":"B.Cu","net":"VCC"}],
            "dry_run":dry_run,"expected_plan_revision":"unobserved"
        })).await;
    }
}
