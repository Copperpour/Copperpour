use super::live_board::create_sync_items_in;
use crate::test_support::MockIpcServer;
use anyhow::{Context, Result};
use copperpour_ipc::{builders::pack_any, gen::kiapi, KiCadIpcClient};
use prost::Message;
use std::sync::{Arc, Mutex};

fn footprints(count: usize) -> Vec<prost_types::Any> {
    let source = include_str!("../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
    let pads = crate::tools::pcb_components::extract_pad_definitions(source).unwrap();
    let graphics = crate::tools::pcb_components::extract_graphic_definitions(source).unwrap();
    let fields = crate::tools::pcb_components::extract_field_placement(source);
    (0..count)
        .map(|i| {
            KiCadIpcClient::build_footprint_item(
                "Capacitor_SMD:C_0603_1608Metric",
                &format!("C{}", i + 1),
                "100n",
                &pads,
                &graphics,
                &fields,
                5.0 + (i % 10) as f64 * 4.0,
                5.0 + (i / 10) as f64 * 4.0,
                0.0,
                "F.Cu",
            )
            .unwrap()
        })
        .collect()
}

#[derive(Default)]
struct Observed {
    chunks: Vec<usize>,
    references: Vec<String>,
    staged: usize,
    begins: usize,
    actions: Vec<i32>,
    updates: usize,
}

fn exercise(count: usize, refusal: bool, malformed: bool) -> (Result<()>, Observed) {
    let directory = tempfile::tempdir().unwrap();
    let board = directory.path().join("chunk.kicad_pcb");
    let document = crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
    let target = document.clone();
    let state = Arc::new(Mutex::new(Observed::default()));
    let observed = state.clone();
    let mock = MockIpcServer::spawn("sync-chunks", move |request| {
        let command = request.message.unwrap();
        let mut state = observed.lock().unwrap();
        let mut response = kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            ..Default::default()
        };
        response.message = match command.type_url.rsplit('.').next().unwrap() {
            "GetOpenDocuments" => Some(pack_any(
                &kiapi::common::commands::GetOpenDocumentsResponse {
                    documents: vec![target.clone()],
                },
                "kiapi.common.commands.GetOpenDocumentsResponse",
            )),
            "SaveDocumentToString" => {
                let request =
                    kiapi::common::commands::SaveDocumentToString::decode(command.value.as_slice())
                        .unwrap();
                assert_eq!(request.document, Some(target.clone()));
                Some(pack_any(
                    &kiapi::common::commands::SavedDocumentResponse {
                        document: Some(target.clone()),
                        contents: include_str!(
                            "../../../../copperpour-sexp/tests/fixtures/gr_poly_outline.kicad_pcb"
                        )
                        .into(),
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                ))
            }
            "BeginCommit" => {
                state.begins += 1;
                Some(pack_any(
                    &kiapi::common::commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "chunk-commit".into(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                ))
            }
            "CreateItems" => {
                let request =
                    kiapi::common::commands::CreateItems::decode(command.value.as_slice()).unwrap();
                assert_eq!(request.header.unwrap().document, Some(target.clone()));
                state.chunks.push(request.items.len());
                for item in &request.items {
                    let fp = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                        .unwrap();
                    state
                        .references
                        .push(fp.reference_field.unwrap().text.unwrap().text.unwrap().text);
                }
                state.staged += request.items.len();
                if state.chunks.len() == 2 && refusal {
                    response.status.as_mut().unwrap().status =
                        kiapi::common::ApiStatusCode::AsBadRequest as i32;
                    response.status.as_mut().unwrap().error_message =
                        "injected second-chunk refusal".into();
                    None
                } else if state.chunks.len() == 2 && malformed {
                    response.status = None;
                    None
                } else {
                    Some(pack_any(
                        &kiapi::common::commands::CreateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            created_items: request
                                .items
                                .into_iter()
                                .map(|item| kiapi::common::commands::ItemCreationResult {
                                    status: Some(kiapi::common::commands::ItemStatus {
                                        code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                        error_message: String::new(),
                                    }),
                                    item: Some(item),
                                })
                                .collect(),
                        },
                        "kiapi.common.commands.CreateItemsResponse",
                    ))
                }
            }
            "UpdateItems" => {
                state.updates += 1;
                let request =
                    kiapi::common::commands::UpdateItems::decode(command.value.as_slice()).unwrap();
                assert_eq!(request.header.unwrap().document, Some(target.clone()));
                Some(pack_any(
                    &kiapi::common::commands::UpdateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        updated_items: request
                            .items
                            .into_iter()
                            .map(|item| kiapi::common::commands::ItemUpdateResult {
                                status: Some(kiapi::common::commands::ItemStatus {
                                    code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(item),
                            })
                            .collect(),
                    },
                    "kiapi.common.commands.UpdateItemsResponse",
                ))
            }
            "EndCommit" => {
                let request =
                    kiapi::common::commands::EndCommit::decode(command.value.as_slice()).unwrap();
                assert_eq!(request.id.unwrap().value, "chunk-commit");
                state.actions.push(request.action);
                if request.action == kiapi::common::commands::CommitAction::CmaDrop as i32 {
                    state.staged = 0;
                }
                Some(pack_any(
                    &kiapi::common::commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                ))
            }
            other => panic!("unexpected {other}"),
        };
        response
    });
    let items = footprints(count);
    let result = KiCadIpcClient::new(mock.address()).run_commit_recovering_in(
        document.clone(),
        "sync",
        |client| {
            create_sync_items_in(client, &document, &items)?;
            client.update_items_in(document.clone(), footprints(1))?;
            Ok(())
        },
    );
    drop(mock);
    let state = Arc::try_unwrap(state).ok().unwrap().into_inner().unwrap();
    (result, state)
}

#[test]
fn sync_chunk_boundaries_preserve_order_and_one_commit() {
    for (count, expected) in [
        (0, vec![]),
        (1, vec![1]),
        (32, vec![32]),
        (33, vec![32, 1]),
        (70, vec![32, 32, 6]),
    ] {
        let (result, state) = exercise(count, false, false);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(state.chunks, expected);
        assert_eq!(
            state.references,
            (1..=count).map(|i| format!("C{i}")).collect::<Vec<_>>()
        );
        assert_eq!(state.begins, 1);
        assert_eq!(
            state.actions,
            vec![kiapi::common::commands::CommitAction::CmaCommit as i32]
        );
        assert_eq!(state.staged, count);
        assert_eq!(state.updates, 1);
    }
}

#[test]
fn sync_chunk_failure_stops_and_drops_without_publishing_partial_work() {
    let (result, state) = exercise(70, true, false);
    assert!(result.unwrap_err().to_string().contains("changes dropped"));
    assert_eq!(state.chunks, vec![32, 32]);
    assert_eq!(
        state.actions,
        vec![kiapi::common::commands::CommitAction::CmaDrop as i32]
    );
    assert_eq!(state.staged, 0);
    assert_eq!(state.updates, 0);
}

#[test]
fn sync_chunk_uncertainty_stops_without_blind_drop_or_publish() {
    let (result, state) = exercise(70, false, true);
    assert!(matches!(
        copperpour_ipc::IpcFailure::from_error(result.unwrap_err()),
        copperpour_ipc::IpcFailure::Uncertain(_)
    ));
    assert_eq!(state.chunks, vec![32, 32]);
    assert!(state.actions.is_empty());
    assert_eq!(state.staged, 64);
    assert_eq!(state.updates, 0);
}

// RunAction is a version-specific acceptance probe, not a supported tool API.
fn native_action(address: &str, action: &str) -> Result<()> {
    use nng::options::Options;
    let socket = nng::Socket::new(nng::Protocol::Req0)?;
    socket.set_opt::<nng::options::SendTimeout>(Some(std::time::Duration::from_secs(5)))?;
    socket.set_opt::<nng::options::RecvTimeout>(Some(std::time::Duration::from_secs(5)))?;
    socket.dial(address)?;
    let request = kiapi::common::ApiRequest {
        header: Some(kiapi::common::ApiRequestHeader {
            kicad_token: std::env::var("KICAD_API_TOKEN").unwrap_or_default(),
            client_name: "copperpour-chunk-acceptance".into(),
        }),
        message: Some(pack_any(
            &kiapi::common::commands::RunAction {
                action: action.into(),
            },
            "kiapi.common.commands.RunAction",
        )),
    };
    socket
        .send(request.encode_to_vec().as_slice())
        .map_err(|(_, error)| error)?;
    let response = socket.recv()?;
    let response = kiapi::common::ApiResponse::decode(response.as_slice())?;
    anyhow::ensure!(
        response.status.context("missing action status")?.status
            == kiapi::common::ApiStatusCode::AsOk as i32,
        "action API refusal"
    );
    let message = response.message.context("missing action response")?;
    anyhow::ensure!(
        message
            .type_url
            .ends_with("kiapi.common.commands.RunActionResponse"),
        "unexpected action response"
    );
    let response = kiapi::common::commands::RunActionResponse::decode(message.value.as_slice())?;
    anyhow::ensure!(
        response.status == kiapi::common::commands::RunActionStatus::RasOk as i32,
        "native action not submitted"
    );
    Ok(())
}

#[test]
#[ignore = "requires COPPERPOUR_LIVE_CHUNK_BOARD: disposable sole open PCB and KiCad 10.0.6 API"]
fn sync_chunk_live_one_undo_restores_entire_board() {
    let board = std::path::PathBuf::from(
        crate::app_dirs::env_var("LIVE_CHUNK_BOARD").expect("disposable board required"),
    );
    let address = std::env::var("KICAD_API_SOCKET")
        .ok()
        .or_else(copperpour_ipc::detect_ipc_address)
        .expect("KiCad IPC required");
    let client = KiCadIpcClient::new(address.clone());
    let document = client.find_open_board(&board).unwrap();
    assert_eq!(
        client.get_open_documents().unwrap(),
        vec![document.clone()],
        "actions require sole disposable board"
    );
    let baseline = client.save_document_to_string_in(document.clone()).unwrap();
    let before = client
        .get_items_in(
            document.clone(),
            kiapi::common::types::KiCadObjectType::KotPcbFootprint,
        )
        .unwrap();
    let items = footprints(70);
    client
        .run_commit_recovering_in(
            document.clone(),
            "70-footprint sync chunk acceptance",
            |client| create_sync_items_in(client, &document, &items),
        )
        .unwrap();
    let after = client
        .get_items_in(
            document.clone(),
            kiapi::common::types::KiCadObjectType::KotPcbFootprint,
        )
        .unwrap();
    assert_eq!(after.len(), before.len() + 70);
    let mut references = after
        .iter()
        .map(|item| {
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                .unwrap()
                .reference_field
                .unwrap()
                .text
                .unwrap()
                .text
                .unwrap()
                .text
        })
        .collect::<Vec<_>>();
    references.sort();
    for i in 1..=70 {
        assert!(references.contains(&format!("C{i}")));
    }
    let published = client.save_document_to_string_in(document.clone()).unwrap();
    assert_ne!(published, baseline);
    let wait_for = |expected: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if client.save_document_to_string_in(document.clone()).unwrap() == expected {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native action did not restore entire serialized board"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    native_action(&address, "common.Interactive.undo").unwrap();
    wait_for(&baseline);
    native_action(&address, "common.Interactive.redo").unwrap();
    wait_for(&published);
    native_action(&address, "common.Interactive.undo").unwrap();
    wait_for(&baseline);
    eprintln!("LIVE PASS: 70 footprints in 32/32/6 chunks; one Undo restores full baseline; Redo restores full published state; final Undo leaves baseline.");
}
