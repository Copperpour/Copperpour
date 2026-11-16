//! Live KiCad check of native pad and via teardrop settings (#555).
//!
//! Ignored by default: it needs a running KiCad with its API enabled and a
//! disposable COPY of `fixtures/teardrops_kicad10.kicad_pcb` open, because it
//! changes settings, refills zones and, with `COPPERPOUR_CAPTURE_IPC_FIXTURE`,
//! saves the board. `live_kicad_test.rs`'s header says how to find the socket.
//!
//! ```sh
//! cp crates/copperpour-ipc/tests/fixtures/teardrops_kicad10.kicad_pcb /tmp/live/
//! # open /tmp/live/teardrops_kicad10.kicad_pcb in pcbnew, then:
//! KICAD_API_SOCKET=<the editor's socket> \
//! COPPERPOUR_LIVE_KICAD_BOARD=/tmp/live/teardrops_kicad10.kicad_pcb \
//! cargo test -p copperpour-ipc --test live_teardrop_test -- --ignored --nocapture
//! ```
//!
//! On a KiCad older than 10.0.7 the test instead proves why the tool layer
//! gates on the version: the editor sends no teardrop settings, and a settings
//! update it is sent is accepted and silently dropped.
//!
//! `COPPERPOUR_TEARDROP_EVIDENCE=<path>` writes what was observed as JSON.

use copperpour_ipc::client::{
    legacy_teardrops_in_board_text, supports_teardrop_settings, IpcTeardropApplyOutcome,
    IpcTeardropEdit, IpcTeardropSettings, IpcTeardropTarget, IpcTeardropTargetKind,
    IpcTeardropZone, KiCadIpcClient, TeardropReadError,
};
use copperpour_ipc::gen::kiapi;
use copperpour_sexp::{parse_sexp, SexpNode};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

mod teardrop_common;
use teardrop_common::{file_items, file_settings};

const CAPTURED_BOARD: &str = "teardrops_kicad10.kicad_pcb";

fn test_env(name: &str) -> Option<String> {
    ["COPPERPOUR", "KONNECT"]
        .iter()
        .find_map(|app| std::env::var_os(format!("{app}_{name}")).filter(|v| !v.is_empty()))
        .and_then(|value| value.into_string().ok())
}

/// The settings this test asks for: every field away from KiCad's default,
/// so a field the editor drops shows up in the read-back.
fn requested() -> IpcTeardropSettings {
    IpcTeardropSettings {
        enabled: true,
        curved_edges: true,
        allow_multiple_track_segments: false,
        prefer_zone_connection: false,
        max_length_nm: Some(1_500_000),
        max_width_nm: None,
        best_length_ratio: 0.6,
        best_width_ratio: 0.9,
        max_track_width_ratio: 0.8,
    }
}

/// Whether a node is a KiCad-generated teardrop zone: `(zone … (attr
/// (teardrop …)))`.
fn is_teardrop_zone(node: &SexpNode) -> bool {
    node.head() == Some("zone")
        && node
            .find("attr")
            .is_some_and(|attr| attr.find("teardrop").is_some())
}

/// A board with every `(teardrops …)` settings list and every generated
/// teardrop zone removed, at any depth.
fn without_teardrops(node: &SexpNode) -> SexpNode {
    match node {
        SexpNode::List(children) => SexpNode::List(
            children
                .iter()
                .filter(|child| child.head() != Some("teardrops") && !is_teardrop_zone(child))
                .map(without_teardrops)
                .collect(),
        ),
        other => other.clone(),
    }
}

fn teardrop_zone_count(text: &str) -> usize {
    parse_sexp(text)
        .unwrap()
        .find_all("zone")
        .into_iter()
        .filter(|zone| is_teardrop_zone(zone))
        .count()
}

/// The innermost differing nodes between two trees, as readable paths.
fn diff_nodes(path: &str, before: &SexpNode, after: &SexpNode, out: &mut Vec<String>) {
    if before == after {
        return;
    }
    match (before, after) {
        (SexpNode::List(a), SexpNode::List(b)) if a.len() == b.len() && a.first() == b.first() => {
            let here = format!("{path}/{}", before.head().unwrap_or("?"));
            for (x, y) in a.iter().zip(b) {
                diff_nodes(&here, x, y, out);
            }
        }
        _ => out.push(format!("{path}: {before:?}\n    became {after:?}")),
    }
}

/// Refill, then read teardrop zones until two reads agree on `expected`
/// zones. `RefillZones` answers before KiCad's queued fill runs, so the
/// first quiet read can precede it.
fn refill_and_settle(
    client: &KiCadIpcClient,
    document: &kiapi::common::types::DocumentSpecifier,
    expected: usize,
) -> (Vec<IpcTeardropZone>, f64) {
    let started = Instant::now();
    client.refill_zones().expect("RefillZones failed");
    let deadline = started + Duration::from_secs(60);
    let mut previous: Option<Vec<IpcTeardropZone>> = None;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        client
            .wait_for_board_ready(Duration::from_secs(30))
            .expect("the board did not become ready after the refill");
        let zones = client
            .get_teardrop_zones_in(document.clone())
            .expect("teardrop zone read failed");
        if previous.as_ref() == Some(&zones) && zones.len() == expected {
            return (zones, started.elapsed().as_secs_f64());
        }
        assert!(
            Instant::now() < deadline,
            "teardrop zones did not settle at {expected} within 60 s; last read {zones:?}"
        );
        previous = Some(zones);
    }
}

#[test]
#[ignore = "requires a running KiCad GUI with its IPC API enabled"]
fn native_teardrop_settings_reach_the_board_and_its_copper() {
    let board =
        test_env("LIVE_KICAD_BOARD").expect("COPPERPOUR_LIVE_KICAD_BOARD must name the open board");
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let client = KiCadIpcClient::new(socket);
    let document = client
        .find_open_board(Path::new(&board))
        .expect("KiCad has not got the requested board open");
    let version = client.get_kicad_version().expect("GetVersion failed");
    eprintln!("KiCad {}", version.full_version);
    let mut evidence = json!({ "kicad_version": version.full_version, "board": board });

    if !supports_teardrop_settings(&version) {
        // The editor serves no settings: a read must refuse, not report
        // every pad as disabled.
        let error = client
            .get_teardrop_targets_in(document.clone())
            .expect_err("a pre-10.0.7 KiCad sent teardrop settings");
        assert!(
            matches!(
                error.downcast_ref::<TeardropReadError>(),
                Some(TeardropReadError::SettingsMissing { .. })
            ),
            "unexpected read failure: {error:#}"
        );
        // And an update carrying them is accepted, then dropped: what the
        // tool's version gate exists to prevent.
        let response = client
            .get_teardrop_items_response_in(document.clone())
            .expect("GetItems failed");
        let response: kiapi::common::commands::GetItemsResponse =
            prost::Message::decode(response.value.as_slice()).unwrap();
        let via = response
            .items
            .iter()
            .find(|item| item.type_url.ends_with("kiapi.board.types.Via"))
            .expect("the fixture has a via");
        let via: kiapi::board::types::Via = prost::Message::decode(via.value.as_slice()).unwrap();
        let uuid = via.id.unwrap().value;
        let before = client.save_document_to_string_in(document.clone()).unwrap();
        let outcome = client
            .set_teardrop_settings_in(
                document.clone(),
                &[IpcTeardropEdit {
                    kind: IpcTeardropTargetKind::Via,
                    uuid: uuid.clone(),
                    footprint_uuid: None,
                    settings: requested(),
                }],
            )
            .expect("the update was refused outright");
        let after = client.save_document_to_string_in(document.clone()).unwrap();
        assert!(
            matches!(outcome, IpcTeardropApplyOutcome::Uncertain { .. }),
            "a pre-10.0.7 KiCad confirmed teardrop settings: {outcome:?}"
        );
        assert_eq!(
            file_settings(&before, &uuid),
            file_settings(&after, &uuid),
            "a pre-10.0.7 KiCad applied teardrop settings"
        );
        evidence["pre_10_0_7"] = json!({
            "read": format!("{error:#}"),
            "update_outcome": format!("{outcome:?}"),
            "via_file_settings_unchanged": true,
        });
        write_evidence(&evidence);
        return;
    }

    let before_text = client.save_document_to_string_in(document.clone()).unwrap();
    assert!(
        !legacy_teardrops_in_board_text(&before_text).unwrap(),
        "the fixture is in legacy teardrop mode"
    );
    assert!(!client
        .board_uses_legacy_teardrops_in(document.clone())
        .unwrap());

    let targets = client
        .get_teardrop_targets_in(document.clone())
        .expect("teardrop read failed");
    assert!(!targets.is_empty());
    for target in &targets {
        assert_eq!(
            target.settings,
            file_settings(&before_text, &target.uuid),
            "{} reads over IPC other than KiCad's own serialization says",
            target.uuid
        );
    }
    let zones_before = client.get_teardrop_zones_in(document.clone()).unwrap();
    evidence["targets_before"] = json!(targets);
    evidence["teardrop_zones_before"] = json!(zones_before);

    // The GND pads and every via: the fixture's one routed net.
    let chosen: Vec<&IpcTeardropTarget> = targets
        .iter()
        .filter(|target| target.net == "GND")
        .collect();
    assert!(chosen.iter().any(|t| t.kind == IpcTeardropTargetKind::Pad));
    assert!(chosen.iter().any(|t| t.kind == IpcTeardropTargetKind::Via));
    let edits: Vec<IpcTeardropEdit> = chosen
        .iter()
        .map(|target| IpcTeardropEdit {
            kind: target.kind,
            uuid: target.uuid.clone(),
            footprint_uuid: target.footprint_uuid.clone(),
            settings: requested(),
        })
        .collect();

    let outcome = client
        .set_teardrop_settings_in(document.clone(), &edits)
        .expect("the settings update failed");
    let IpcTeardropApplyOutcome::Applied(applied) = outcome else {
        let text = client.save_document_to_string_in(document.clone()).unwrap();
        for edit in &edits {
            eprintln!(
                "{:?} {} in KiCad's text: {:?}",
                edit.kind,
                edit.uuid,
                file_settings(&text, &edit.uuid)
            );
        }
        panic!("the settings update was not confirmed: {outcome:?}");
    };
    let readback = applied.readback.clone();
    evidence["carried_pads"] = json!(applied.carried_pads);
    assert_eq!(readback.len(), edits.len());
    // Recorded, not asserted: whether `GetItemsById` still answers with each
    // edited item as it was before the commit (seen on 10.0.7 and 10.0.8 for pads updated one by one).
    let by_id = client
        .get_items_by_id_in(
            document.clone(),
            &edits.iter().map(|e| e.uuid.clone()).collect::<Vec<_>>(),
        )
        .expect("GetItemsById failed");
    let by_id = copperpour_ipc::client::teardrop_targets_from_items(&by_id, &Default::default())
        .expect("GetItemsById answer unreadable");
    let stale: Vec<String> = by_id
        .iter()
        .filter(|target| target.settings != requested())
        .map(|target| format!("{:?} {}", target.kind, target.uuid))
        .collect();
    eprintln!("GetItemsById stale after the commit: {stale:?}");
    evidence["get_items_by_id_stale_after_commit"] = json!(stale);

    // KiCad's serialization: only teardrop blocks changed, the chosen items
    // hold the request, every other item what it held.
    let after_text = client.save_document_to_string_in(document.clone()).unwrap();
    let before_tree = parse_sexp(&before_text).unwrap();
    let after_tree = parse_sexp(&after_text).unwrap();
    let mut differences = Vec::new();
    diff_nodes(
        "kicad_pcb",
        &without_teardrops(&before_tree),
        &without_teardrops(&after_tree),
        &mut differences,
    );
    assert!(
        differences.is_empty(),
        "the settings update changed something besides teardrop settings:\n{}",
        differences.join("\n")
    );
    for (uuid, _) in file_items(&before_tree) {
        let expected = if edits.iter().any(|edit| edit.uuid == uuid) {
            requested()
        } else {
            file_settings(&before_text, &uuid)
        };
        assert_eq!(file_settings(&after_text, &uuid), expected, "{uuid}");
    }
    // KiCad's commit rebuilds the teardrops of every pad of a touched
    // footprint and of a touched via (`BOARD_COMMIT::Push`), in the same undo
    // step: the settings reach copper without a refill.
    let zones_committed = client.get_teardrop_zones_in(document.clone()).unwrap();
    assert!(
        !zones_committed.is_empty(),
        "the commit built no teardrops; zones before were {zones_before:?}"
    );
    assert!(
        zones_committed.iter().all(|zone| zone.net == "GND"),
        "teardrops grew on a net that was not enabled: {zones_committed:?}"
    );
    assert_eq!(teardrop_zone_count(&after_text), zones_committed.len());
    evidence["edited"] = json!(readback);
    evidence["only_teardrop_settings_and_teardrop_zones_changed"] = json!(true);
    evidence["teardrop_zones_after_enable_commit"] = json!(zones_committed);
    eprintln!("{} teardrop zones after the commit", zones_committed.len());

    // Only a full rebuild restores a teardrop zone deleted on its own: the
    // deletion touches no pad, via or track, so no commit rebuilds it. That
    // makes the refill's teardrop rebuild observable.
    let removed = zones_committed[0].uuid.clone();
    client
        .run_commit("Delete one teardrop zone", |c| {
            c.delete_items_in(document.clone(), vec![removed.clone()])
        })
        .expect("deleting one teardrop zone failed");
    let zones_after_delete = client.get_teardrop_zones_in(document.clone()).unwrap();
    assert_eq!(zones_after_delete.len(), zones_committed.len() - 1);
    let (zones_enabled, enable_seconds) =
        refill_and_settle(&client, &document, zones_committed.len());
    let ids = |zones: &[IpcTeardropZone]| -> Vec<String> {
        zones.iter().map(|zone| zone.uuid.clone()).collect()
    };
    assert_eq!(
        ids(&zones_enabled),
        ids(&zones_committed),
        "the refill rebuilt other teardrops than the commit built"
    );
    let same = |field: fn(&IpcTeardropZone) -> &String| -> Vec<bool> {
        zones_enabled
            .iter()
            .zip(&zones_committed)
            .map(|(a, b)| field(a) == field(b))
            .collect()
    };
    evidence["refill_outline_same_as_commit"] = json!(same(|z| &z.outline_digest));
    evidence["refill_fill_same_as_commit"] = json!(same(|z| &z.fill_digest));
    eprintln!(
        "refill vs commit: outlines same {:?}, fills same {:?}",
        same(|z| &z.outline_digest),
        same(|z| &z.fill_digest)
    );
    evidence["deleted_teardrop_zone_restored_by_refill"] = json!(removed);
    let refilled_text = client.save_document_to_string_in(document.clone()).unwrap();
    let mut refill_differences = Vec::new();
    diff_nodes(
        "kicad_pcb",
        &without_teardrops(&after_tree),
        &without_teardrops(&parse_sexp(&refilled_text).unwrap()),
        &mut refill_differences,
    );
    evidence["teardrop_zones_after_enable_refill"] = json!(zones_enabled);
    evidence["enable_refill_seconds"] = json!(enable_seconds);
    evidence["enable_refill_changed_other_items"] = json!(refill_differences.len());
    eprintln!(
        "{} teardrop zones after the refill; {} other differences",
        zones_enabled.len(),
        refill_differences.len()
    );

    if test_env("CAPTURE_IPC_FIXTURE").is_some() {
        if board.ends_with(CAPTURED_BOARD) {
            client.save_board().expect("save failed");
            client
                .wait_for_board_ready(Duration::from_secs(30))
                .unwrap();
            let answer = client
                .get_teardrop_items_response_in(document.clone())
                .expect("GetItems failed");
            let out = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/teardrops_kicad10.ipc.bin");
            std::fs::write(&out, &answer.value).expect("failed to write capture");
            eprintln!(
                "wrote {} bytes of {} to {}",
                answer.value.len(),
                answer.type_url,
                out.display()
            );
            evidence["capture_bytes"] = json!(answer.value.len());
        } else {
            eprintln!("no capture: the capture is of {CAPTURED_BOARD}, not {board}");
        }
    }

    // Disable again: the refill removes what it built.
    let disable: Vec<IpcTeardropEdit> = edits
        .iter()
        .map(|edit| IpcTeardropEdit {
            settings: IpcTeardropSettings {
                enabled: false,
                ..edit.settings.clone()
            },
            ..edit.clone()
        })
        .collect();
    let outcome = client
        .set_teardrop_settings_in(document.clone(), &disable)
        .expect("the disable update failed");
    assert!(
        matches!(outcome, IpcTeardropApplyOutcome::Applied(_)),
        "{outcome:?}"
    );
    let zones_disabled_commit = client.get_teardrop_zones_in(document.clone()).unwrap();
    evidence["teardrop_zones_after_disable_commit"] = json!(zones_disabled_commit);
    assert!(
        zones_disabled_commit.is_empty(),
        "the disable commit left teardrops: {zones_disabled_commit:?}"
    );
    let (zones_disabled, disable_seconds) = refill_and_settle(&client, &document, 0);
    assert!(zones_disabled.is_empty(), "{zones_disabled:?}");
    evidence["teardrop_zones_after_disable_refill"] = json!(zones_disabled);
    evidence["disable_refill_seconds"] = json!(disable_seconds);
    write_evidence(&evidence);
}

fn write_evidence(evidence: &serde_json::Value) {
    if let Some(path) = test_env("TEARDROP_EVIDENCE") {
        std::fs::write(&path, serde_json::to_string_pretty(evidence).unwrap())
            .expect("failed to write evidence");
        eprintln!("evidence written to {path}");
    }
}
