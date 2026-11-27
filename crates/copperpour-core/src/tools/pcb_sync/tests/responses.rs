use super::*;

/// #688 through the served boundary: a part the board lacks is staged
/// 5 mm to the right of what KiCad holds, measured item by item.
///
/// KiCad answers `GetBoundingBox` only for the KIIDs a request names. The
/// snapshot used to send an empty request, read the empty answer as an
/// empty board, and stage every addition beside the page origin instead.
#[tokio::test]
async fn additions_are_staged_beside_the_board_kicad_holds() {
    // An outline away from the origin, as a real board is.
    let served = ServedSync::holding_outline(Some((100.0, 80.0, 50.0, 40.0))).await;

    let plan = served
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    let added = &plan["changes"][0];
    assert_eq!(added["kind"], "add", "{plan:#}");
    let x = added["position"]["x"].as_f64().unwrap();
    let y = added["position"]["y"].as_f64().unwrap();
    // The staging column starts 5 mm past the board's right edge (150 mm)
    // and the first part is centred half its width further on.
    assert!(x > 155.0 && x < 165.0, "staged at x = {x}: {plan:#}");
    // Stacked down from the board's top edge (80 mm), not from y = 0.
    assert!(y > 80.0 && y < 90.0, "staged at y = {y}: {plan:#}");
}

/// #688's disclosure in its sync consumer. On a board KiCad 10.0.5 holds,
/// additions are staged beside what KiCad measured, and the dry run says
/// that is not the whole board: tables and generators were not listed.
#[tokio::test]
async fn staging_names_the_classes_kicad_would_not_list() {
    let served = ServedSync::refusing(Some((100.0, 80.0, 50.0, 40.0)), kicad_10_refusals()).await;

    let plan = served
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["staging"],
        serde_json::json!({
            "basis": "partial_geometry",
            "unavailable_item_classes": [refused_class("tables"), refused_class("generators")],
        }),
        "{plan:#}"
    );
    // Still staged beside the measured outline.
    let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
    assert!(x > 155.0 && x < 165.0, "staged at x = {x}: {plan:#}");
}

/// On KiCad 10.0.7+, tables are listed and measured, so additions are
/// staged to the right of a table extending past the board outline and
/// `staging.unavailable_item_classes` names only generators.
#[tokio::test]
async fn staging_measures_tables_when_kicad_lists_them() {
    use copperpour_ipc::gen::kiapi::common::types::KiCadObjectType as Kind;

    let served = ServedSync::build(
        Some((100.0, 80.0, 50.0, 40.0)),
        vec![("board-table", (200.0, 80.0, 30.0, 20.0))],
        vec![Kind::KotPcbGenerator],
        Readback::AsSent,
        HeldBoard::default(),
    )
    .await;

    let plan = served
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["staging"],
        serde_json::json!({
            "basis": "partial_geometry",
            "unavailable_item_classes": [refused_class("generators")],
        }),
        "{plan:#}"
    );
    // Staged past the table's right edge (230 mm), not the outline's (150 mm).
    let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
    assert!(x > 235.0 && x < 245.0, "staged at x = {x}: {plan:#}");
}

/// A board where KiCad measured nothing but would not list some classes is
/// not called empty: it may hold tables or generators. Staging still starts
/// at the origin, and the response says why.
#[tokio::test]
async fn a_board_with_nothing_measured_is_not_reported_empty() {
    let served = ServedSync::refusing(None, kicad_10_refusals()).await;

    let plan = served
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["staging"]["basis"], "no_measured_geometry", "{plan:#}");
    assert_eq!(
        plan["staging"]["unavailable_item_classes"],
        serde_json::json!([refused_class("tables"), refused_class("generators")])
    );
    let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
    assert!(
        x > 5.0 && x < 15.0,
        "staged from the origin, x = {x}: {plan:#}"
    );
}

/// The controls: a KiCad that lists every class reports a board it measured
/// as complete, and one holding nothing as empty, with no classes named.
#[tokio::test]
async fn a_board_whose_every_class_was_listed_is_complete_or_empty() {
    let measured = ServedSync::holding_outline(Some((100.0, 80.0, 50.0, 40.0))).await;
    let plan = measured
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;
    assert_eq!(
        plan["staging"],
        serde_json::json!({ "basis": "complete_geometry", "unavailable_item_classes": [] }),
        "{plan:#}"
    );

    let empty = ServedSync::new().await;
    let plan = empty
        .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;
    assert_eq!(
        plan["staging"],
        serde_json::json!({ "basis": "empty_board", "unavailable_item_classes": [] }),
        "{plan:#}"
    );
}

/// A refusal before the board was read has no staging to report, and
/// says so with `null` rather than a basis it never measured.
#[test]
fn a_plan_that_never_reached_the_board_reports_no_staging() {
    let plan = plan_adding(Vec::new());
    let body =
        match &sync_response(&plan, "conflict", 1, false, &ResponseOptions::default()).content[0] {
            ToolContent::Text { text } => serde_json::from_str::<serde_json::Value>(text).unwrap(),
            _ => panic!("sync response was not JSON text"),
        };
    assert_eq!(body["staging"], serde_json::Value::Null);
}

/// A refusal before any plan exists (saved hierarchy, netlist export, IPC
/// preflight) went out with a hand-built diagnostic of only `code` and
/// `message`, so the responses that say least were also the ones missing
/// the fields every other diagnostic carries. One constructor builds them
/// all now. The whole object is compared: indexing a missing key yields
/// `null` as well, so a per-field check could not tell absent from null.
#[tokio::test]
async fn a_preflight_refusal_has_the_same_diagnostic_fields_through_the_served_dispatch() {
    let served = ServedSync::new().await;

    // An export with no components section fails the netlist preflight.
    let (is_error, refused) = served.dry_run_result("(export (version \"E\"))").await;

    assert!(is_error, "{refused:#}");
    assert_eq!(refused["status"], "conflict");
    let message = refused["diagnostics"][0]["message"].as_str().unwrap();
    assert!(message.starts_with("netlist preflight failed"), "{message}");
    assert_eq!(
        refused["diagnostics"],
        serde_json::json!([{
            "code": "preflight_conflict",
            "message": message,
            "reference": null,
            "references": [],
            "footprint_id": null
        }])
    );

    // The same keys as a diagnostic from planning, so one reader serves both.
    let planned = served
        .dry_run(&exported_netlist(&[("U1", TEXAS_VQFN, &["1"])]))
        .await;
    let keys = |diagnostic: &serde_json::Value| {
        diagnostic
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        keys(&refused["diagnostics"][0]),
        keys(&planned["diagnostics"][0])
    );
}

#[tokio::test]
async fn sync_reports_create_rejection_after_acknowledged_drop() {
    let served = ServedSync::reading_back(Readback::RefuseCreation).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);
    let before = std::fs::read(&served.board).unwrap();
    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan}");
    let (is_error, result) = served
        .sync(
            &netlist,
            serde_json::json!({
                "dry_run": false, "expected_plan_revision": plan["plan_revision"],
            }),
        )
        .await;
    assert!(is_error);
    let recovery = &result["recovery"];
    assert_eq!(recovery["cause"], "kicad_api_rejection", "{result}");
    assert_eq!(recovery["mutation_outcome"], "unknown");
    assert_eq!(recovery["evidence"]["rollback"], "confirmed");
    assert_eq!(
        recovery["evidence"]["rejected_items"][0]["status"],
        "ISC_INVALID_DATA"
    );
    assert_eq!(served.sent("CreateItems"), 1);
    assert_eq!(served.sent("EndCommit"), 1);
    assert_eq!(served.sent("UpdateItems"), 0);
    assert!(served.held.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&served.board).unwrap(), before);
}

#[tokio::test]
async fn a_dry_run_lists_the_first_limit_changes_and_counts_them_all() {
    let served = ServedSync::new().await;

    let (is_error, plan) = served
        .sync(&additions(3), serde_json::json!({ "limit": 1 }))
        .await;

    assert!(!is_error, "{plan:#}");
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 3, "{plan:#}");
    assert_eq!(plan["changes_shown"], 1);
    assert_eq!(plan["changes_truncated"], true);
    let changes = plan["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["reference"], "C1");
    assert_eq!(plan["coverage"]["footprints_added"]["planned"], 3);
}

/// The input schema takes an integral `1.0` as an integer, so the limit
/// does too.
#[tokio::test]
async fn an_integral_float_limit_is_the_limit() {
    let served = ServedSync::new().await;

    let (is_error, plan) = served
        .sync(&additions(3), serde_json::json!({ "limit": 1.0 }))
        .await;

    assert!(!is_error, "{plan:#}");
    assert_eq!(plan["changes_shown"], 1, "{plan:#}");
    assert_eq!(plan["changes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn fifty_changes_are_listed_when_no_limit_is_named() {
    let served = ServedSync::new().await;

    let plan = served.dry_run(&additions(51)).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 51);
    assert_eq!(plan["changes_shown"], 50);
    assert_eq!(plan["changes_truncated"], true);
    assert_eq!(plan["changes"].as_array().unwrap().len(), 50);
}

/// `json!` sorted the keys, so `status` and `plan_revision` came after
/// every change, past where a client cuts a long response.
#[tokio::test]
async fn the_verdict_and_plan_revision_come_before_the_changes() {
    let served = ServedSync::new().await;

    let (_, text) = served.sync_text(&additions(3), serde_json::json!({})).await;

    assert!(
        text.starts_with(r#"{"status":"ready","plan_revision":""#),
        "{text}"
    );
    let changes = text.find(r#""changes":["#).expect("changes");
    for key in [
        "coverage",
        "changes_count",
        "changes_shown",
        "changes_truncated",
        "staging",
        "undo",
        "diagnostics",
        "unassigned_footprints",
    ] {
        let at = text
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("no {key}: {text}"));
        assert!(at < changes, "{key} comes after the changes: {text}");
    }
}

#[tokio::test]
async fn the_output_file_holds_the_whole_response() {
    let served = ServedSync::new().await;
    let out = tempfile::tempdir().unwrap();
    // A directory that does not exist yet, as `run_drc` creates.
    let output = out.path().join("plans").join("sync.json");

    let (is_error, plan) = served
        .sync(
            &additions(3),
            serde_json::json!({ "limit": 1, "output": output.to_string_lossy() }),
        )
        .await;

    assert!(!is_error, "{plan:#}");
    assert_eq!(plan["changes_shown"], 1, "{plan:#}");
    let whole: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(whole["status"], plan["status"]);
    assert_eq!(whole["plan_revision"], plan["plan_revision"]);
    assert_eq!(whole["coverage"], plan["coverage"]);
    assert_eq!(
        (
            &whole["changes_count"],
            &whole["changes_shown"],
            &whole["changes_truncated"]
        ),
        (
            &serde_json::json!(3),
            &serde_json::json!(3),
            &serde_json::json!(false)
        )
    );
    let references = whole["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["reference"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(references, ["C1", "C2", "C3"]);
}

/// The bound is the response's only: the revision hashes the whole plan
/// and the apply sends all of it, and its file lists all of it.
#[tokio::test]
async fn a_bounded_dry_run_applies_its_whole_plan() {
    let served = ServedSync::new().await;
    let netlist = additions(3);
    let out = tempfile::tempdir().unwrap();
    let output = out.path().join("applied.json");

    let unbounded = served.dry_run(&netlist).await;
    let (_, bounded) = served
        .sync(&netlist, serde_json::json!({ "limit": 0 }))
        .await;
    assert_eq!(bounded["changes"], serde_json::json!([]), "{bounded:#}");
    assert_eq!(bounded["changes_count"], 3);
    assert_eq!(bounded["plan_revision"], unbounded["plan_revision"]);

    let (is_error, applied) = served
        .sync(
            &netlist,
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": bounded["plan_revision"],
                "limit": 0,
                "output": output.to_string_lossy(),
            }),
        )
        .await;

    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_added"]["applied"], 3);
    assert_eq!(applied["changes"], serde_json::json!([]));
    assert_eq!(applied["changes_count"], 3);
    assert_eq!(served.sent_footprints().len(), 3);
    let whole: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(whole["status"], "applied", "{whole:#}");
    assert_eq!(whole["changes"].as_array().unwrap().len(), 3);
}

/// An output that would replace the board or a schematic of the saved
/// hierarchy, or that cannot be a file, is refused before the board is
/// read, as `run_drc` refuses its report path.
#[tokio::test]
async fn an_output_over_the_board_a_schematic_or_a_file_is_refused_first() {
    let served = ServedSync::new().await;
    // A root with one sheet, so a sheet below the root is a source too.
    std::fs::write(
        &served.schematic,
        include_bytes!("../../../../tests/fixtures/single_pin_nets.kicad_sch"),
    )
    .unwrap();
    let sheet = served
        .schematic
        .with_file_name("single_pin_nets_child.kicad_sch");
    std::fs::write(
        &sheet,
        include_bytes!("../../../../tests/fixtures/single_pin_nets_child.kicad_sch"),
    )
    .unwrap();
    let sources = [served.board.clone(), served.schematic.clone(), sheet];
    let before = sources.clone().map(|path| std::fs::read(path).unwrap());
    let out = tempfile::tempdir().unwrap();
    let blocker = out.path().join("blocker");
    std::fs::write(&blocker, "a file, not a directory").unwrap();
    let netlist = additions(1);

    for output in sources
        .iter()
        .cloned()
        .chain([out.path().to_path_buf(), blocker.join("sync.json")])
    {
        for mut extra in [
            serde_json::json!({}),
            serde_json::json!({ "dry_run": false, "expected_plan_revision": "any" }),
        ] {
            extra["output"] = serde_json::json!(output.to_string_lossy());
            let (is_error, refused) = served.sync(&netlist, extra).await;

            assert!(is_error, "{}: {refused:#}", output.display());
            assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
            assert_eq!(refused["error"]["field"], "output", "{refused:#}");
        }
    }
    assert!(served.sent_footprints().is_empty());
    assert_eq!(sources.map(|path| std::fs::read(path).unwrap()), before);
}

/// A write that fails after the check, as on a full disk or a permission
/// change, is answered as `run_drc` answers a report it could not write:
/// with nothing applied the call fails and can be repeated; after an
/// apply it fails carrying the applied response.
#[test]
fn an_output_that_cannot_be_written_fails_the_call_and_keeps_an_applied_response() {
    let plan = plan_adding(vec![planned_add("C1", STOCK_0603, &["1", "2"])]);
    let out = tempfile::tempdir().unwrap();
    let blocker = out.path().join("blocker");
    std::fs::write(&blocker, "a file, not a directory").unwrap();
    let output = blocker.join("sync.json");
    let options = ResponseOptions {
        output: Some(output.clone()),
        ..ResponseOptions::default()
    };
    let body = |result: &CallToolResult| -> serde_json::Value {
        match &result.content[0] {
            ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
            _ => panic!("sync response was not JSON text"),
        }
    };
    let path = output.display().to_string();

    let dry_run = sync_response(&plan, "ready", 1, false, &options);
    assert!(dry_run.is_error);
    let refused = body(&dry_run);
    assert_eq!(refused["error"]["kind"], "handler_error", "{refused:#}");
    let reason = refused["error"]["reason"].as_str().unwrap();
    assert!(reason.contains(&path), "{reason}");

    let applied = sync_response(&plan, "applied", 1, true, &options);
    assert!(applied.is_error);
    let applied = body(&applied);
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["changes_count"], 1);
    assert_eq!(applied["coverage"]["conflicts"]["planned"], 0);
    let diagnostics = applied["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{applied:#}");
    assert_eq!(diagnostics[0]["code"], "output_not_written");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(message.contains(&path), "{message}");
    assert!(
        message.contains("do not apply this plan again"),
        "{message}"
    );
    assert!(!output.exists());
}

/// Refused by the input schema, before the handler runs: an empty
/// `output` would otherwise reach the writer and fail there instead.
#[tokio::test]
async fn a_negative_limit_or_an_empty_output_is_refused_by_the_schema() {
    let served = ServedSync::new().await;

    for (field, extra) in [
        ("limit", serde_json::json!({ "limit": -1 })),
        ("output", serde_json::json!({ "output": "" })),
    ] {
        let (is_error, refused) = served.sync(&additions(1), extra).await;

        assert!(is_error, "{refused:#}");
        assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
        assert_eq!(refused["error"]["field"], field, "{refused:#}");
    }
    assert!(served.sent_footprints().is_empty());
}
