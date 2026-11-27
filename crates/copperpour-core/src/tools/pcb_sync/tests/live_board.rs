use super::*;

/// A complete #474 sync through the registered tool, not just the pure
/// planner. The mock speaks KiCad's real protobuf protocol and retains the
/// live board between requests, so the assertions below are an independent
/// readback of what the apply actually left behind.
#[tokio::test]
async fn issue_474_apply_preserves_every_board_only_object() {
    use crate::router::ToolRouter;
    use crate::tools::cli::test_support::write_script;
    use crate::tools::{ServerConfig, ToolContext};
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockBoard {
        footprints: Arc<Mutex<Vec<prost_types::Any>>>,
        zones: Arc<Mutex<Vec<prost_types::Any>>>,
    }

    fn ok_item_status() -> kiapi::common::commands::ItemStatus {
        kiapi::common::commands::ItemStatus {
            code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
            error_message: String::new(),
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("preserve.kicad_sch");
    let board = directory.path().join("preserve.kicad_pcb");
    let exported = directory.path().join("preserve.net");
    std::fs::write(
        &schematic,
        include_bytes!("../../../../tests/fixtures/structural_scans_kicad10.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        &board,
        include_bytes!("../../../../tests/fixtures/specctra_two_resistors.kicad_pcb"),
    )
    .unwrap();
    std::fs::write(
        &exported,
        ONE_RESISTOR
            .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
            .replace("/Power/VCC", "VCC"),
    )
    .unwrap();

    let unix_source = exported.to_string_lossy().replace('\'', "'\\''");
    let windows_source = exported.to_string_lossy();
    let cli = write_script(
        directory.path(),
        "fake-kicad-cli-sync",
        &format!(
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then\n    shift\n    cp '{unix_source}' \"$1\"\n    exit $?\n  fi\n  shift\ndone\nexit 2\n"
        ),
        &format!(
            "@echo off\r\n:loop\r\nif \"%~1\"==\"\" exit /b 2\r\nif \"%~1\"==\"--output\" goto found\r\nshift\r\ngoto loop\r\n:found\r\nshift\r\ncopy /Y \"{windows_source}\" \"%~1\" >nul\r\nexit /b %ERRORLEVEL%\r\n"
        ),
    );

    let schematic_backed = schematic_backed_resistor();
    let logo = copperpour_ipc::builders::pack_any(
        &board_only_instance("logo-live", "REF**"),
        "kiapi.board.types.FootprintInstance",
    );
    let fiducial = copperpour_ipc::builders::pack_any(
        &board_only_instance("fiducial-live", "REF**"),
        "kiapi.board.types.FootprintInstance",
    );
    let copper_zone = zone_item(GND_POUR);
    let keepout = zone_item(RULE_AREA);
    let copper_kind = kiapi::board::types::Zone::decode(copper_zone.value.as_slice())
        .expect("captured copper zone");
    let keepout_kind =
        kiapi::board::types::Zone::decode(keepout.value.as_slice()).expect("captured rule area");
    assert_eq!(
        copper_kind.r#type,
        kiapi::board::types::ZoneType::ZtCopper as i32
    );
    assert_eq!(
        keepout_kind.r#type,
        kiapi::board::types::ZoneType::ZtRuleArea as i32
    );
    let state = MockBoard {
        footprints: Arc::new(Mutex::new(vec![schematic_backed, logo, fiducial])),
        zones: Arc::new(Mutex::new(vec![copper_zone, keepout])),
    };
    let footprints_before = state.footprints.lock().unwrap().clone();
    let zones_before = state.zones.lock().unwrap().clone();
    let responder_state = state.clone();

    let server =
        crate::tools::pcb_board::board_mock::spawn_kicad_holding_board(&board, move |command| {
            if command.type_url.ends_with("SaveDocumentToString") {
                let request =
                    kiapi::common::commands::SaveDocumentToString::decode(command.value.as_slice())
                        .expect("snapshot request");
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::SavedDocumentResponse {
                        document: request.document,
                        contents: include_str!(
                            "../../../../tests/fixtures/specctra_two_resistors.kicad_pcb"
                        )
                        .into(),
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                ));
            }
            if command.type_url.ends_with("GetItems") {
                let request = kiapi::common::commands::GetItems::decode(command.value.as_slice())
                    .expect("GetItems request");
                let requested = request.types.first().copied().unwrap_or_default();
                let items = match kiapi::common::types::KiCadObjectType::try_from(requested) {
                    Ok(kiapi::common::types::KiCadObjectType::KotPcbFootprint) => {
                        responder_state.footprints.lock().unwrap().clone()
                    }
                    Ok(kiapi::common::types::KiCadObjectType::KotPcbZone) => {
                        responder_state.zones.lock().unwrap().clone()
                    }
                    _ => Vec::new(),
                };
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items,
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ));
            }
            if command.type_url.ends_with("GetNets") {
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::board::commands::NetsResponse {
                        nets: vec![
                            kiapi::board::types::Net {
                                code: Some(kiapi::board::types::NetCode { value: 1 }),
                                name: "/Power/VCC".to_string(),
                            },
                            kiapi::board::types::Net {
                                code: Some(kiapi::board::types::NetCode { value: 2 }),
                                name: "GND".to_string(),
                            },
                        ],
                    },
                    "kiapi.board.commands.NetsResponse",
                ));
            }
            if command.type_url.ends_with("GetBoundingBox") {
                return Some(crate::tools::pcb_board::board_mock::kicad_bounding_boxes(
                    command,
                    |_| (0.0, 0.0, 50.0, 40.0),
                ));
            }
            if command.type_url.ends_with("BeginCommit") {
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "commit-474".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                ));
            }
            if command.type_url.ends_with("UpdateItems") {
                let request =
                    kiapi::common::commands::UpdateItems::decode(command.value.as_slice())
                        .expect("UpdateItems request");
                let mut board_footprints = responder_state.footprints.lock().unwrap();
                let mut updated_items = Vec::new();
                for updated in request.items {
                    let updated_fp =
                        kiapi::board::types::FootprintInstance::decode(updated.value.as_slice())
                            .expect("updated footprint");
                    let updated_id = updated_fp.id.as_ref().expect("updated KIID").value.clone();
                    let position = board_footprints
                        .iter()
                        .position(|item| {
                            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                                .ok()
                                .and_then(|fp| fp.id)
                                .is_some_and(|id| id.value == updated_id)
                        })
                        .expect("existing update target");
                    board_footprints[position] = updated.clone();
                    updated_items.push(kiapi::common::commands::ItemUpdateResult {
                        status: Some(ok_item_status()),
                        item: Some(updated),
                    });
                }
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::UpdateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        updated_items,
                    },
                    "kiapi.common.commands.UpdateItemsResponse",
                ));
            }
            if command.type_url.ends_with("EndCommit") {
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                ));
            }
            if command.type_url.ends_with("GetVersion") {
                return Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetVersionResponse {
                        version: Some(kiapi::common::types::KiCadVersion {
                            major: 10,
                            minor: 0,
                            patch: 8,
                            full_version: "10.0.8".to_string(),
                        }),
                    },
                    "kiapi.common.commands.GetVersionResponse",
                ));
            }
            None
        });

    let router = Arc::new(ToolRouter::new());
    router.load("sch_export").await.expect("registered toolset");
    let tool = router
        .get_tool("update_pcb_from_schematic")
        .await
        .expect("registered sync tool");
    let context = Arc::new(ToolContext::new(
        ServerConfig {
            kicad_cli: cli.to_string_lossy().to_string(),
            kicad_binary: String::new(),
            ipc_address: server.address().to_string(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        router,
    ));
    let paths = serde_json::json!({
        "schematic": schematic.to_string_lossy(),
        "board": board.to_string_lossy(),
    });
    let dry_run = (tool.handler)(&paths, context.clone()).await.unwrap();
    let dry_run: serde_json::Value = serde_json::from_str(&match &dry_run.content[0] {
        ToolContent::Text { text } => text.clone(),
        _ => panic!("sync response was not JSON text"),
    })
    .unwrap();
    assert_eq!(dry_run["status"], "ready", "{dry_run:#}");
    let apply = serde_json::json!({
        "schematic": schematic.to_string_lossy(),
        "board": board.to_string_lossy(),
        "dry_run": false,
        "expected_plan_revision": dry_run["plan_revision"],
    });
    let applied = (tool.handler)(&apply, context).await.unwrap();
    let applied: serde_json::Value = serde_json::from_str(&match &applied.content[0] {
        ToolContent::Text { text } => text.clone(),
        _ => panic!("sync response was not JSON text"),
    })
    .unwrap();
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["coverage"]["conflicts"]["planned"], 0);
    assert_eq!(applied["coverage"]["conflicts"]["applied"], 0);
    assert_eq!(applied["coverage"]["board_only_preserved"]["applied"], 2);

    let readback = copperpour_ipc::KiCadIpcClient::new(server.address().to_string());
    let document = readback
        .find_open_board(&board)
        .expect("the mock still holds the requested board");
    let footprints_after = readback
        .get_items_in(
            document.clone(),
            kiapi::common::types::KiCadObjectType::KotPcbFootprint,
        )
        .expect("footprint readback");
    let zones_after = readback
        .get_items_in(document, kiapi::common::types::KiCadObjectType::KotPcbZone)
        .expect("zone and rule-area readback");
    let before =
        kiapi::board::types::FootprintInstance::decode(footprints_before[0].value.as_slice())
            .expect("captured R1 before apply");
    let after =
        kiapi::board::types::FootprintInstance::decode(footprints_after[0].value.as_slice())
            .expect("captured R1 after apply");
    let before = board_footprint_from_instance(&before).expect("R1 identity before apply");
    let after = board_footprint_from_instance(&after).expect("R1 identity after apply");
    assert_eq!(before.value, "1k");
    assert_eq!(after.value, "10k");
    assert_eq!(after.footprint_id, before.footprint_id);
    assert_eq!(
        after.kiid, before.kiid,
        "the updated footprint keeps its KIID"
    );
    assert_eq!(
        after.position, before.position,
        "the updated footprint keeps its placement"
    );
    assert_eq!(
        &footprints_after[1..],
        &footprints_before[1..],
        "both board-only footprints must retain their complete protobuf identity and geometry"
    );
    assert_eq!(
        zones_after, zones_before,
        "the copper zone and keep-out/rule area must retain their complete protobuf identity and geometry"
    );
}

/// Without the option, the dry run and the apply name R3, the footprint
/// that lost its symbol, and not MH1, which is board-only; both stay.
#[tokio::test]
async fn footprints_without_symbols_are_named_in_the_dry_run_and_the_apply() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    let netlist = r1_on(STOCK_0603);

    let plan = served.dry_run(&netlist).await;
    assert_eq!(named_without_symbols(&plan), r3_named(false), "{plan:#}");
    assert_eq!(
        plan["footprints_without_symbols"][0]["reason"],
        "symbol_removed"
    );
    assert_eq!(plan["coverage"]["board_only_preserved"]["planned"], 2);
    assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(named_without_symbols(&applied), r3_named(false));
    assert_eq!(served.sent("DeleteItems"), 0);
    assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
}

/// R3 marked Exclude from board while its footprint is on the board: the
/// sync no longer refuses, and treats the footprint as KiCad's Update PCB
/// does, as one with no symbol (#870). The dry run names it with
/// `excluded_from_board`; with `delete_footprints_without_symbols` it is
/// deleted in the sync's commit.
#[tokio::test]
async fn an_excluded_part_s_footprint_is_named_and_deleted_on_request() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    let netlist = excluding_r3(&served);

    let plan = served.dry_run(&netlist).await;
    assert_ne!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(
        plan["coverage"]["skipped_by_flag"]["planned"], 1,
        "{plan:#}"
    );
    assert_eq!(named_without_symbols(&plan), r3_named(false), "{plan:#}");
    assert_eq!(
        plan["footprints_without_symbols"][0]["reason"],
        "excluded_from_board"
    );

    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    let deletions = plan["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["kind"] == "delete")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(deletions.len(), 1, "{plan:#}");
    assert_eq!(deletions[0]["reference"], "R3");
    assert_eq!(deletions[0]["reason"], "excluded_from_board");

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = served.sync(&netlist, apply).await;
    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
    assert_eq!(served.sent("DeleteItems"), 1);
    assert_eq!(held_references(&served), ["R1", "MH1"]);
}

/// With `delete_footprints_without_symbols`, R3 is a planned `delete`
/// change, and the apply deletes it in the sync's one commit. MH1 stays.
#[tokio::test]
async fn footprints_without_symbols_are_deleted_in_the_same_commit_on_request() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    let netlist = r1_on(STOCK_0603);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });

    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    let deletions = plan["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["kind"] == "delete")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(deletions.len(), 1, "{plan:#}");
    assert_eq!(deletions[0]["kiid"], "r3-kiid");
    assert_eq!(deletions[0]["reference"], "R3");
    assert_eq!(deletions[0]["reason"], "symbol_removed");
    assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 1);
    assert_eq!(plan["coverage"]["board_only_preserved"]["planned"], 1);
    assert_eq!(named_without_symbols(&plan), []);

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = served.sync(&netlist, apply).await;
    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    assert_eq!(served.sent("BeginCommit"), 1);
    assert_eq!(served.sent("DeleteItems"), 1);
    assert_eq!(held_references(&served), ["R1", "MH1"]);
}

/// A dry run without the option plans no deletion, so its revision does
/// not apply with the option: the apply is stale, deletes nothing, and
/// names R3 as kept.
#[tokio::test]
async fn a_plan_made_without_the_option_deletes_nothing() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    let netlist = r1_on(STOCK_0603);
    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    let (_, refused) = served
        .sync(
            &netlist,
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
                "delete_footprints_without_symbols": true,
            }),
        )
        .await;

    assert_eq!(refused["status"], "conflict", "{refused:#}");
    assert_eq!(refused["diagnostics"][0]["code"], "stale_plan_revision");
    assert_eq!(refused["coverage"]["footprints_deleted"]["planned"], 0);
    assert_eq!(named_without_symbols(&refused), r3_named(false));
    assert_eq!(served.sent("DeleteItems"), 0);
    assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
}

/// A locked R3 is kept with a `footprint_locked_not_deleted` note that
/// refuses nothing; the rest of the plan applies.
#[tokio::test]
async fn a_locked_footprint_without_a_symbol_is_kept_and_noted() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(true)).await;
    let netlist = r1_on(STOCK_0603);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });

    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);
    assert_eq!(named_without_symbols(&plan), r3_named(true));
    let notes = plan["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["code"] == "footprint_locked_not_deleted")
        .collect::<Vec<_>>();
    assert_eq!(notes.len(), 1, "{plan:#}");
    assert_eq!(notes[0]["reference"], "R3");
    assert_eq!(plan["coverage"]["conflicts"]["planned"], 0);

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = served.sync(&netlist, apply).await;
    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(served.sent("DeleteItems"), 0);
    assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
}

/// KiCad before 10.0.7 answers a delete with no per-item results, so a
/// footprint it kept would pass unnoticed. The board is read back after
/// the commit: R3 still on it is reported, counted as kept rather than
/// deleted, and listed with the kept footprints.
#[tokio::test]
async fn a_footprint_kicad_kept_after_its_deletion_is_reported() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    served
        .keeps_deleted
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let netlist = r1_on(STOCK_0603);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (_, applied) = served.sync(&netlist, apply).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(served.sent("DeleteItems"), 1);
    let differs = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["code"] == "board_readback_differs")
        .collect::<Vec<_>>();
    assert_eq!(differs.len(), 1, "{applied:#}");
    assert_eq!(differs[0]["reference"], "R3");
    assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 0);
    assert_eq!(applied["coverage"]["footprints_deleted"]["planned"], 1);
    assert_eq!(applied["coverage"]["board_only_preserved"]["applied"], 2);
    assert_eq!(named_without_symbols(&applied), r3_named(false));
}

/// KiCad 10.0.7+ reports each deleted item, and `delete_items_in` holds
/// the answer to the request: the deletion goes through on that path too.
#[tokio::test]
async fn a_deletion_kicad_reports_item_by_item_is_applied() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    served
        .reports_deleted
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let netlist = r1_on(STOCK_0603);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = served.sync(&netlist, apply).await;

    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    assert_eq!(held_references(&served), ["R1", "MH1"]);
}

/// A readback that fails after the commit refuses nothing: the commit
/// stands, so the apply answers `applied` with a note for each readback
/// it could not finish, as an error so it is not taken for a clean one.
#[tokio::test]
async fn a_readback_that_fails_after_the_commit_is_reported_on_the_applied_response() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    served
        .fails_readback
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let netlist = r1_on(STOCK_0603);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
    let (_, plan) = served.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = served.sync(&netlist, apply).await;

    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let unchecked = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["code"] == "apply_readback_unchecked")
        .count();
    // R1's replacement and R3's deletion are each read back.
    assert_eq!(unchecked, 2, "{applied:#}");
    assert_eq!(held_references(&served), ["R1", "MH1"]);
}

/// Each readback after the commit flags the response on its own: with
/// no deletion, the footprints KiCad was sent; with the first readback
/// answered, the footprints it deleted.
#[tokio::test]
async fn each_readback_that_fails_after_the_commit_flags_the_response() {
    let unchecked = |applied: &serde_json::Value| {
        applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "apply_readback_unchecked")
            .count()
    };

    let sent = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    sent.fails_readback
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let netlist = r1_on(STOCK_0603);
    let plan = sent.dry_run(&netlist).await;
    let (is_error, applied) = sent
        .sync(
            &netlist,
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }),
        )
        .await;
    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(unchecked(&applied), 1, "{applied:#}");

    // The readback of R1's replacement is answered; R3's deletion's fails.
    let deleted = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    deleted
        .fails_readback
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
    let (_, plan) = deleted.sync(&netlist, deleting.clone()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    let mut apply = deleting;
    apply["dry_run"] = serde_json::json!(false);
    apply["expected_plan_revision"] = plan["plan_revision"].clone();
    let (is_error, applied) = deleted.sync(&netlist, apply).await;
    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(unchecked(&applied), 1, "{applied:#}");
}

/// An explicit `false` is the default: nothing is deleted, R3 is named.
#[tokio::test]
async fn delete_footprints_without_symbols_false_deletes_nothing() {
    let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
    let (_, plan) = served
        .sync(
            &r1_on(STOCK_0603),
            serde_json::json!({ "delete_footprints_without_symbols": false }),
        )
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert!(
        !plan["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["kind"] == "delete"),
        "{plan:#}"
    );
    assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);
    assert_eq!(named_without_symbols(&plan), r3_named(false));
}
