//! #701: a tool that sends a whole placed footprint back to a live KiCad
//! refuses, with nothing written, when the footprint owns a group, which
//! KiCad 10.0.8 drops on every footprint update over its API.

use super::{owned_group_reasons_in, OwnedGroupReason};
use crate::mcp::handler::McpHandler;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::spawn_kicad_holding_board;
use copperpour_ipc::builders::{any_type_name, pack_any};
use copperpour_ipc::gen::kiapi;
use prost::Message;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A board KiCad 10.0.8 wrote: U1 owns a group, R1 is only a member of a
/// board-level group, which is not ownership.
const BOARD: &str = include_str!("../../tests/fixtures/footprint_children_kicad10.kicad_pcb");

/// U1's own group in [`BOARD`].
const U1_GROUP: &str = "\t\t(group \"\"\n\t\t\t(uuid \"6b3f8f1e-e891-48a2-9dca-37c4a0d6ab82\")\n\t\t\t(members \"0b59daea-b25c-466e-a3c2-be0447f3dad9\" \"965d8d26-073b-410e-a23d-59d91417ce56\")\n\t\t)\n";

fn reasons(found: &[OwnedGroupReason]) -> Vec<(Option<&str>, &str)> {
    found
        .iter()
        .map(|found| (found.reference.as_deref(), found.reason.as_str()))
        .collect()
}

#[test]
fn only_a_footprint_that_owns_a_group_is_named() {
    assert_eq!(BOARD.matches(U1_GROUP).count(), 1);
    let found = owned_group_reasons_in(BOARD, "move", Some(&["R1", "U1", "X9", "U1"]));
    assert_eq!(
        reasons(&found),
        [(
            Some("U1"),
            "U1 owns 1 group, which KiCad drops when its API updates a footprint; move U1 in \
             KiCad's PCB editor instead"
        )]
    );
    assert!(owned_group_reasons_in(BOARD, "move", Some(&["R1", "X9"])).is_empty());
    assert!(owned_group_reasons_in(BOARD, "move", Some(&[])).is_empty());
}

#[test]
fn every_footprint_is_checked_when_none_is_named() {
    let found = owned_group_reasons_in(BOARD, "edit the fields of", None);
    assert_eq!(
        reasons(&found),
        [(
            Some("U1"),
            "U1 owns 1 group, which KiCad drops when its API updates a footprint; edit the \
             fields of U1 in KiCad's PCB editor instead"
        )]
    );
    let ungrouped = BOARD.replacen(U1_GROUP, "", 1);
    assert!(owned_group_reasons_in(&ungrouped, "move", None).is_empty());
}

#[test]
fn groups_are_counted() {
    let twice = BOARD.replacen(U1_GROUP, &format!("{U1_GROUP}{U1_GROUP}"), 1);
    let found = owned_group_reasons_in(&twice, "rotate", Some(&["U1"]));
    assert!(
        found[0].reason.starts_with("U1 owns 2 groups,"),
        "{found:?}"
    );
}

/// Two footprints with one reference, as before annotation: the first owns a
/// group, the second none. Their counts add up, so the reference is refused;
/// keeping only the last footprint's count would let the edit through, and the
/// tool might edit the footprint that owns the group (#826's review).
#[test]
fn a_reference_two_footprints_carry_owns_what_both_own() {
    let r1 = "(property \"Reference\" \"R1\"";
    let u1 = "(property \"Reference\" \"U1\"";
    assert!(BOARD.find(r1) < BOARD.find(u1), "R1 must come first");
    // U1 without its own group, renamed to R1, after the original R1.
    let two_r1 = BOARD.replacen(U1_GROUP, "", 1).replacen(u1, r1, 1);
    assert_eq!(two_r1.matches(r1).count(), 2);
    assert!(owned_group_reasons_in(&two_r1, "move", Some(&["R1"])).is_empty());
    // The first R1 owns a group.
    let first_owns = two_r1.replacen(
        r1,
        &format!(
            "(group \"\" (uuid \"00000000-0000-0000-0000-000000000826\") (members))\n\t\t{r1}"
        ),
        1,
    );
    assert_eq!(
        reasons(&owned_group_reasons_in(&first_owns, "move", Some(&["R1"]))),
        [(
            Some("R1"),
            "R1 owns 1 group, which KiCad drops when its API updates a footprint; move R1 in \
             KiCad's PCB editor instead"
        )]
    );
}

#[test]
fn an_unreadable_board_refuses_rather_than_passes() {
    let found = owned_group_reasons_in("(kicad_pcb", "move", Some(&["R1", "U1"]));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].reference, None);
    assert!(
        found[0]
            .reason
            .starts_with("could not determine whether R1, U1 own groups"),
        "{found:?}"
    );
    // Two footprints with one UUID (R1's own, the first time it is written,
    // given U1's): which one owns what is unknown.
    let uuid = "c7901245-d0c4-476f-8215-85f1183d04de";
    let r1 = "20a44d6b-d6bb-4ec1-ac0c-f1d3d7e20db2";
    assert!(BOARD.find(r1) < BOARD.find("(property \"Reference\" \"R1\""));
    let doubled = BOARD.replacen(r1, uuid, 1);
    let found = owned_group_reasons_in(&doubled, "move", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].reference, None);
}

/// A KiCad holding [`BOARD`] open, answering `SaveDocumentToString` with
/// `board_text` and nothing else, so a tool that gets past the groups check
/// fails on its next read; every command it receives is logged.
struct Kicad {
    _dir: tempfile::TempDir,
    board: PathBuf,
    sent: Arc<Mutex<Vec<String>>>,
    handler: McpHandler,
    _server: MockIpcServer,
}

impl Kicad {
    async fn holding(board_text: String) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("groups.kicad_pcb");
        std::fs::write(&board, BOARD).unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let server = spawn_kicad_holding_board(&board, move |command| {
            let name = any_type_name(command)
                .rsplit('.')
                .next()
                .unwrap()
                .to_string();
            log.lock().unwrap().push(name);
            if !command.type_url.ends_with("SaveDocumentToString") {
                return None;
            }
            let request =
                kiapi::common::commands::SaveDocumentToString::decode(command.value.as_slice())
                    .unwrap();
            Some(pack_any(
                &kiapi::common::commands::SavedDocumentResponse {
                    document: request.document,
                    contents: board_text.clone(),
                },
                "kiapi.common.commands.SavedDocumentResponse",
            ))
        });
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
            sent,
            handler,
            _server: server,
        }
    }

    async fn call(&self, tool: &str, mut arguments: Value) -> (bool, Value) {
        arguments["board"] = json!(self.board.to_string_lossy());
        let response = self
            .handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 701,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
        )
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

/// Each tool, its arguments naming U1, and the PCB editor action its refusal
/// offers instead.
fn calls() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "move_component",
            json!({ "reference": "U1", "x": 120.0, "y": 80.0 }),
            "move",
        ),
        (
            "move_component",
            json!({ "reference": "U1", "x": 120.0, "y": 80.0, "dry_run": true }),
            "move",
        ),
        (
            "rotate_component",
            json!({ "reference": "U1", "rotation": 90.0 }),
            "rotate",
        ),
        (
            "set_component_placements",
            json!({ "placements": [
                { "reference": "R1", "x": 100.0, "y": 80.0, "rotation": 0.0 },
                { "reference": "U1", "x": 120.0, "y": 80.0, "rotation": 0.0 },
            ] }),
            "move",
        ),
        (
            "align_components",
            json!({ "references": ["R1", "U1"], "axis": "x", "value": 100.0 }),
            "move",
        ),
        (
            "edit_component",
            json!({ "reference": "U1", "dnp": true }),
            "edit",
        ),
        (
            "set_footprint_field",
            json!({ "reference": "U1", "name": "MPN", "value": "X-1" }),
            "edit the fields of",
        ),
        (
            "edit_footprint_field",
            json!({ "reference": "U1", "field": "value", "visible": false }),
            "edit the fields of",
        ),
        (
            "set_footprint_field_style",
            json!({ "references": ["R1", "U1"], "visible": false }),
            "edit the fields of",
        ),
        (
            "set_footprint_field_style",
            json!({ "size_x": 1.0, "size_y": 1.0 }),
            "edit the fields of",
        ),
        (
            "edit_board_footprint_graphic",
            json!({
                "reference": "U1",
                "uuid": "0b59daea-b25c-466e-a3c2-be0447f3dad9",
                "points": [{ "x": 0.0, "y": 0.0 }, { "x": 1.0, "y": 1.0 }],
            }),
            "edit the graphic of",
        ),
        (
            "set_pad_net",
            json!({ "reference": "U1", "pad_number": "1", "net_name": "GND" }),
            "change the pad nets of",
        ),
        (
            "set_placed_footprint_models",
            json!({
                "reference": "U1",
                "mode": "remove",
                "model_index": 0,
                "expected_models_revision": "any",
            }),
            "change the 3D models of",
        ),
    ]
}

#[tokio::test]
async fn every_live_footprint_edit_refuses_a_footprint_that_owns_a_group() {
    let kicad = Kicad::holding(BOARD.to_string()).await;
    for (tool, arguments, action) in calls() {
        kicad.sent.lock().unwrap().clear();
        let (is_error, body) = kicad.call(tool, arguments.clone()).await;
        assert!(is_error, "{tool} {arguments}: {body:#}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{tool}: {body:#}");
        assert_eq!(body["error"]["operation"], tool, "{tool}: {body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!([format!(
                "U1 owns 1 group, which KiCad drops when its API updates a footprint; \
                 {action} U1 in KiCad's PCB editor instead"
            )]),
            "{tool} {arguments}: {body:#}"
        );
        // The board was read once and nothing else reached KiCad.
        assert_eq!(kicad.sent(), ["SaveDocumentToString"], "{tool} {arguments}");
        assert_eq!(std::fs::read_to_string(&kicad.board).unwrap(), BOARD);
    }
}

#[tokio::test]
async fn a_footprint_without_groups_gets_past_the_check() {
    // The same calls on a board where U1 owns nothing: none is refused for
    // groups, and each goes on to read KiCad's items.
    let kicad = Kicad::holding(BOARD.replacen(U1_GROUP, "", 1)).await;
    for (tool, arguments, _) in calls() {
        kicad.sent.lock().unwrap().clear();
        let (_, body) = kicad.call(tool, arguments.clone()).await;
        assert!(
            !body.to_string().contains("owns 1 group"),
            "{tool} {arguments}: {body:#}"
        );
        let sent = kicad.sent();
        assert_eq!(
            sent.first().map(String::as_str),
            Some("SaveDocumentToString")
        );
        assert!(sent.len() > 1, "{tool} {arguments}: {sent:?}");
    }
}
