//! Served #72 acceptance through authenticated HTTP bridge and IPC doubles.
use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::pcb_board::board_mock::{
    spawn_kicad_holding_board, spawn_kicad_holding_items, ItemStore,
};
use crate::tools::ServerConfig;
use copperpour_ipc::builders::{distance, net, pack_any, vec2};
use copperpour_ipc::gen::kiapi;
use prost::Message;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

const CIRCLE: &str = include_str!("../../tests/fixtures/specctra_native_circle.kicad_pcb");
const CIRCLE_DSN: &str =
    include_str!("../../tests/fixtures/specctra_native_circle.native-kicad-10.dsn");
const SES: &str = include_str!(
    "../../tests/fixtures/specctra_native_circle.native-kicad-10.freerouting-2.5.0.ses"
);
const APPLIED: &str =
    include_str!("../../tests/fixtures/specctra_native_circle.native-applied.kicad_pcb");
const PARTIAL_TH: &str = include_str!("../../tests/fixtures/specctra_native_partial_th.kicad_pcb");
const PARTIAL_TH_DSN_RAW: &str =
    include_str!("../../tests/fixtures/specctra_native_partial_th.native-kicad-10.dsn");

/// Strip header courtyard polygons (`(outline (polygon ...))`) not yet read by
/// `topola_specctra`, matching `specctra::tests::four_layers_native`.
fn partial_th_dsn() -> &'static str {
    Box::leak(
        PARTIAL_TH_DSN_RAW
            .lines()
            .filter(|line| !line.trim_start().starts_with("(outline (polygon "))
            .map(|line| format!("{line}\n"))
            .collect::<String>()
            .into_boxed_str(),
    )
}
const PARTIAL_TH_SES: &str = include_str!(
    "../../tests/fixtures/specctra_native_partial_th.native-kicad-10.freerouting-2.5.0.ses"
);
const PARTIAL_TH_APPLIED: &str =
    include_str!("../../tests/fixtures/specctra_native_partial_th.native-applied.kicad_pcb");
const RECT: &str = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
const RECT_DSN: &str =
    include_str!("../../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");

fn default_class() -> kiapi::common::project::NetClass {
    kiapi::common::project::NetClass {
        name: "Default".into(),
        board: Some(kiapi::common::project::NetClassBoardSettings {
            clearance: Some(distance(0.2)),
            track_width: Some(distance(0.2)),
            via_stack: Some(kiapi::board::types::PadStack {
                drill: Some(kiapi::board::types::DrillProperties {
                    diameter: Some(vec2(0.3, 0.3)),
                    ..Default::default()
                }),
                copper_layers: vec![kiapi::board::types::PadStackLayer {
                    size: Some(vec2(0.6, 0.6)),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn call(handler: &McpHandler, name: &str, args: Value) -> (bool, Value) {
    let response = handler.handle_message(json!({"jsonrpc":"2.0","id":72,"method":"tools/call","params":{"name":name,"arguments":args}})).await.unwrap();
    assert!(response.error.is_none(), "{response:?}");
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    (
        result["isError"] == true,
        serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
    )
}

async fn bridge(
    root: &Path,
    board: &Path,
    dsn: &'static str,
    exports: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    use axum::{
        routing::{get, post},
        Json,
    };
    let token = "native-profile-test-token-000000000000";
    let pid = std::process::id();
    let status = move |headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"], format!("Bearer {token}"));
        Json(json!({"success":true,"protocol_version":1,"pid":pid,"native_specctra_export":true}))
    };
    let path = root.join("native.dsn");
    let board = board.to_path_buf();
    let export = move |headers: axum::http::HeaderMap, Json(args): Json<Value>| {
        let path = path.clone();
        let board = board.clone();
        let exports = exports.clone();
        async move {
            assert_eq!(headers["authorization"], format!("Bearer {token}"));
            assert_eq!(args["expected_board"], board.to_string_lossy().as_ref());
            exports.fetch_add(1, Ordering::SeqCst);
            std::fs::write(&path, dsn).unwrap();
            Json(
                json!({"success":true,"protocol_version":1,"pid":pid,"board_path":board,"dsn_path":path,"dsn_bytes":dsn.len()}),
            )
        }
    };
    let app = axum::Router::new()
        .route("/v1/status", get(status))
        .route("/v1/export-specctra-dsn", post(export));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::fs::write(root.join("bridge-test.json"),json!({"protocol_version":1,"pid":pid,"address":format!("http://{}",listener.local_addr().unwrap()),"token":token,"started_at_unix":1.0}).to_string()).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

async fn scenario(
    root: &Path,
    label: &str,
    source: &'static str,
    dsn: Option<&'static str>,
    mode: &str,
    custom_rules: bool,
    unstable: bool,
) -> (bool, Value, usize, bool, bool) {
    let registration = root.join("bridge-test.json");
    if registration.exists() {
        std::fs::remove_file(registration).unwrap();
    }
    let case = root.join(label);
    std::fs::create_dir_all(&case).unwrap();
    let board = case.join("board.kicad_pcb");
    std::fs::write(&board, source).unwrap();
    if custom_rules {
        std::fs::write(board.with_extension("kicad_dru"), "(version 1)\n").unwrap();
    }
    let output = case.join("board.dsn");
    let calls = Arc::new(AtomicUsize::new(0));
    let bridge_task = match dsn {
        Some(dsn) => Some(bridge(root, &board, dsn, calls.clone()).await),
        None => None,
    };
    let observed = calls.clone();
    let ipc = spawn_kicad_holding_board(&board, move |command| {
        Some(match command.type_url.rsplit('.').next().unwrap() {
            "SaveDocumentToString" => pack_any(
                &kiapi::common::commands::SavedDocumentResponse {
                    contents: if unstable && observed.load(Ordering::SeqCst) > 0 {
                        format!("{source}\n")
                    } else {
                        source.into()
                    },
                    document: None,
                },
                "kiapi.common.commands.SavedDocumentResponse",
            ),
            "GetNets" => pack_any(
                &kiapi::board::commands::NetsResponse {
                    nets: vec![net("GND", 1), net("VCC", 2)],
                },
                "kiapi.board.commands.NetsResponse",
            ),
            "GetNetClassForNets" => pack_any(
                &kiapi::board::commands::NetClassForNetsResponse {
                    classes: ["GND", "VCC"]
                        .into_iter()
                        .map(|n| (n.into(), default_class()))
                        .collect(),
                },
                "kiapi.board.commands.NetClassForNetsResponse",
            ),
            _ => return None,
        })
    });
    let handler = McpHandler::new(ServerConfig {
        ipc_address: ipc.address().into(),
        auto_load_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let (error, body) = call(
        &handler,
        "export_specctra_dsn",
        json!({"board":board,"output":output,"native_bridge_mode":mode}),
    )
    .await;
    let manifest = output.with_extension("dsn.copperpour.json");
    if !error {
        assert_eq!(
            body["source_sha256"],
            crate::hex::sha256_hex(source.as_bytes())
        );
        let written = std::fs::read_to_string(&output).unwrap();
        if mode != "disable" && body["method"] == "kicad10_native_actionplugin" {
            assert_eq!(written, dsn.unwrap());
            let plan = crate::specctra_ses::parse_import_plan(
                &board,
                source,
                &std::fs::read_to_string(&manifest).unwrap(),
                SES,
            );
            if source == CIRCLE {
                assert_eq!(plan.unwrap().tracks.len(), 5);
            }
        }
    }
    assert_eq!(std::fs::read_to_string(board).unwrap(), source);
    if let Some(task) = bridge_task {
        task.abort();
        let _ = task.await;
    }
    (
        error,
        body,
        calls.load(Ordering::SeqCst),
        output.exists(),
        manifest.exists(),
    )
}

fn run_isolated_native_profile(test_name: &str, group: &str) {
    const CHILD: &str = "COPPERPOUR_NATIVE_PROFILE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("tools::pcb_export::native_specctra_order_tests::{test_name}"),
                "--nocapture",
            ])
            .env(CHILD, group)
            .env("COPPERPOUR_BRIDGE_DIR", temp.path())
            .env_remove("KONNECT_BRIDGE_DIR")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert_eq!(std::env::var(CHILD).unwrap(), group);
    let root = PathBuf::from(std::env::var_os("COPPERPOUR_BRIDGE_DIR").unwrap())
        .canonicalize()
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        match group {
            "native-first" => {
                for mode in ["require", "prefer"] {
                    let (error, body, count, dsn, manifest) = scenario(
                        &root,
                        &format!("circle-{mode}"),
                        CIRCLE,
                        Some(CIRCLE_DSN),
                        mode,
                        false,
                        false,
                    )
                    .await;
                    assert!(!error, "{body}");
                    assert_eq!(body["method"], "kicad10_native_actionplugin");
                    assert_eq!(count, 1);
                    assert!(dsn && manifest);
                }
            }
            "rust-only" => {
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "circle-disabled",
                    CIRCLE,
                    Some(CIRCLE_DSN),
                    "disable",
                    false,
                    false,
                )
                .await;
                assert!(error && body.to_string().contains("gr_circle"), "{body}");
                assert_eq!(count, 0);
                assert!(!dsn && !manifest);
            }
            "required-unavailable" => {
                let (error, body, _, dsn, manifest) = scenario(
                    &root,
                    "circle-required-unavailable",
                    CIRCLE,
                    None,
                    "require",
                    false,
                    false,
                )
                .await;
                assert!(
                    error && body.to_string().contains("required but unavailable"),
                    "{body}"
                );
                assert!(!body.to_string().contains("gr_circle"));
                assert!(!dsn && !manifest);
            }
            "fallback" => {
                let (error, body, _, dsn, manifest) = scenario(
                    &root,
                    "circle-preferred-unavailable",
                    CIRCLE,
                    None,
                    "prefer",
                    false,
                    false,
                )
                .await;
                assert!(
                    error
                        && body.to_string().contains("Rust fallback refused")
                        && body.to_string().contains("gr_circle"),
                    "{body}"
                );
                assert!(!dsn && !manifest);
                let (error, body, _, dsn, manifest) = scenario(
                    &root,
                    "rect-preferred-unavailable",
                    RECT,
                    None,
                    "prefer",
                    false,
                    false,
                )
                .await;
                assert!(!error, "{body}");
                assert_eq!(body["method"], "kicad_ipc_snapshot");
                assert!(dsn && manifest);
            }
            "validation" => {
                let invalid: &'static str = Box::leak(
                    RECT_DSN
                        .replace("(place R1 100000 -50000", "(place R1 101000 -50000")
                        .into_boxed_str(),
                );
                assert_ne!(invalid, RECT_DSN);
                for mode in ["prefer", "require"] {
                    let (error, body, count, dsn, manifest) = scenario(
                        &root,
                        &format!("invalid-native-{mode}"),
                        RECT,
                        Some(invalid),
                        mode,
                        false,
                        false,
                    )
                    .await;
                    assert_eq!(count, 1);
                    if mode == "require" {
                        assert!(
                            error && body.to_string().contains("semantic validation failed"),
                            "{body}"
                        );
                        assert!(!dsn && !manifest);
                    } else {
                        assert!(!error, "{body}");
                        assert_eq!(body["method"], "kicad_ipc_snapshot");
                        assert!(!body["native_bridge_diagnostics"]
                            .as_array()
                            .unwrap()
                            .is_empty());
                        assert!(dsn && manifest);
                    }
                }
            }
            "shared" => {
                for mode in ["disable", "prefer", "require"] {
                    let (error, body, count, dsn, manifest) = scenario(
                        &root,
                        &format!("custom-{mode}"),
                        CIRCLE,
                        Some(CIRCLE_DSN),
                        mode,
                        true,
                        false,
                    )
                    .await;
                    assert!(
                        error && body.to_string().contains("revision-bound SES import"),
                        "{body}"
                    );
                    assert_eq!(count, 0);
                    assert!(!dsn && !manifest);
                }
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "unlocked-routing",
                    APPLIED,
                    Some(CIRCLE_DSN),
                    "require",
                    false,
                    false,
                )
                .await;
                assert!(error && body.to_string().contains("not locked"), "{body}");
                assert_eq!(count, 0);
                assert!(!dsn && !manifest);
            }
            "revision" => {
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "changed-revision",
                    CIRCLE,
                    Some(CIRCLE_DSN),
                    "require",
                    false,
                    true,
                )
                .await;
                assert!(error, "{body}");
                assert_eq!(body["error"]["kind"], "conflict");
                assert_eq!(count, 1);
                assert!(!dsn && !manifest);
            }
            "partial-th" => {
                let partial_th_native = partial_th_dsn();
                for mode in ["prefer", "require"] {
                    let (error, body, count, dsn, manifest) = scenario(
                        &root,
                        &format!("partial-th-{mode}"),
                        PARTIAL_TH,
                        Some(partial_th_native),
                        mode,
                        false,
                        false,
                    )
                    .await;
                    assert!(!error, "{body}");
                    assert_eq!(body["method"], "kicad10_native_actionplugin");
                    assert_eq!(count, 1);
                    assert!(dsn && manifest);
                }
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "partial-th-disabled",
                    PARTIAL_TH,
                    Some(partial_th_native),
                    "disable",
                    false,
                    false,
                )
                .await;
                assert!(
                    error
                        && body
                            .to_string()
                            .contains("Rust export requires a shape on every copper layer"),
                    "{body}"
                );
                assert_eq!(count, 0);
                assert!(!dsn && !manifest);
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "partial-th-fallback-unavailable",
                    PARTIAL_TH,
                    None,
                    "prefer",
                    false,
                    false,
                )
                .await;
                assert!(
                    error
                        && body.to_string().contains("Rust fallback refused")
                        && body
                            .to_string()
                            .contains("Rust export requires a shape on every copper layer"),
                    "{body}"
                );
                assert_eq!(count, 0);
                assert!(!dsn && !manifest);
                let foreign: &'static str = Box::leak(
                    PARTIAL_TH
                        .replacen(
                            r#"(layers "*.Mask" "F.Cu" "In1.Cu")"#,
                            r#"(layers "F.Cu" "In99.Cu" "*.Mask")"#,
                            1,
                        )
                        .into_boxed_str(),
                );
                assert_ne!(foreign, PARTIAL_TH);
                let (error, body, count, dsn, manifest) = scenario(
                    &root,
                    "partial-th-foreign-layer",
                    foreign,
                    Some(partial_th_native),
                    "require",
                    false,
                    false,
                )
                .await;
                assert!(
                    error
                        && body
                            .to_string()
                            .contains("unsupported through-hole pad J1-1: copper layers are"),
                    "{body}"
                );
                assert_eq!(count, 0);
                assert!(!dsn && !manifest);
            }
            _ => panic!("unknown native-profile test group"),
        }
    });
}

#[test]
fn native_profile_partial_through_hole_pads() {
    run_isolated_native_profile("native_profile_partial_through_hole_pads", "partial-th");
}

#[test]
fn native_profile_native_first_outside_rust() {
    run_isolated_native_profile("native_profile_native_first_outside_rust", "native-first");
}
#[test]
fn native_profile_disable_never_calls_bridge() {
    run_isolated_native_profile("native_profile_disable_never_calls_bridge", "rust-only");
}
#[test]
fn native_profile_required_unavailable_has_no_rust_fallback() {
    run_isolated_native_profile(
        "native_profile_required_unavailable_has_no_rust_fallback",
        "required-unavailable",
    );
}
#[test]
fn native_profile_prefer_fallback_obeys_rust_limits() {
    run_isolated_native_profile(
        "native_profile_prefer_fallback_obeys_rust_limits",
        "fallback",
    );
}
#[test]
fn native_profile_semantic_failure_is_not_native_success() {
    run_isolated_native_profile(
        "native_profile_semantic_failure_is_not_native_success",
        "validation",
    );
}
#[test]
fn native_profile_common_import_safety_precedes_bridge() {
    run_isolated_native_profile(
        "native_profile_common_import_safety_precedes_bridge",
        "shared",
    );
}
#[test]
fn native_profile_source_revision_is_rechecked() {
    run_isolated_native_profile("native_profile_source_revision_is_rechecked", "revision");
}

#[tokio::test]
async fn native_profile_invalid_mode_types_refuse_in_direct_and_served_calls() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("missing.kicad_pcb");
    let output = temp.path().join("job.dsn");
    let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(String::new());
    let handler = McpHandler::new(ServerConfig {
        auto_load_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    for value in [
        json!(null),
        json!(1),
        json!(true),
        json!([]),
        json!("invalid-mode"),
    ] {
        let args = json!({"board":board,"output":output,"native_bridge_mode":value});
        let result = handle_export_specctra_dsn(&args, &ctx)
            .await
            .expect("mode refusal before filesystem/IPC");
        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        assert!(call(&handler, "export_specctra_dsn", args).await.0);
        assert!(!output.exists());
        assert!(!output.with_extension("dsn.copperpour.json").exists());
    }
}

async fn apply_native_with_manifest(
    board_source: &'static str,
    native_dsn: &str,
    ses_source: &str,
    applied_source: &'static str,
    log_tag: &str,
    cli: Option<&str>,
) {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("board.kicad_pcb");
    std::fs::write(&board, board_source).unwrap();
    let rules: copperpour_ipc::IpcEffectiveRoutingRules = ["GND", "VCC"]
        .into_iter()
        .map(|n| {
            (
                n.into(),
                copperpour_ipc::IpcRoutingRules {
                    class_name: "Default".into(),
                    constituents: vec!["Default".into()],
                    track_width_mm: Some(0.2),
                    clearance_mm: Some(0.2),
                    via_diameter_mm: Some(0.6),
                    via_drill_mm: Some(0.3),
                },
            )
        })
        .collect();
    let export = crate::specctra::prepare_export(&board, board_source, &rules)
        .unwrap()
        .adopt_native(native_dsn.into())
        .unwrap();
    let manifest = temp.path().join("board.manifest.json");
    let ses = temp.path().join("board.ses");
    std::fs::write(&manifest, &export.manifest).unwrap();
    std::fs::write(&ses, ses_source).unwrap();
    let fake=crate::tools::cli::test_support::write_script(temp.path(),"drc-stub",
        "#!/bin/sh\nprev=\nfor a; do [ \"$prev\" = --output ] && out=\"$a\"; prev=\"$a\"; done\nprintf '%s' '{\"violations\":[],\"unconnected_items\":[],\"schematic_parity\":[]}' > \"$out\"\n", "@exit /b 1\r\n");
    let store = Arc::new(Mutex::new(ItemStore::default()));
    let commits = Arc::new(Mutex::new(Vec::new()));
    let observed_commits = commits.clone();
    let ipc = spawn_kicad_holding_items(
        &board,
        vec!["", "GND", "VCC"],
        store.clone(),
        |_| {},
        move |command, held| {
            if command.type_url.ends_with("BeginCommit") || command.type_url.ends_with("EndCommit")
            {
                observed_commits
                    .lock()
                    .unwrap()
                    .push(command.type_url.clone());
            }
            command.type_url.ends_with("SaveDocumentToString").then(|| {
                pack_any(
                    &kiapi::common::commands::SavedDocumentResponse {
                        contents: if held.creates == 0 {
                            board_source.into()
                        } else {
                            applied_source.into()
                        },
                        document: None,
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                )
            })
        },
    );
    let handler = McpHandler::new(ServerConfig {
        ipc_address: ipc.address().into(),
        kicad_cli: cli
            .map(str::to_string)
            .unwrap_or_else(|| fake.to_string_lossy().into_owned()),
        auto_load_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let candidate = temp.path().join("candidate.kicad_pcb");
    // A stale manifest must refuse before even opening a route transaction.
    let mut stale: Value = serde_json::from_str(&export.manifest).unwrap();
    stale["source_sha256"] = json!("0".repeat(64));
    std::fs::write(&manifest, stale.to_string()).unwrap();
    let (error,body)=call(&handler,"apply_specctra_ses",json!({"board":board,"ses_path":ses,"manifest_path":manifest,"candidate_output_path":candidate})).await;
    assert!(error, "{body}");
    assert_eq!(store.lock().unwrap().creates, 0);
    assert!(commits.lock().unwrap().is_empty());
    assert!(!candidate.exists());
    std::fs::write(&manifest, export.manifest).unwrap();
    let (error,body)=call(&handler,"apply_specctra_ses",json!({"board":board,"ses_path":ses,"manifest_path":manifest,"candidate_output_path":candidate})).await;
    assert!(!error, "{body}");
    assert_eq!(
        commits
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.ends_with("BeginCommit"))
            .count(),
        1
    );
    assert_eq!(
        commits
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.ends_with("EndCommit"))
            .count(),
        1
    );
    assert_eq!(body["undo_description"], "Import Freerouting SES");
    assert_eq!(std::fs::read_to_string(&candidate).unwrap(), applied_source);
    assert_eq!(std::fs::read_to_string(&board).unwrap(), board_source);
    // Compare the IPC-created geometry against KiCad's actual SES import, not
    // against the planner's own arithmetic. Native UUIDs/order need not agree.
    let tree = copperpour_sexp::parse_sexp(applied_source).unwrap();
    let mm_to_nm = |node: &copperpour_sexp::SexpNode, tag: &str, index: usize| {
        (node.find(tag).unwrap().get_f64(index).unwrap() * 1_000_000.0).round() as i64
    };
    let mut expected_tracks = tree
        .find_all("segment")
        .into_iter()
        .map(|segment| {
            let mut ends = [
                (mm_to_nm(segment, "start", 1), mm_to_nm(segment, "start", 2)),
                (mm_to_nm(segment, "end", 1), mm_to_nm(segment, "end", 2)),
            ];
            ends.sort();
            (
                ends,
                mm_to_nm(segment, "width", 1),
                segment.find_str("layer").unwrap().to_string(),
                segment.find_str("net").unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    let mut expected_vias = tree
        .find_all("via")
        .into_iter()
        .map(|via| {
            (
                mm_to_nm(via, "at", 1),
                mm_to_nm(via, "at", 2),
                mm_to_nm(via, "size", 1),
                mm_to_nm(via, "drill", 1),
                via.find_str("net").unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        body["created_count"],
        expected_tracks.len() + expected_vias.len()
    );
    let mut actual_tracks = Vec::new();
    let mut actual_vias = Vec::new();
    for item in &store.lock().unwrap().items {
        if item.type_url.ends_with("Via") {
            let via = kiapi::board::types::Via::decode(item.value.as_slice()).unwrap();
            let pos = via.position.unwrap();
            let stack = via.pad_stack.unwrap();
            let drill = stack.drill.unwrap().diameter.unwrap();
            let size = stack.copper_layers.first().unwrap().size.unwrap();
            actual_vias.push((
                pos.x_nm,
                pos.y_nm,
                size.x_nm,
                drill.x_nm,
                via.net.unwrap().name,
            ));
        } else {
            let track = kiapi::board::types::Track::decode(item.value.as_slice()).unwrap();
            let net = track.net.as_ref().unwrap().name.clone();
            let a = track.start.unwrap();
            let b = track.end.unwrap();
            let mut ends = [(a.x_nm, a.y_nm), (b.x_nm, b.y_nm)];
            ends.sort();
            actual_tracks.push((
                ends,
                track.width.unwrap().value_nm,
                copperpour_ipc::builders::layer_name(
                    kiapi::board::types::BoardLayer::try_from(track.layer).unwrap(),
                )
                .unwrap()
                .to_string(),
                net,
            ));
        }
    }
    expected_tracks.sort();
    actual_tracks.sort();
    assert_eq!(actual_tracks, expected_tracks);
    expected_vias.sort();
    actual_vias.sort();
    assert_eq!(actual_vias, expected_vias);
    if cli.is_some() {
        assert_eq!(body["drc"]["unconnected_items"], 0, "{body}");
        eprintln!("{log_tag}: {}", body["drc"]);
    }
}

#[tokio::test]
async fn native_profile_circle_manifest_applies_its_native_session() {
    apply_native_with_manifest(CIRCLE, CIRCLE_DSN, SES, APPLIED, "NATIVE_CIRCLE_DRC", None).await;
}

#[tokio::test]
#[ignore = "native KiCad CLI smoke; requires kicad-cli"]
async fn native_profile_circle_manifest_apply_with_native_drc() {
    let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".into());
    let version = std::process::Command::new(&cli)
        .arg("version")
        .output()
        .expect("native kicad-cli required");
    assert!(version.status.success());
    apply_native_with_manifest(
        CIRCLE,
        CIRCLE_DSN,
        SES,
        APPLIED,
        "NATIVE_CIRCLE_DRC",
        Some(&cli),
    )
    .await;
}

#[tokio::test]
async fn native_profile_partial_th_manifest_applies_its_native_session() {
    apply_native_with_manifest(
        PARTIAL_TH,
        partial_th_dsn(),
        PARTIAL_TH_SES,
        PARTIAL_TH_APPLIED,
        "NATIVE_PARTIAL_TH_DRC",
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "native KiCad CLI smoke; requires kicad-cli"]
async fn native_profile_partial_th_manifest_apply_with_native_drc() {
    let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".into());
    let version = std::process::Command::new(&cli)
        .arg("version")
        .output()
        .expect("native kicad-cli required");
    assert!(version.status.success());
    apply_native_with_manifest(
        PARTIAL_TH,
        partial_th_dsn(),
        PARTIAL_TH_SES,
        PARTIAL_TH_APPLIED,
        "NATIVE_PARTIAL_TH_DRC",
        Some(&cli),
    )
    .await;
}
