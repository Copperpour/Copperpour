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
    WrongType,
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
            type_url: "type.googleapis.com/kiapi.board.commands.NetsResponse".into(),
            value: vec![0xff],
        });
    }
    if command.type_url.ends_with("GetNets") && matches!(net_reply, NetReply::WrongType) {
        return Some(pack_any(
            &kiapi::common::commands::EndCommitResponse {},
            "kiapi.common.commands.EndCommitResponse",
        ));
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
        Self::with_create_reply(net_reply, empty_layers, false).await
    }
    async fn with_create_reply(
        net_reply: NetReply,
        empty_layers: bool,
        ignore_create: bool,
    ) -> Self {
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
                move |command, store| {
                    let reply = metadata_reply(command, &document, net_reply, empty_layers);
                    if ignore_create && command.type_url.ends_with("CreateItems") {
                        store.creates += 1;
                        return Some(pack_any(
                            &kiapi::common::commands::CreateItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                created_items: vec![],
                            },
                            "kiapi.common.commands.CreateItemsResponse",
                        ));
                    }
                    reply
                },
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
        self.call_with_status(tool, changes).await.0
    }
    async fn call_with_status(&self, tool: &str, changes: Value) -> (Value, bool) {
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
        if matches!(tool, "get_nets_list" | "add_via") {
            assert_eq!(
                response["isError"],
                body.get("error").is_some(),
                "{response}"
            );
        }
        if matches!(tool, "modify_trace" | "apply_copper") {
            assert_eq!(response["isError"], true, "{response}");
        }
        (body, response["isError"].as_bool().unwrap())
    }
}
async fn refused(tool: &str, changes: Value, field: &str, value: &str) {
    let scene = Scene::new().await;
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let (body, is_error) = scene.call_with_status(tool, changes).await;
    assert!(is_error, "{tool}: {body}");
    assert!(body.get("zone_refill").is_none(), "{tool}: {body}");
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
    for (tool, count, mut expected) in [
        (
            "route_trace",
            1,
            json!({
                "net": "VCC", "layer": "In4.Cu", "width": 0.25,
                "from": { "x": 10.0, "y": 10.0 }, "to": { "x": 20.0, "y": 10.0 }
            }),
        ),
        (
            "route_pad_to_pad",
            2,
            json!({
                "routed": true, "net": "VCC", "layer": "In4.Cu", "width": 0.25,
                "from": { "ref": "R1", "pad": "1", "x": 99.5, "y": 60.0 },
                "to": { "ref": "R2", "pad": "2", "x": 110.5, "y": 50.0 },
                "source": "ipc"
            }),
        ),
        (
            "route_differential_pair",
            2,
            json!({
                "net_pos": "VCC", "net_neg": "GND",
                "layer": "In4.Cu", "width": 0.1, "gap": 0.1
            }),
        ),
        (
            "add_via",
            1,
            json!({
                "uuid": "created-1", "net": "VCC", "x": 15.0, "y": 15.0,
                "drill": 0.4, "pad_size": 0.8, "source": "ipc_readback"
            }),
        ),
    ] {
        let scene = Scene::new().await;
        let before = std::fs::read(&scene.board).unwrap();
        let (body, is_error) = scene.call_with_status(tool, json!({})).await;
        assert!(!is_error, "{tool}: {body}");
        expected["zone_refill"] = json!({ "remedy": EXPECTED_ROUTING_REFILL_REMEDY });
        assert_eq!(body, expected, "{tool}");
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
                // Public defaults, in KiCad's nanometres, independent of the builder.
                assert_eq!(
                    track.width.unwrap().value_nm,
                    if tool == "route_differential_pair" {
                        100_000
                    } else {
                        250_000
                    }
                );
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
async fn routing_refill_guidance_is_absent_when_kicad_creates_nothing() {
    let scene = Scene::with_create_reply(NetReply::Normal, false, true).await;
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let (body, is_error) = scene.call_with_status("route_trace", json!({})).await;
    assert!(is_error, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("KiCad created no items"),
        "{body}"
    );
    assert!(body.get("zone_refill").is_none(), "{body}");
    let store = scene.store.lock().unwrap();
    assert_eq!((store.creates, store.updates, store.deletes), (1, 0, 0));
    assert_eq!(store.items, items);
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}

#[tokio::test]
async fn routing_refill_guidance_is_in_the_served_tool_descriptions() {
    let scene = Scene::new().await;
    let response = scene
        .handler
        .handle_message(json!({ "jsonrpc": "2.0", "id": 71, "method": "tools/list" }))
        .await
        .unwrap()
        .result
        .unwrap();
    let tools = response["tools"].as_array().unwrap();
    let affected = [
        "route_trace",
        "route_pad_to_pad",
        "add_via",
        "delete_trace",
        "modify_trace",
        "route_differential_pair",
    ];
    for name in affected
        .into_iter()
        .chain(["query_traces", "move_via", "delete_via"])
    {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        let description = tool["description"].as_str().unwrap();
        assert_eq!(
            description.ends_with(EXPECTED_ROUTING_REFILL_REMEDY),
            affected.contains(&name),
            "{name}: {description}"
        );
    }
}

#[tokio::test]
async fn routing_preflight_unreadable_net_evidence_is_not_a_missing_net() {
    evidence_failure(NetReply::Malformed, false, "Failed to decode protobuf").await;
}

async fn evidence_failure(net_reply: NetReply, empty_layers: bool, message: &str) {
    let scene = Scene::with_evidence(net_reply, empty_layers).await;
    let before = std::fs::read(&scene.board).unwrap();
    let items = scene.store.lock().unwrap().items.clone();
    let body = scene.call("route_trace", json!({})).await;
    assert_eq!(body["error"]["kind"], "handler_error", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains(message),
        "{body}"
    );
    assert_eq!(body["recovery"]["cause"], "preflight_refusal", "{body}");
    assert_eq!(body["recovery"]["mutation_outcome"], "none", "{body}");
    assert!(
        !body["recovery"]
            .as_object()
            .unwrap()
            .contains_key("troubleshooting_url"),
        "{body}"
    );
    let store = scene.store.lock().unwrap();
    assert_eq!((store.creates, store.updates, store.deletes), (0, 0, 0));
    assert_eq!(store.items, items);
    assert_eq!(std::fs::read(&scene.board).unwrap(), before);
}
#[tokio::test]
async fn routing_wrong_type_net_payload_never_sends_a_write() {
    evidence_failure(
        NetReply::WrongType,
        false,
        "unexpected GetNets response type",
    )
    .await;
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

#[tokio::test]
async fn routing_dimensions_are_refused_before_ipc_even_without_dispatch() {
    // An unusable address proves argument refusal happens before any IPC request.
    let ctx = Arc::new(ToolContext::new(
        crate::tools::ServerConfig::default(),
        Arc::new(crate::router::ToolRouter::new()),
    ));
    for (name, field) in [
        ("route_trace", "width"),
        ("route_pad_to_pad", "width"),
        ("modify_trace", "width"),
        ("route_differential_pair", "width"),
        ("route_differential_pair", "gap"),
    ] {
        let tool = tools().into_iter().find(|tool| tool.name == name).unwrap();
        for value in [
            json!(-0.2),
            json!(0),
            json!(0.0000001),
            json!("bad"),
            json!(false),
            json!([]),
            json!({}),
        ] {
            let mut args = json!({"board":"/not-open.kicad_pcb", "net_name":"VCC", "layer":"F.Cu", "x1":10,"y1":10,"x2":20,"y2":10,
                "uuid":"track", "ref1":"R1", "pad1":"1", "ref2":"R2", "pad2":"2", "net_pos":"VCC", "net_neg":"GND"});
            args[field] = value.clone();
            let result = (tool.handler)(&args, ctx.clone()).await.unwrap();
            assert!(result.is_error, "{name}.{field}={value}: {result:?}");
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text")
            };
            let body: Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{name}.{field}={value}: {body}"
            );
            assert_eq!(
                body["error"]["field"], field,
                "{name}.{field}={value}: {body}"
            );
        }
    }
}

#[tokio::test]
async fn routing_dimensions_served_refusals_preserve_copper() {
    for (tool, field) in [
        ("route_trace", "width"),
        ("route_pad_to_pad", "width"),
        ("modify_trace", "width"),
        ("route_differential_pair", "width"),
        ("route_differential_pair", "gap"),
    ] {
        for value in [json!(-0.2), json!(0), json!(0.0000001), json!("bad")] {
            let mut changes = json!({field:value});
            if tool == "modify_trace" {
                changes["uuid"] = json!("track");
            }
            refused(tool, changes, field, field).await;
        }
    }
}

#[test]
fn routing_dimensions_schema_and_rounding_boundaries() {
    for name in [
        "route_trace",
        "route_pad_to_pad",
        "modify_trace",
        "route_differential_pair",
    ] {
        let tool = tools().into_iter().find(|tool| tool.name == name).unwrap();
        assert_eq!(
            tool.input_schema["properties"]["width"]["exclusiveMinimum"], 0,
            "{name}"
        );
        if name == "route_differential_pair" {
            assert_eq!(
                tool.input_schema["properties"]["gap"]["exclusiveMinimum"],
                0
            );
        }
    }
    assert_eq!(routing_dimension(&json!({}), "width", 0.25).unwrap(), 0.25);
    assert_eq!(routing_dimension(&json!({}), "gap", 0.1).unwrap(), 0.1);
    // Exactly half a nanometre rounds up; slightly less must be refused.
    assert_eq!(
        routing_dimension(&json!({"width":0.0000005}), "width", 0.25).unwrap(),
        0.0000005
    );
    assert!(routing_dimension(&json!({"width":0.00000049}), "width", 0.25).is_err());
}

#[tokio::test]
async fn routing_dimensions_explicit_positive_values_reach_copper() {
    for tool in ["route_trace", "route_pad_to_pad", "route_differential_pair"] {
        let scene = Scene::new().await;
        let changes = if tool == "route_differential_pair" {
            json!({"width":0.3,"gap":0.2})
        } else {
            json!({"width":0.3})
        };
        let body = scene.call(tool, changes).await;
        assert!(body.get("error").is_none(), "{tool}: {body}");
        let store = scene.store.lock().unwrap();
        let tracks: Vec<_> = store.items[2..]
            .iter()
            .map(|item| kiapi::board::types::Track::decode(item.value.as_slice()).unwrap())
            .collect();
        assert!(!tracks.is_empty(), "{tool}");
        for track in &tracks {
            assert_eq!(track.width.as_ref().unwrap().value_nm, 300_000, "{tool}");
        }
        if tool == "route_differential_pair" {
            assert_eq!(tracks.len(), 2);
            // Horizontal pair: centre spacing = 0.3 mm width + 0.2 mm gap.
            assert_eq!(
                (tracks[0].start.as_ref().unwrap().y_nm - tracks[1].start.as_ref().unwrap().y_nm)
                    .abs(),
                500_000
            );
        }
    }
}

// Geometry oracle: PCB_VIA::ValidateViaParameters rejects diameter <= drill:
// https://github.com/KiCad/kicad-source-mirror/blob/10.0.5/pcbnew/pcb_track.cpp#L1694-L1699
fn via_geometry_invalid_cases() -> Vec<(Value, &'static str)> {
    vec![
        (json!({"drill": -0.2}), "drill"),
        (json!({"drill": 0}), "drill"),
        (json!({"pad_size": 0}), "pad_size"),
        (json!({"pad_size": -0.8}), "pad_size"),
        (json!({"drill": "bad"}), "drill"),
        (json!({"pad_size": "bad"}), "pad_size"),
        (json!({"drill": null}), "drill"),
        (json!({"pad_size": false}), "pad_size"),
        (json!({"drill": []}), "drill"),
        (json!({"pad_size": {}}), "pad_size"),
        (json!({"drill": 0.0000001}), "drill"),
        (json!({"pad_size": 0.0000001}), "pad_size"),
        (json!({"drill": 1, "pad_size": 0.4}), "drill"),
        (json!({"drill": 0.4, "pad_size": 0.4}), "drill"),
        (json!({"drill": 0.4000001, "pad_size": 0.4000002}), "drill"),
        (json!({"drill": 0.8}), "drill"),
        (json!({"pad_size": 0.4}), "drill"),
    ]
}

#[tokio::test]
async fn via_geometry_direct_refuses_before_ipc() {
    use crate::tools::pcb_board::board_mock::ctx_talking_to;
    let ctx = ctx_talking_to(String::new());
    for (changes, field) in via_geometry_invalid_cases() {
        let mut args = json!({"board":"unused.kicad_pcb","net_name":"VCC","x":1,"y":2});
        for (key, value) in changes.as_object().unwrap() {
            args[key] = value.clone();
        }
        // No endpoint exists: reaching IPC would return an error, not this refusal.
        let result = handle_add_via(&args, &ctx).await.unwrap();
        assert!(result.is_error, "{args}: {result:?}");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected structured refusal");
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument", "{args}: {body}");
        assert_eq!(body["error"]["field"], field, "{args}: {body}");
    }
}

#[tokio::test]
async fn via_geometry_served_refusals_preserve_copper() {
    for (changes, field) in via_geometry_invalid_cases() {
        refused("add_via", changes, field, "").await;
    }
}

#[tokio::test]
async fn via_geometry_defaults_and_explicit_sizes_are_stored() {
    for (changes, drill_nm, pad_nm) in [
        (json!({}), 400_000, 800_000),
        (json!({"drill":0.3,"pad_size":0.6}), 300_000, 600_000),
        (json!({"drill":0.3}), 300_000, 800_000),
        (json!({"pad_size":1}), 400_000, 1_000_000),
        (json!({"drill":0.4,"pad_size":0.400001}), 400_000, 400_001),
    ] {
        let scene = Scene::new().await;
        let body = scene.call("add_via", changes).await;
        assert_eq!(body["source"], "ipc_readback", "{body}");
        assert_eq!(body["net"], "VCC", "{body}");
        assert_eq!(body["drill"], drill_nm as f64 / 1_000_000.0);
        assert_eq!(body["pad_size"], pad_nm as f64 / 1_000_000.0);
        let store = scene.store.lock().unwrap();
        assert_eq!((store.creates, store.updates, store.deletes), (1, 0, 0));
        let vias: Vec<_> = store
            .items
            .iter()
            .filter(|i| i.type_url.ends_with("Via"))
            .collect();
        assert_eq!(vias.len(), 1);
        let via = kiapi::board::types::Via::decode(vias[0].value.as_slice()).unwrap();
        let stack = via.pad_stack.unwrap();
        let drill = stack.drill.unwrap().diameter.unwrap();
        assert_eq!((drill.x_nm, drill.y_nm), (drill_nm, drill_nm));
        let pad = stack.copper_layers[0].size.unwrap();
        assert_eq!((pad.x_nm, pad.y_nm), (pad_nm, pad_nm));
    }
}

#[test]
fn via_geometry_schema_advertises_positive_sizes_and_defaults() {
    let definition = tools()
        .into_iter()
        .find(|tool| tool.name == "add_via")
        .unwrap();
    for (field, default) in [("drill", 0.4), ("pad_size", 0.8)] {
        let property = &definition.input_schema["properties"][field];
        assert_eq!(property["type"], "number");
        assert_eq!(property["exclusiveMinimum"], 0);
        assert_eq!(property["default"], default);
    }
}
