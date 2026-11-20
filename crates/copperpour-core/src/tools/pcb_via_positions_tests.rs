//! Stateful served-dispatch checks using a captured KiCad-authored via message.
use super::*;
use crate::mcp::handler::McpHandler;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{board_document, spawn_kicad_holding_board};
use copperpour_ipc::gen::kiapi::common::commands;
use copperpour_ipc::gen::kiapi::common::types::ItemRequestStatus;
use std::sync::{Arc, Mutex};

const CAPTURE: &[u8] =
    include_bytes!("../../../copperpour-ipc/tests/fixtures/teardrops_kicad10.ipc.bin");
const BOARD: &str =
    include_str!("../../../copperpour-ipc/tests/fixtures/teardrops_kicad10.kicad_pcb");

#[derive(Default)]
struct State {
    vias: BTreeMap<String, Via>,
    precommit: Option<BTreeMap<String, Via>>,
    sent: Vec<String>,
    updates: usize,
    renet: bool,
    renet_restore: bool,
    corrupt_property: bool,
    partial_update: bool,
    failed_restore: bool,
    bad_readback: bool,
    duplicate_readback: bool,
    foreign_readback: bool,
    wrong_type: bool,
    race: bool,
    target_race: bool,
    restore_race: bool,
    failed_drop: bool,
    late_update: bool,
    snapshot_generation: u32,
}

fn fixture() -> State {
    let captured = commands::GetItemsResponse::decode(CAPTURE).unwrap();
    let template = captured
        .items
        .iter()
        .find(|item| any_is(item, VIA))
        .unwrap();
    let via = Via::decode(template.value.as_slice()).unwrap();
    let mut state = State::default();
    for index in 0..20 {
        let mut item = via.clone();
        item.id.as_mut().unwrap().value = format!("65100000-0000-4000-8000-{index:012}");
        // Clones model a 20-via batch; native padstack/teardrop/free-via data is KiCad's capture.
        item.position = Some(Vector2 {
            x_nm: 100_000_000 + index * 2_000_000,
            y_nm: 90_000_000,
        });
        item.locked = LockedState::LsUnlocked as i32;
        state
            .vias
            .insert(item.id.as_ref().unwrap().value.clone(), item);
    }
    state
}

fn answer(
    state: &mut State,
    document: &DocumentSpecifier,
    any: &prost_types::Any,
) -> Option<prost_types::Any> {
    let name = any.type_url.rsplit('.').next().unwrap();
    state.sent.push(name.into());
    let bytes = any.value.as_slice();
    match name {
        "SaveDocumentToString" => {
            let request = commands::SaveDocumentToString::decode(bytes).unwrap();
            assert_eq!(request.document.as_ref(), Some(document));
            Some(pack_any(
                &commands::SavedDocumentResponse {
                    document: Some(document.clone()),
                    // Snapshot changes with every native property and deliberate foreign-copper edit.
                    contents: format!("{BOARD}\n{:?}/{}", state.vias, state.snapshot_generation),
                },
                "kiapi.common.commands.SavedDocumentResponse",
            ))
        }
        "GetItemsById" => {
            let request = commands::GetItemsById::decode(bytes).unwrap();
            assert_eq!(request.header.unwrap().document.as_ref(), Some(document));
            let mut items: Vec<_> = request
                .items
                .iter()
                .filter_map(|id| state.vias.get(&id.value))
                .map(|via| pack_any(via, VIA))
                .collect();
            if state.bad_readback && state.updates > 0 {
                items.clear();
            }
            if state.duplicate_readback {
                items.push(items[0].clone());
            }
            if state.foreign_readback {
                let mut foreign = Via::decode(items[0].value.as_slice()).unwrap();
                foreign.id.as_mut().unwrap().value = "65100000-0000-4000-8000-999999999999".into();
                items.push(pack_any(&foreign, VIA));
            }
            if state.wrong_type {
                items[0].type_url = "kiapi.board.types.Track".into();
            }
            Some(pack_any(
                &commands::GetItemsResponse {
                    status: ItemRequestStatus::IrsOk as i32,
                    items,
                    ..Default::default()
                },
                "kiapi.common.commands.GetItemsResponse",
            ))
        }
        "BeginCommit" => {
            let request = commands::BeginCommit::decode(bytes).unwrap();
            assert_eq!(request.header.unwrap().document.as_ref(), Some(document));
            if state.race {
                state.snapshot_generation += 1;
            }
            if state.target_race || (state.restore_race && state.updates > 0) {
                state
                    .vias
                    .values_mut()
                    .next()
                    .unwrap()
                    .position
                    .as_mut()
                    .unwrap()
                    .x_nm += 10;
            }
            state.precommit = Some(state.vias.clone());
            Some(pack_any(
                &commands::BeginCommitResponse {
                    id: Some(copperpour_ipc::gen::kiapi::common::types::Kiid {
                        value: "via-move-651".into(),
                    }),
                },
                "kiapi.common.commands.BeginCommitResponse",
            ))
        }
        "UpdateItems" => {
            let request = commands::UpdateItems::decode(bytes).unwrap();
            assert_eq!(request.header.unwrap().document.as_ref(), Some(document));
            state.updates += 1;
            let mut updated_items = Vec::new();
            for (index, item) in request.items.iter().enumerate() {
                let mut via = Via::decode(item.value.as_slice()).unwrap();
                let refused = (state.partial_update && state.updates == 1 && index == 1)
                    || (state.failed_restore && state.updates > 1);
                if !refused {
                    if state.renet && (state.updates == 1 || state.renet_restore) && index == 0 {
                        via.net.as_mut().unwrap().name = "FOREIGN".into();
                        via.net.as_mut().unwrap().code =
                            Some(copperpour_ipc::gen::kiapi::board::types::NetCode { value: 999 });
                    }
                    if state.corrupt_property {
                        via.is_free = !via.is_free;
                    }
                    state
                        .vias
                        .insert(via.id.as_ref().unwrap().value.clone(), via);
                }
                updated_items.push(commands::ItemUpdateResult {
                    status: Some(commands::ItemStatus {
                        code: if refused {
                            commands::ItemStatusCode::IscInvalidData
                        } else {
                            commands::ItemStatusCode::IscOk
                        } as i32,
                        error_message: if refused {
                            "injected refusal".into()
                        } else {
                            String::new()
                        },
                    }),
                    item: Some(item.clone()),
                });
            }
            if state.late_update && state.updates == 1 {
                // Exercise the production 30 s receive window, including the live
                // typed late-reply recovery path, rather than injecting an envelope.
                std::thread::sleep(std::time::Duration::from_secs(31));
            }
            Some(pack_any(
                &commands::UpdateItemsResponse {
                    status: ItemRequestStatus::IrsOk as i32,
                    updated_items,
                    ..Default::default()
                },
                "kiapi.common.commands.UpdateItemsResponse",
            ))
        }
        "EndCommit" => {
            let request = commands::EndCommit::decode(bytes).unwrap();
            assert_eq!(request.header.unwrap().document.as_ref(), Some(document));
            if request.action == commands::CommitAction::CmaDrop as i32 && !state.failed_drop {
                state.vias = state.precommit.take().unwrap();
            }
            Some(pack_any(
                &commands::EndCommitResponse {},
                "kiapi.common.commands.EndCommitResponse",
            ))
        }
        _ => None,
    }
}

struct Scene {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    state: Arc<Mutex<State>>,
    _server: MockIpcServer,
    handler: McpHandler,
}

impl Scene {
    async fn new(state: State) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teardrops_kicad10.kicad_pcb");
        std::fs::write(&path, BOARD).unwrap();
        let document = board_document(path.to_str().unwrap());
        let state = Arc::new(Mutex::new(state));
        let held = state.clone();
        let server = spawn_kicad_holding_board(&path, move |any| {
            answer(&mut held.lock().unwrap(), &document, any)
        });
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().into(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            path,
            state,
            _server: server,
            handler,
        }
    }

    fn offset(&self) -> Value {
        json!({"uuids": self.state.lock().unwrap().vias.keys().collect::<Vec<_>>(), "offset": {"x": 1.25, "y": -2.5}})
    }

    async fn call(&self, mut args: Value) -> (bool, Value) {
        args["board"] = json!(self.path);
        let response = self
            .handler
            .handle_message(json!({"jsonrpc": "2.0", "id": 651,
            "method": "tools/call", "params": {"name": "set_via_positions", "arguments": args}}))
            .await
            .unwrap();
        let result = response.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap_or_else(|_| json!({"message":text})),
        )
    }

    async fn apply(&self, mut args: Value) -> (bool, Value) {
        let (error, planned) = self.call(args.clone()).await;
        assert!(!error, "{planned}");
        args["dry_run"] = json!(false);
        args["expected_plan_revision"] = planned["plan_revision"].clone();
        self.call(args).await
    }

    fn assert_no_writes(&self) {
        assert!(!self.state.lock().unwrap().sent.iter().any(|c| [
            "BeginCommit",
            "UpdateItems",
            "EndCommit"
        ]
        .contains(&c.as_str())));
    }
}

#[tokio::test]
async fn twenty_vias_share_one_update_and_preserve_every_native_property() {
    let scene = Scene::new(fixture()).await;
    let before = scene.state.lock().unwrap().vias.clone();
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(!error, "{result}");
    assert_eq!(result["via_count"], 20);
    assert_eq!(result["source"], "ipc_readback");
    assert_eq!(result["outcome"]["status"], "complete");
    let state = scene.state.lock().unwrap();
    assert_eq!(state.updates, 1);
    assert_eq!(
        state
            .sent
            .iter()
            .filter(|c| c.as_str() == "BeginCommit")
            .count(),
        1
    );
    for (id, via) in &state.vias {
        let mut expected = before[id].clone();
        expected.position.as_mut().unwrap().x_nm += 1_250_000;
        expected.position.as_mut().unwrap().y_nm -= 2_500_000;
        assert_eq!(*via, expected);
    }
    assert!(!state
        .sent
        .iter()
        .any(|c| ["SaveDocument", "RefillZones"].contains(&c.as_str())));
}

#[tokio::test]
async fn absolute_positions_round_to_nanometres_and_dry_run_never_writes() {
    let scene = Scene::new(fixture()).await;
    let id = scene
        .state
        .lock()
        .unwrap()
        .vias
        .keys()
        .next()
        .unwrap()
        .clone();
    let args = json!({"placements": [{"uuid":id,"x":-0.0000006,"y":125.0000006}]});
    let (error, result) = scene.call(args.clone()).await;
    assert!(!error, "{result}");
    assert_eq!(result["source"], "ipc_plan");
    assert_eq!(result["vias"][0]["x"], -0.000001);
    assert_eq!(result["vias"][0]["y"], 125.000001);
    scene.assert_no_writes();
    let (error, result) = scene.apply(args).await;
    assert!(!error, "{result}");
}

#[tokio::test]
async fn malformed_entries_duplicates_and_wrong_options_never_write() {
    let scene = Scene::new(fixture()).await;
    let id = scene
        .state
        .lock()
        .unwrap()
        .vias
        .keys()
        .next()
        .unwrap()
        .clone();
    for args in [
        json!({"placements":[]}),
        json!({"uuids":[id,id],"offset":{"x":1,"y":2}}),
        json!({"placements":[{"uuid":id,"x":1}]}),
        json!({"placements":[{"uuid":id,"x":1,"y":2,"typo":0}]}),
        json!({"uuids":[id],"offset":{"x":1,"y":2},"placements":[{"uuid":id,"x":1,"y":2}]}),
        json!({"uuids":[id],"offset":{"x":1,"y":2},"dry_run":"false"}),
        json!({"uuids":[id],"offset":{"x":1,"y":2},"dry_run":false}),
        json!({"placements":[{"uuid":id,"x":1e99,"y":2}]}),
    ] {
        let (error, result) = scene.call(args).await;
        assert!(error, "{result}");
    }
    scene.assert_no_writes();
}

#[tokio::test]
async fn locked_missing_non_via_duplicate_and_overflow_targets_refuse() {
    for case in 0..5 {
        let mut state = fixture();
        let id = state.vias.keys().next().unwrap().clone();
        match case {
            0 => state.vias.get_mut(&id).unwrap().locked = LockedState::LsLocked as i32,
            1 => {
                state.vias.remove(&id);
            }
            2 => state.wrong_type = true,
            3 => state.duplicate_readback = true,
            _ => {
                state
                    .vias
                    .get_mut(&id)
                    .unwrap()
                    .position
                    .as_mut()
                    .unwrap()
                    .x_nm = LIMIT
            }
        }
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .call(json!({"uuids":[id],"offset":{"x":1,"y":0}}))
            .await;
        assert!(error, "case {case}: {result}");
        scene.assert_no_writes();
    }
}

#[tokio::test]
async fn a_changed_board_or_target_or_destination_invalidates_the_revision() {
    for case in 0..3 {
        let scene = Scene::new(fixture()).await;
        let mut args = scene.offset();
        let (error, result) = scene.call(args.clone()).await;
        assert!(!error, "{result}");
        match case {
            0 => scene.state.lock().unwrap().snapshot_generation += 1,
            1 => {
                scene
                    .state
                    .lock()
                    .unwrap()
                    .vias
                    .values_mut()
                    .next()
                    .unwrap()
                    .is_free = !fixture().vias.values().next().unwrap().is_free
            }
            _ => args["offset"]["x"] = json!(2),
        }
        args["dry_run"] = json!(false);
        args["expected_plan_revision"] = result["plan_revision"].clone();
        let (error, result) = scene.call(args).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "stale_target");
        scene.assert_no_writes();
    }
}

#[tokio::test]
async fn board_changes_after_begin_commit_are_not_overwritten() {
    let mut state = fixture();
    state.race = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "stale_target");
    assert_eq!(scene.state.lock().unwrap().updates, 0);
}

#[tokio::test]
async fn renetting_restores_the_entire_batch_and_failed_restoration_is_uncertain() {
    for fail in [false, true] {
        let mut state = fixture();
        state.renet = true;
        state.failed_restore = fail;
        let before = state.vias.clone();
        let scene = Scene::new(state).await;
        let (error, result) = scene.apply(scene.offset()).await;
        assert!(error, "{result}");
        assert_eq!(
            result["error"]["kind"],
            if fail {
                "mutation_outcome_uncertain"
            } else {
                "plan_blocked"
            }
        );
        if fail {
            assert_eq!(result["uuids"].as_array().unwrap().len(), 20);
            assert_eq!(result["outcome"]["retry"]["safe"], false);
        } else {
            assert_eq!(scene.state.lock().unwrap().vias, before);
        }
        assert_eq!(scene.state.lock().unwrap().updates, 2);
    }
}

#[tokio::test]
async fn partial_updates_drop_and_prove_originals_or_report_uncertainty() {
    for fail in [false, true] {
        let mut state = fixture();
        state.partial_update = true;
        state.failed_drop = fail;
        let before = state.vias.clone();
        let scene = Scene::new(state).await;
        let (error, result) = scene.apply(scene.offset()).await;
        assert!(error, "{result}");
        assert_eq!(
            result["error"]["kind"],
            if fail {
                "mutation_outcome_uncertain"
            } else {
                "plan_blocked"
            }
        );
        if !fail {
            assert_eq!(scene.state.lock().unwrap().vias, before);
        }
    }
}

#[tokio::test]
async fn missing_readback_or_lost_native_properties_never_report_success() {
    for corrupt in [false, true] {
        let mut state = fixture();
        state.bad_readback = !corrupt;
        state.corrupt_property = corrupt;
        let scene = Scene::new(state).await;
        let (error, result) = scene.apply(scene.offset()).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
        assert_eq!(
            scene.state.lock().unwrap().updates,
            1,
            "never blindly restore unaccounted-for changes"
        );
    }
}

#[tokio::test]
async fn zero_offset_is_observed_without_an_undo_commit() {
    let scene = Scene::new(fixture()).await;
    let mut args = scene.offset();
    args["offset"] = json!({"x":0,"y":0});
    let (error, result) = scene.apply(args).await;
    assert!(!error, "{result}");
    assert_eq!(result["unchanged"], true);
    scene.assert_no_writes();
}

/// Run only on a disposable copy of a KiCad-authored fixture (for example
/// copperpour-ipc/tests/fixtures/teardrops_kicad10.kicad_pcb), opened in pcbnew.
/// KICAD_API_SOCKET=... COPPERPOUR_LIVE_VIA_BOARD=... cargo test -p copperpour-core
/// --locked live_via_positions_batch_and_benchmark -- --ignored --nocapture
#[tokio::test]
#[ignore = "requires an explicitly named disposable KiCad board and its API socket"]
async fn live_via_positions_batch_and_benchmark() {
    let path = std::path::PathBuf::from(
        std::env::var("COPPERPOUR_LIVE_VIA_BOARD")
            .expect("name a disposable open board, never a user's design"),
    );
    let address = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET");
    let handler = McpHandler::new(crate::tools::ServerConfig {
        ipc_address: address.clone(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    async fn call(handler: &McpHandler, path: &Path, name: &str, mut args: Value) -> Value {
        args["board"] = json!(path);
        let response = handler
            .handle_message(json!({"jsonrpc":"2.0","id":651,
            "method":"tools/call","params":{"name":name,"arguments":args}}))
            .await
            .unwrap();
        let result = response.result.unwrap();
        assert_ne!(result["isError"], true, "{name}: {result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    let client = KiCadIpcClient::new(address);
    let document = client.find_open_board(&path).unwrap();
    let vias: Vec<_> = (0..20)
        .map(|index| {
            json!({"position":{"x":10+index*2,"y":15},
        "net":"GND","size":0.6,"drill":0.3})
        })
        .collect();
    let create = json!({"vias":vias});
    let plan = call(&handler, &path, "apply_copper", create.clone()).await;
    let mut create_apply = create;
    create_apply["dry_run"] = json!(false);
    create_apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let created = call(&handler, &path, "apply_copper", create_apply).await;
    let ids: Vec<String> = created["vias"]
        .as_array()
        .unwrap()
        .iter()
        .map(|via| via["uuid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 20);
    let before = read_vias(&client, &document, &ids).unwrap();
    let start = std::time::Instant::now();
    let mut baseline_bytes = 0;
    for (id, via) in &before {
        let pos = via.position.unwrap();
        let result = call(
            &handler,
            &path,
            "move_via",
            json!({"uuid":id,
            "x":nm_to_mm(pos.x_nm)+1.0,"y":nm_to_mm(pos.y_nm)+1.0}),
        )
        .await;
        baseline_bytes += serde_json::to_vec(&result).unwrap().len();
    }
    let baseline_ms = start.elapsed().as_millis();
    let offset = json!({"uuids":ids,"offset":{"x":-1,"y":-1}});
    let start = std::time::Instant::now();
    let plan = call(&handler, &path, "set_via_positions", offset.clone()).await;
    let mut apply = offset;
    apply["dry_run"] = json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let plan_ms = start.elapsed().as_millis();
    let start = std::time::Instant::now();
    let result = call(&handler, &path, "set_via_positions", apply).await;
    let apply_ms = start.elapsed().as_millis();
    assert_eq!(read_vias(&client, &document, &ids).unwrap(), before);
    eprintln!(
        "via-batch benchmark: {}",
        json!({"kicad":format!("{:?}", client.get_kicad_version().unwrap()),
        "baseline_mutation_calls":20,"baseline_ms":baseline_ms,"baseline_response_bytes":baseline_bytes,
        "batch_plan_calls":1,"batch_mutation_calls":1,"batch_plan_ms":plan_ms,"batch_apply_ms":apply_ms,
        "batch_response_bytes":serde_json::to_vec(&result).unwrap().len(),"via_count":20})
    );
    // Cleanup is also through the served tool contract; the original fixture vias stay untouched.
    for id in ids {
        call(&handler, &path, "delete_via", json!({"uuid":id})).await;
    }
}

#[tokio::test]
async fn external_target_edit_before_update_is_a_stale_refusal_not_our_mutation() {
    let mut state = fixture();
    state.target_race = true;
    let before = state.vias.clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "stale_target");
    assert_eq!(scene.state.lock().unwrap().updates, 0);
    assert_ne!(
        scene.state.lock().unwrap().vias,
        before,
        "preserve the external edit"
    );
}

#[test]
fn uncertain_ipc_and_recovered_errors_never_advertise_safe_replay() {
    for kind in [
        ToolErrorKind::IpcOutcomeUnknown {
            board_state: "unknown".into(),
            retry_safe: false,
            reason: "lost reply".into(),
        },
        ToolErrorKind::IpcBatchRecovered {
            board_state: "restored".into(),
            drop_confirmed: true,
            retry_safe: false,
            reason: "late reply".into(),
        },
    ] {
        let unknown = matches!(kind, ToolErrorKind::IpcOutcomeUnknown { .. });
        let result = envelope(
            CallToolResult::error_kind(kind, "injected IPC result"),
            Path::new("board.kicad_pcb"),
            20,
        );
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("text")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["outcome"]["retry"]["safe"], false);
        assert_eq!(
            body["outcome"]["status"],
            if unknown { "uncertain" } else { "failed" }
        );
    }
}

#[tokio::test]
async fn missing_revision_handler_refusal_has_outcome_and_wrong_board_never_writes() {
    let mut scene = Scene::new(fixture()).await;
    let mut args = scene.offset();
    args["dry_run"] = json!(false);
    let (error, result) = scene.call(args).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "invalid_argument");
    assert_eq!(result["outcome"]["status"], "failed");
    scene.path = scene.path.with_file_name("different.kicad_pcb");
    let (error, result) = scene.call(scene.offset()).await;
    assert!(error, "{result}");
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_late_update_is_recovered_without_advertising_safe_replay() {
    let mut state = fixture();
    state.late_update = true;
    let before = state.vias.clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "ipc_batch_recovered");
    assert_eq!(result["error"]["retry_safe"], false);
    assert_eq!(result["outcome"]["retry"]["safe"], false);
    assert_eq!(scene.state.lock().unwrap().vias, before);
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

#[test]
fn coordinates_refuse_out_of_range_before_integer_conversion() {
    assert!(coordinate(&json!({"x":1e99}), "x").is_err());
    assert!(coordinate(&json!({"x":-1e99}), "x").is_err());
    assert_eq!(coordinate(&json!({"x":-0.0000006}), "x").unwrap(), -1);
}

#[tokio::test]
async fn restore_commit_whose_readback_still_differs_is_uncertain() {
    let mut state = fixture();
    state.renet = true;
    state.renet_restore = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(result["uuids"].as_array().unwrap().len(), 20);
    assert_eq!(result["outcome"]["retry"]["safe"], false);
    assert_eq!(scene.state.lock().unwrap().updates, 2);
}

#[tokio::test]
async fn renetting_with_another_property_change_never_restores() {
    let mut state = fixture();
    state.renet = true;
    state.corrupt_property = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(result["uuids"].as_array().unwrap().len(), 20);
    assert_eq!(result["outcome"]["retry"]["safe"], false);
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

#[tokio::test]
async fn external_edit_before_restoration_is_uncertain_without_second_update() {
    let mut state = fixture();
    state.renet = true;
    state.restore_race = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(scene.offset()).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(result["uuids"].as_array().unwrap().len(), 20);
    assert_eq!(result["outcome"]["retry"]["safe"], false);
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

#[tokio::test]
async fn unrequested_via_in_readback_is_refused() {
    let mut state = fixture();
    state.foreign_readback = true;
    let id = state.vias.keys().next().unwrap().clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .call(json!({"uuids":[id],"offset":{"x":1,"y":0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    scene.assert_no_writes();
}
