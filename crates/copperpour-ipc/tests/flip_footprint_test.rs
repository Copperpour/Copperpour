//! Issue #604 regression tests: `flip_footprint` drives KiCad 10.0.6's native
//! `FlipItems` IPC command and must never treat envelope-level `IRS_OK` as
//! proof, must independently re-read the board rather than trusting the
//! mutation response, and must roll back its commit on failure.
//!
//! Same mock-KiCad approach as `footprint_transform_test.rs` and
//! `mock_server_test.rs`: an in-process NNG rep0 socket serving canned
//! protobuf responses.

use copperpour_ipc::gen::kiapi;
use copperpour_ipc::types::{IpcFlipOutcome, IpcFootprint3DModel, IpcVector3};
use copperpour_ipc::{builders, KiCadIpcClient};
use nng::options::Options;
use prost::Message;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct MockKicad {
    url: String,
    _thread: std::thread::JoinHandle<()>,
}

fn spawn_mock<F>(respond: F) -> MockKicad
where
    F: Fn(kiapi::common::ApiRequest) -> Option<kiapi::common::ApiResponse> + Send + 'static,
{
    static NEXT_MOCK: AtomicUsize = AtomicUsize::new(0);
    let url = format!(
        "inproc://mock-kicad-flip-{}",
        NEXT_MOCK.fetch_add(1, Ordering::Relaxed)
    );

    let socket = nng::Socket::new(nng::Protocol::Rep0).expect("mock rep socket");
    socket
        .set_opt::<nng::options::RecvTimeout>(Some(Duration::from_secs(20)))
        .unwrap();
    socket.listen(&url).expect("mock listen");

    let thread = std::thread::spawn(move || {
        while let Ok(msg) = socket.recv() {
            let request = match kiapi::common::ApiRequest::decode(msg.as_slice()) {
                Ok(r) => r,
                Err(_) => break,
            };
            match respond(request) {
                Some(resp) => {
                    let out = nng::Message::from(resp.encode_to_vec().as_slice());
                    if socket.send(out).is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    });

    MockKicad {
        url,
        _thread: thread,
    }
}

fn ok_response() -> kiapi::common::ApiResponse {
    kiapi::common::ApiResponse {
        status: Some(kiapi::common::ApiResponseStatus {
            status: kiapi::common::ApiStatusCode::AsOk as i32,
            error_message: String::new(),
        }),
        header: None,
        message: None,
    }
}

fn error_response(code: kiapi::common::ApiStatusCode, message: &str) -> kiapi::common::ApiResponse {
    kiapi::common::ApiResponse {
        status: Some(kiapi::common::ApiResponseStatus {
            status: code as i32,
            error_message: message.to_string(),
        }),
        header: None,
        message: None,
    }
}

fn reply_with(inner: prost_types::Any) -> kiapi::common::ApiResponse {
    kiapi::common::ApiResponse {
        message: Some(inner),
        ..ok_response()
    }
}

fn mock_project_dir() -> &'static str {
    if cfg!(windows) {
        r"C:\konnect-mock-project"
    } else {
        "/konnect-mock-project"
    }
}

fn mk_field(name: &str, text: &str, x_mm: f64, y_mm: f64) -> kiapi::board::types::Field {
    kiapi::board::types::Field {
        name: name.to_string(),
        text: Some(kiapi::board::types::BoardText {
            text: Some(kiapi::common::types::Text {
                text: text.to_string(),
                position: Some(builders::vec2(x_mm, y_mm)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn mk_model(filename: &str, offset_y: f64, rotate_x: f64, rotate_y: f64) -> prost_types::Any {
    builders::pack_any(
        &kiapi::board::types::Footprint3DModel {
            filename: filename.to_string(),
            scale: Some(kiapi::common::types::Vector3D {
                x_nm: 1.0,
                y_nm: 1.0,
                z_nm: 1.0,
            }),
            rotation: Some(kiapi::common::types::Vector3D {
                x_nm: rotate_x,
                y_nm: rotate_y,
                z_nm: 0.0,
            }),
            offset: Some(kiapi::common::types::Vector3D {
                x_nm: 0.0,
                y_nm: offset_y,
                z_nm: 0.0,
            }),
            visible: true,
            opacity: 1.0,
        },
        "kiapi.board.types.Footprint3DModel",
    )
}

/// A placed footprint with a stable KIID, a layer, one pad (unrelated content
/// that must survive untouched), and one 3D model with a non-zero Y offset
/// and X/Y rotation — the exact geometry the closed-board fallback refuses.
fn mk_footprint(
    kiid: &str,
    layer: kiapi::board::types::BoardLayer,
) -> kiapi::board::types::FootprintInstance {
    kiapi::board::types::FootprintInstance {
        id: Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        }),
        position: Some(builders::vec2(100.0, 100.0)),
        orientation: Some(kiapi::common::types::Angle { value_degrees: 0.0 }),
        layer: layer as i32,
        reference_field: Some(mk_field("Reference", "U1", 100.0, 98.0)),
        definition: Some(kiapi::board::types::Footprint {
            items: vec![
                builders::pack_any(
                    &kiapi::board::types::Pad {
                        number: "1".to_string(),
                        position: Some(builders::vec2(99.0, 100.0)),
                        pad_stack: Some(kiapi::board::types::PadStack {
                            angle: Some(kiapi::common::types::Angle { value_degrees: 0.0 }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    "kiapi.board.types.Pad",
                ),
                mk_model("model.step", 1.5, 2.0, 3.0),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn read_models(
    fp: &kiapi::board::types::FootprintInstance,
) -> Vec<kiapi::board::types::Footprint3DModel> {
    fp.definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| {
            item.type_url
                .ends_with("kiapi.board.types.Footprint3DModel")
        })
        .map(|item| kiapi::board::types::Footprint3DModel::decode(item.value.as_slice()).unwrap())
        .collect()
}

/// KiCad's own flip transform: negate offset.y and rotate.x/rotate.y (Z and
/// offset.x ride along unchanged). Mirrors the closed-board fallback's
/// documented transform (`refuse_model_a_flip_would_move` in
/// copperpour-core/src/tools/pcb_components.rs) so the mock exercises the same
/// semantic KiCad's native FlipItems performs.
fn flip_model_in_place(fp: &mut kiapi::board::types::FootprintInstance) {
    let Some(definition) = fp.definition.as_mut() else {
        return;
    };
    for item in definition.items.iter_mut() {
        if !item
            .type_url
            .ends_with("kiapi.board.types.Footprint3DModel")
        {
            continue;
        }
        let mut model =
            kiapi::board::types::Footprint3DModel::decode(item.value.as_slice()).unwrap();
        if let Some(offset) = model.offset.as_mut() {
            offset.y_nm = -offset.y_nm;
        }
        if let Some(rotation) = model.rotation.as_mut() {
            rotation.x_nm = -rotation.x_nm;
            rotation.y_nm = -rotation.y_nm;
        }
        *item = builders::pack_any(&model, "kiapi.board.types.Footprint3DModel");
    }
}

/// What the mock does when it receives `FlipItems`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlipBehavior {
    /// Flip the footprint's layer and 3D model, return one ISC_OK result
    /// carrying the updated item, and commit.
    Success,
    /// `IRS_OK` envelope status with an EMPTY `flipped_items` list — the
    /// protocol-authorized false-success trap (10.0.6 board_commands.proto).
    OkButEmpty,
    /// One result, but for a KIID that does not match the request.
    WrongKiid,
    /// One result whose status is not ISC_OK.
    ItemFailed,
    /// KiCad answers AS_UNHANDLED for the whole request — authoritative
    /// "this KiCad predates FlipItems" evidence.
    Unhandled,
    /// FlipItems reports success and updates the mock's layer, but a
    /// SEPARATE bug leaves the readback (GetItems) layer unchanged — proves
    /// the readback guard, not the mutation response, is load-bearing.
    SucceedButReadbackDisagrees,
    /// The layer changes, but fresh readback contains an unreadable 3D-model
    /// payload. Malformed evidence must not be dropped and reported as an
    /// empty model list.
    SucceedButModelReadbackMalformed,
    /// The layer changes, but fresh readback omits a required model vector.
    /// Missing evidence must not be converted to a zero-valued transform.
    SucceedButModelReadbackMissingScale,
}

struct MockState {
    footprints: Vec<kiapi::board::types::FootprintInstance>,
    commit_actions: Vec<i32>,
    flip_calls: Vec<kiapi::board::commands::FlipItems>,
}

fn spawn_flip_mock(
    footprint: kiapi::board::types::FootprintInstance,
    behavior: FlipBehavior,
) -> (MockKicad, Arc<Mutex<MockState>>) {
    let state = Arc::new(Mutex::new(MockState {
        footprints: vec![footprint],
        commit_actions: Vec::new(),
        flip_calls: Vec::new(),
    }));
    let state_in_mock = state.clone();

    let mock = spawn_mock(move |req| {
        let msg = req.message.expect("request must pack a command");
        if msg.type_url.ends_with("GetOpenDocuments") {
            let resp = kiapi::common::commands::GetOpenDocumentsResponse {
                documents: vec![kiapi::common::types::DocumentSpecifier {
                    r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
                    project: Some(kiapi::common::types::ProjectSpecifier {
                        name: "konnect-mock".to_string(),
                        path: mock_project_dir().to_string(),
                    }),
                    identifier: Some(
                        kiapi::common::types::document_specifier::Identifier::BoardFilename(
                            "test.kicad_pcb".to_string(),
                        ),
                    ),
                }],
            };
            Some(reply_with(builders::pack_any(
                &resp,
                "kiapi.common.commands.GetOpenDocumentsResponse",
            )))
        } else if msg.type_url.ends_with("GetItems") {
            let held = state_in_mock.lock().unwrap();
            let resp = kiapi::common::commands::GetItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                items: held
                    .footprints
                    .iter()
                    .map(|fp| builders::pack_any(fp, "kiapi.board.types.FootprintInstance"))
                    .collect(),
            };
            Some(reply_with(builders::pack_any(
                &resp,
                "kiapi.common.commands.GetItemsResponse",
            )))
        } else if msg.type_url.ends_with("BeginCommit") {
            Some(reply_with(builders::pack_any(
                &kiapi::common::commands::BeginCommitResponse {
                    id: Some(kiapi::common::types::Kiid {
                        value: "flip-commit".to_string(),
                    }),
                },
                "kiapi.common.commands.BeginCommitResponse",
            )))
        } else if msg.type_url.ends_with("EndCommit") {
            let end = kiapi::common::commands::EndCommit::decode(msg.value.as_slice()).unwrap();
            state_in_mock
                .lock()
                .unwrap()
                .commit_actions
                .push(end.action);
            Some(reply_with(builders::pack_any(
                &kiapi::common::commands::EndCommitResponse {},
                "kiapi.common.commands.EndCommitResponse",
            )))
        } else if msg.type_url.ends_with("kiapi.board.commands.FlipItems") {
            let flip = kiapi::board::commands::FlipItems::decode(msg.value.as_slice()).unwrap();
            state_in_mock.lock().unwrap().flip_calls.push(flip.clone());

            match behavior {
                FlipBehavior::Unhandled => Some(error_response(
                    kiapi::common::ApiStatusCode::AsUnhandled,
                    "FlipItems is not handled by this endpoint",
                )),
                FlipBehavior::OkButEmpty => Some(reply_with(builders::pack_any(
                    &kiapi::board::commands::FlipItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        flipped_items: vec![],
                    },
                    "kiapi.board.commands.FlipItemsResponse",
                ))),
                FlipBehavior::WrongKiid => {
                    let mut held = state_in_mock.lock().unwrap();
                    let mut other = held.footprints[0].clone();
                    other.id = Some(kiapi::common::types::Kiid {
                        value: "not-the-requested-kiid".to_string(),
                    });
                    let item = builders::pack_any(&other, "kiapi.board.types.FootprintInstance");
                    let _ = &mut held; // keep board state untouched
                    Some(reply_with(builders::pack_any(
                        &kiapi::board::commands::FlipItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                                status: Some(kiapi::common::commands::ItemStatus {
                                    code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(item),
                            }],
                        },
                        "kiapi.board.commands.FlipItemsResponse",
                    )))
                }
                FlipBehavior::ItemFailed => Some(reply_with(builders::pack_any(
                    &kiapi::board::commands::FlipItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                            status: Some(kiapi::common::commands::ItemStatus {
                                code: kiapi::common::commands::ItemStatusCode::IscImmutable as i32,
                                error_message: "item is locked".to_string(),
                            }),
                            item: None,
                        }],
                    },
                    "kiapi.board.commands.FlipItemsResponse",
                ))),
                FlipBehavior::Success
                | FlipBehavior::SucceedButModelReadbackMalformed
                | FlipBehavior::SucceedButModelReadbackMissingScale => {
                    let mut held = state_in_mock.lock().unwrap();
                    let target_layer = match flip.direction() {
                        kiapi::board::commands::BoardFlipDirection::BfdTopBottom => {
                            if held.footprints[0].layer
                                == kiapi::board::types::BoardLayer::BlFCu as i32
                            {
                                kiapi::board::types::BoardLayer::BlBCu as i32
                            } else {
                                kiapi::board::types::BoardLayer::BlFCu as i32
                            }
                        }
                        _ => held.footprints[0].layer,
                    };
                    held.footprints[0].layer = target_layer;
                    flip_model_in_place(&mut held.footprints[0]);
                    if behavior == FlipBehavior::SucceedButModelReadbackMalformed {
                        let model = held.footprints[0]
                            .definition
                            .as_mut()
                            .unwrap()
                            .items
                            .iter_mut()
                            .find(|item| {
                                item.type_url
                                    .ends_with("kiapi.board.types.Footprint3DModel")
                            })
                            .unwrap();
                        model.value = vec![0xff, 0xff];
                    } else if behavior == FlipBehavior::SucceedButModelReadbackMissingScale {
                        let model = held.footprints[0]
                            .definition
                            .as_mut()
                            .unwrap()
                            .items
                            .iter_mut()
                            .find(|item| {
                                item.type_url
                                    .ends_with("kiapi.board.types.Footprint3DModel")
                            })
                            .unwrap();
                        let mut decoded =
                            kiapi::board::types::Footprint3DModel::decode(model.value.as_slice())
                                .unwrap();
                        decoded.scale = None;
                        *model = builders::pack_any(&decoded, "kiapi.board.types.Footprint3DModel");
                    }
                    let item = builders::pack_any(
                        &held.footprints[0],
                        "kiapi.board.types.FootprintInstance",
                    );
                    Some(reply_with(builders::pack_any(
                        &kiapi::board::commands::FlipItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                                status: Some(kiapi::common::commands::ItemStatus {
                                    code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(item),
                            }],
                        },
                        "kiapi.board.commands.FlipItemsResponse",
                    )))
                }
                FlipBehavior::SucceedButReadbackDisagrees => {
                    let held = state_in_mock.lock().unwrap();
                    // Respond as if the flip succeeded and the layer changed,
                    // WITHOUT actually mutating `held.footprints` — so a
                    // later GetItems still reports the original layer. This
                    // is exactly the divergence the readback guard exists to
                    // catch.
                    let mut echoed = held.footprints[0].clone();
                    echoed.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
                    let item = builders::pack_any(&echoed, "kiapi.board.types.FootprintInstance");
                    Some(reply_with(builders::pack_any(
                        &kiapi::board::commands::FlipItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                                status: Some(kiapi::common::commands::ItemStatus {
                                    code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                    error_message: String::new(),
                                }),
                                item: Some(item),
                            }],
                        },
                        "kiapi.board.commands.FlipItemsResponse",
                    )))
                }
            }
        } else if msg.type_url.ends_with("GetVersion") {
            Some(reply_with(builders::pack_any(
                &kiapi::common::commands::GetVersionResponse {
                    version: Some(kiapi::common::types::KiCadVersion {
                        major: 10,
                        minor: 0,
                        patch: 5,
                        full_version: "10.0.5".to_string(),
                    }),
                },
                "kiapi.common.commands.GetVersionResponse",
            )))
        } else {
            Some(ok_response())
        }
    });

    (mock, state)
}

#[test]
fn flip_sends_top_bottom_direction_and_the_exact_kiid() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::Success);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Flipped { .. } => {}
        other => panic!("expected Flipped, got {other:?}"),
    }

    let calls = state.lock().unwrap().flip_calls.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].direction(),
        kiapi::board::commands::BoardFlipDirection::BfdTopBottom
    );
    assert_eq!(calls[0].items.len(), 1);
    assert_eq!(calls[0].items[0].value, "U1-kiid");
}

#[test]
fn flip_success_reports_layer_and_flipped_3d_model_and_leaves_the_pad_alone() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let original_models = read_models(&fp);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::Success);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    let IpcFlipOutcome::Flipped {
        reference,
        kiid,
        previous_layer,
        layer,
        already_on_layer,
        models,
    } = outcome
    else {
        panic!("expected Flipped");
    };
    assert_eq!(reference, "U1");
    assert_eq!(kiid, "U1-kiid");
    assert_eq!(previous_layer, "F.Cu");
    assert_eq!(layer, "B.Cu");
    assert!(!already_on_layer);

    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0],
        IpcFootprint3DModel {
            filename: "model.step".to_string(),
            offset_mm: IpcVector3 {
                x: 0.0,
                y: -original_models[0].offset.as_ref().unwrap().y_nm,
                z: 0.0
            },
            rotation_degrees: IpcVector3 {
                x: -original_models[0].rotation.as_ref().unwrap().x_nm,
                y: -original_models[0].rotation.as_ref().unwrap().y_nm,
                z: 0.0
            },
            scale: IpcVector3 {
                x: 1.0,
                y: 1.0,
                z: 1.0
            },
            visible: true,
            opacity: original_models[0].opacity,
        }
    );

    // Unrelated content (the pad) is untouched by the flip.
    let held = state.lock().unwrap();
    let pads: Vec<_> = held.footprints[0]
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|i| i.type_url.ends_with("kiapi.board.types.Pad"))
        .collect();
    assert_eq!(pads.len(), 1);
    let pad = kiapi::board::types::Pad::decode(pads[0].value.as_slice()).unwrap();
    assert_eq!(pad.number, "1");
}

#[test]
fn already_on_target_layer_is_a_readback_derived_noop() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlBCu);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::Success);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Flipped {
            already_on_layer, ..
        } => assert!(already_on_layer),
        other => panic!("expected Flipped no-op, got {other:?}"),
    }

    // No FlipItems call was made, and no commit was opened/closed for it.
    let held = state.lock().unwrap();
    assert!(held.flip_calls.is_empty());
    assert!(held.commit_actions.is_empty());
}

#[test]
fn irs_ok_with_empty_flipped_items_is_not_treated_as_success() {
    // The protocol-authorized false-success trap (10.0.6 board_commands.proto
    // FlipItemsResponse.status doc comment): IRS_OK does not mean anything
    // was flipped.
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::OkButEmpty);
    let client = KiCadIpcClient::new(&mock.url);

    let error = client.flip_footprint("U1", "B.Cu").unwrap_err();
    assert!(format!("{error:#}").contains("0 flip results"), "{error:#}");

    // The commit must have been rolled back, not pushed.
    let actions = state.lock().unwrap().commit_actions.clone();
    assert_eq!(
        actions,
        vec![kiapi::common::commands::CommitAction::CmaDrop as i32]
    );
}

#[test]
fn wrong_kiid_in_the_flip_result_is_a_hard_failure_and_rolls_back() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::WrongKiid);
    let client = KiCadIpcClient::new(&mock.url);

    let error = client.flip_footprint("U1", "B.Cu").unwrap_err();
    assert!(
        format!("{error:#}").contains("does not match requested KIID"),
        "{error:#}"
    );
    let actions = state.lock().unwrap().commit_actions.clone();
    assert_eq!(
        actions,
        vec![kiapi::common::commands::CommitAction::CmaDrop as i32]
    );
}

#[test]
fn a_non_ok_per_item_status_is_a_hard_failure_and_rolls_back() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::ItemFailed);
    let client = KiCadIpcClient::new(&mock.url);

    let error = client.flip_footprint("U1", "B.Cu").unwrap_err();
    assert!(format!("{error:#}").contains("item is locked"), "{error:#}");
    let actions = state.lock().unwrap().commit_actions.clone();
    assert_eq!(
        actions,
        vec![kiapi::common::commands::CommitAction::CmaDrop as i32]
    );
}

#[test]
fn as_unhandled_is_reported_as_unsupported_with_the_observed_version() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, _state) = spawn_flip_mock(fp, FlipBehavior::Unhandled);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Unsupported { kicad_version } => {
            let version = kicad_version.expect("mock serves GetVersion");
            assert_eq!(version.full_version, "10.0.5");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn a_mutation_that_looks_ok_but_fails_readback_is_uncertain_not_success() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, _state) = spawn_flip_mock(fp, FlipBehavior::SucceedButReadbackDisagrees);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Uncertain {
            reference,
            requested_layer,
            ..
        } => {
            assert_eq!(reference, "U1");
            assert_eq!(requested_layer, "B.Cu");
        }
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[test]
fn malformed_post_flip_model_evidence_is_uncertain_not_an_empty_success() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, _state) = spawn_flip_mock(fp, FlipBehavior::SucceedButModelReadbackMalformed);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Uncertain { reason, .. } => {
            assert!(reason.contains("3D-model state"), "{reason}");
            assert!(reason.contains("not valid"), "{reason}");
        }
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[test]
fn missing_post_flip_model_vector_is_uncertain_not_zero_evidence() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let (mock, _state) = spawn_flip_mock(fp, FlipBehavior::SucceedButModelReadbackMissingScale);
    let client = KiCadIpcClient::new(&mock.url);

    let outcome = client.flip_footprint("U1", "B.Cu").unwrap();
    match outcome {
        IpcFlipOutcome::Uncertain { reason, .. } => {
            assert!(reason.contains("3D-model state"), "{reason}");
            assert!(reason.contains("no scale readback"), "{reason}");
        }
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[test]
fn round_trip_f_cu_b_cu_f_cu_restores_layer_model_and_unrelated_content() {
    let fp = mk_footprint("U1-kiid", kiapi::board::types::BoardLayer::BlFCu);
    let starting_models = read_models(&fp);
    let (mock, state) = spawn_flip_mock(fp, FlipBehavior::Success);
    let client = KiCadIpcClient::new(&mock.url);

    // F.Cu -> B.Cu
    let to_back = client.flip_footprint("U1", "B.Cu").unwrap();
    let IpcFlipOutcome::Flipped {
        layer: back_layer,
        models: back_models,
        ..
    } = to_back
    else {
        panic!("expected Flipped");
    };
    assert_eq!(back_layer, "B.Cu");
    assert_eq!(
        back_models[0].offset_mm.y,
        -starting_models[0].offset.as_ref().unwrap().y_nm
    );

    // B.Cu -> F.Cu: the semantic round trip the accepted #604 contract
    // requires.
    let to_front = client.flip_footprint("U1", "F.Cu").unwrap();
    let IpcFlipOutcome::Flipped {
        layer: front_layer,
        models: front_models,
        ..
    } = to_front
    else {
        panic!("expected Flipped");
    };
    assert_eq!(front_layer, "F.Cu");
    assert_eq!(front_models, starting_models_as_ipc(&starting_models));

    // Unrelated content (the pad) is byte-for-byte the same as it started.
    let held = state.lock().unwrap();
    let pads: Vec<_> = held.footprints[0]
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|i| i.type_url.ends_with("kiapi.board.types.Pad"))
        .map(|i| kiapi::board::types::Pad::decode(i.value.as_slice()).unwrap())
        .collect();
    assert_eq!(pads.len(), 1);
    assert_eq!(pads[0].number, "1");
    assert_eq!(
        pads[0].position.as_ref().unwrap(),
        &builders::vec2(99.0, 100.0)
    );
}

fn starting_models_as_ipc(
    models: &[kiapi::board::types::Footprint3DModel],
) -> Vec<IpcFootprint3DModel> {
    models
        .iter()
        .map(|model| IpcFootprint3DModel {
            filename: model.filename.clone(),
            offset_mm: IpcVector3 {
                x: model.offset.as_ref().unwrap().x_nm,
                y: model.offset.as_ref().unwrap().y_nm,
                z: model.offset.as_ref().unwrap().z_nm,
            },
            rotation_degrees: IpcVector3 {
                x: model.rotation.as_ref().unwrap().x_nm,
                y: model.rotation.as_ref().unwrap().y_nm,
                z: model.rotation.as_ref().unwrap().z_nm,
            },
            scale: IpcVector3 {
                x: model.scale.as_ref().unwrap().x_nm,
                y: model.scale.as_ref().unwrap().y_nm,
                z: model.scale.as_ref().unwrap().z_nm,
            },
            visible: model.visible,
            opacity: model.opacity,
        })
        .collect()
}
