//! `repeat_layout` on a board whose sheet is placed three times.
//!
//! `repeat_layout_kicad10.kicad_pcb` holds `channel.kicad_sch` placed as
//! Channel 1 to 3, four footprints each, the fourth on a sheet below it; the
//! README beside it says how pcbnew 10.0.6 made it. Channel 1 (R1-R4) is laid
//! out, R5 and R9 are placed where Channels 2 and 3 belong, at 90° and -30°,
//! and the rest of those channels sit where a sync left them.
//!
//! The expected poses are pcbnew 10.0.6's own: the same board's target
//! footprints moved by the calls `MULTICHANNEL_TOOL::copyRuleAreaContents`
//! makes, anchored on R1 (the README's `oracle`). They are restated here as
//! literals. Every case runs through served `tools/call`.

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
use crate::tools::ServerConfig;
use copperpour_ipc::builders::{any_is, any_type_name, mm_to_nm, pack_any};
use copperpour_ipc::gen::kiapi;
use copperpour_sexp::{parse_sexp, SexpNode};
use prost::Message;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const BOARD: &str = "repeat_layout_kicad10.kicad_pcb";
/// The same board with Channels 2 and 3 changed so that they no longer
/// match Channel 1 (README).
const MISMATCH: &str = "repeat_layout_mismatch_kicad10.kicad_pcb";

/// A footprint where pcbnew 10.0.6 puts it: the reference, x and y in nm,
/// the orientation, the side.
type Placed = (&'static str, i64, i64, f64, &'static str);

const CHANNEL_2: [Placed; 3] = [
    ("R6", 140_000_000, 98_000_000, 180.0, "F.Cu"),
    ("R7", 142_500_000, 100_000_000, -90.0, "B.Cu"),
    ("R8", 142_750_000, 96_750_000, 135.0, "F.Cu"),
];
const CHANNEL_3: [Placed; 3] = [
    ("R10", 162_232_051, 121_250_000, 60.0, "F.Cu"),
    ("R11", 159_250_000, 122_415_064, 150.0, "B.Cu"),
    ("R12", 161_939_583, 124_256_570, 15.0, "F.Cu"),
];

type Pose = (i64, i64, f64, String);

/// Each footprint's root pose and side in a board's text, by reference.
fn poses(content: &str) -> BTreeMap<String, Pose> {
    footprint_nodes(content)
        .into_iter()
        .map(|node| {
            let at = node.find("at").unwrap();
            (
                reference_of(&node),
                (
                    mm_to_nm(at.get_f64(1).unwrap()),
                    mm_to_nm(at.get_f64(2).unwrap()),
                    at.get_f64(3).unwrap_or(0.0),
                    node.find_str("layer").unwrap().to_string(),
                ),
            )
        })
        .collect()
}

fn footprint_nodes(content: &str) -> Vec<SexpNode> {
    parse_sexp(content)
        .unwrap()
        .find_all("footprint")
        .into_iter()
        .cloned()
        .collect()
}

fn reference_of(node: &SexpNode) -> String {
    node.find_all("property")
        .into_iter()
        .find(|property| property.get(1).and_then(SexpNode::as_str) == Some("Reference"))
        .and_then(|property| property.get(2))
        .and_then(SexpNode::as_str)
        .unwrap()
        .to_string()
}

fn expected(channels: &[&[Placed]]) -> BTreeMap<String, Pose> {
    channels
        .iter()
        .flat_map(|channel| channel.iter())
        .map(|(reference, x, y, rotation, side)| {
            (reference.to_string(), (*x, *y, *rotation, side.to_string()))
        })
        .collect()
}

/// A refusal's reasons, sorted: they come in board order, and KiCad orders
/// a saved board's footprints by its own key.
fn reasons(body: &Value) -> Vec<String> {
    let mut reasons: Vec<String> = body["error"]["reasons"]
        .as_array()
        .unwrap_or_else(|| panic!("no reasons: {body:#}"))
        .iter()
        .map(|reason| reason.as_str().unwrap().to_string())
        .collect();
    reasons.sort();
    reasons
}

/// The poses `after` holds that differ from `before`'s.
fn moved(before: &str, after: &str) -> BTreeMap<String, Pose> {
    let before = poses(before);
    poses(after)
        .into_iter()
        .filter(|(reference, pose)| before.get(reference) != Some(pose))
        .collect()
}

// ─── The closed board ─────────────────────────────────────────────────────────

struct Board {
    _dir: tempfile::TempDir,
    path: PathBuf,
    original: String,
}

impl Board {
    fn new(fixture: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(fixture);
        std::fs::copy(Path::new(FIXTURE_DIR).join(fixture), &path).unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        Self {
            _dir: dir,
            path,
            original,
        }
    }

    fn now(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap()
    }

    fn assert_untouched(&self) {
        assert!(
            self.now() == self.original,
            "the board must not be modified"
        );
    }
}

async fn handler_talking_to(address: &str) -> McpHandler {
    McpHandler::new(ServerConfig {
        ipc_address: address.to_string(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn call_with(handler: &McpHandler, board: &Path, arguments: Value) -> (bool, Value) {
    let mut arguments = arguments;
    arguments["board"] = json!(board.to_string_lossy());
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "repeat_layout", "arguments": arguments }
        }))
        .await
        .unwrap();
    let result: CallToolResult = serde_json::from_value(response.result.unwrap()).unwrap();
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text content");
    };
    (
        result.is_error,
        serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
    )
}

/// No KiCad is reachable, so the saved board is authoritative.
async fn call(board: &Board, arguments: Value) -> (bool, Value) {
    call_with(&handler_talking_to("").await, &board.path, arguments).await
}

#[tokio::test]
async fn every_other_placement_takes_the_pose_kicad_gives_it() {
    let board = Board::new(BOARD);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["source"], "file", "{body:#}");
    assert_eq!(body["count"], 6, "{body:#}");
    assert_eq!(body["copper_check"]["status"], "not_run", "{body:#}");
    // R5 and R9, the targets' anchors, are already where they belong.
    assert_eq!(
        moved(&board.original, &board.now()),
        expected(&[&CHANNEL_2, &CHANNEL_3])
    );
    let reported: BTreeMap<String, (i64, i64, f64)> = body["placements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|placement| {
            (
                placement["reference"].as_str().unwrap().to_string(),
                (
                    mm_to_nm(placement["x"].as_f64().unwrap()),
                    mm_to_nm(placement["y"].as_f64().unwrap()),
                    placement["rotation"].as_f64().unwrap(),
                ),
            )
        })
        .collect();
    let written: BTreeMap<String, (i64, i64, f64)> = moved(&board.original, &board.now())
        .into_iter()
        .map(|(reference, (x, y, rotation, _))| (reference, (x, y, rotation)))
        .collect();
    assert_eq!(reported, written);
}

#[tokio::test]
async fn targets_lay_out_only_the_placements_they_name() {
    let board = Board::new(BOARD);
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R1", "target_references": ["R9"] }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["count"], 3, "{body:#}");
    assert_eq!(
        moved(&board.original, &board.now()),
        expected(&[&CHANNEL_3])
    );
}

#[tokio::test]
async fn any_placement_can_be_the_reference() {
    // Anchored on R5, Channel 2's synced poses are the layout, turned by
    // -90° and moved onto R1.
    let board = Board::new(BOARD);
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R5", "target_references": ["R1"] }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["count"], 3, "{body:#}");
    assert_eq!(
        moved(&board.original, &board.now()),
        expected(&[&[
            ("R2", 100_000_000, 95_000_000, -90.0, "F.Cu"),
            ("R3", 100_000_000, 97_000_000, -90.0, "B.Cu"),
            ("R4", 100_000_000, 99_000_000, -90.0, "F.Cu"),
        ]])
    );
}

#[tokio::test]
async fn turns_of_any_angle_round_as_kicad_rounds_them() {
    // Anchored on R9 at -30°, Channels 1 and 2 turn by 30° and 120°.
    let board = Board::new(BOARD);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R9" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(
        moved(&board.original, &board.now()),
        expected(&[
            &[
                ("R2", 110_111_861, 111_193_747, 30.0, "F.Cu"),
                ("R3", 111_843_911, 110_193_747, 30.0, "B.Cu"),
                ("R4", 113_575_962, 109_193_747, 30.0, "F.Cu"),
            ],
            &[
                ("R6", 151_193_747, 99_888_139, 120.0, "F.Cu"),
                ("R7", 150_193_747, 98_156_089, 120.0, "B.Cu"),
                ("R8", 149_193_747, 96_424_038, 120.0, "F.Cu"),
            ],
        ])
    );
    // Anchored on R5 at 90°, Channel 3 turns by -120°.
    let board = Board::new(BOARD);
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R5", "target_references": ["R9"] }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(
        moved(&board.original, &board.now()),
        expected(&[&[
            ("R10", 154_339_746, 110_919_873, -120.0, "F.Cu"),
            ("R11", 153_339_746, 112_651_923, -120.0, "B.Cu"),
            ("R12", 152_339_746, 114_383_974, -120.0, "F.Cu"),
        ]])
    );
}

#[tokio::test]
async fn a_dry_run_reports_each_pose_and_writes_nothing() {
    let board = Board::new(BOARD);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1", "dry_run": true })).await;
    assert!(!is_error, "{body:#}");
    board.assert_untouched();
    assert_eq!(body["dry_run"], true, "{body:#}");
    assert_eq!(body["source"], "file", "{body:#}");
    assert_eq!(body["count"], 6, "{body:#}");
    assert_eq!(body["copper_check"]["status"], "not_run", "{body:#}");
    let r6 = body["placements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|placement| placement["reference"] == "R6")
        .unwrap();
    assert_eq!(
        r6["current"],
        json!({ "x": 135.0, "y": 110.0, "rotation": 0.0 })
    );
    assert_eq!(
        r6["target"],
        json!({ "x": 140.0, "y": 98.0, "rotation": 180.0 })
    );
}

#[tokio::test]
async fn placements_that_do_not_match_are_refused_naming_each_footprint() {
    let board = Board::new(MISMATCH);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(is_error, "{body:#}");
    board.assert_untouched();
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    assert_eq!(body["error"]["operation"], "repeat_layout", "{body:#}");
    assert_eq!(
        reasons(&body),
        [
            "R10 has 1 pad where R2 has 2",
            "R11 and R16 are footprints of one symbol",
            "R12 is Resistor_SMD:R_0603 where R4 is Resistor_SMD:R_0402",
            "R15 has no counterpart on R1's placement",
            "R2 appears more than once on this board",
            "R4 has no counterpart on R5's placement",
            "R7 is on F.Cu where R3 is on B.Cu; flip it first",
        ],
        "{body:#}"
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .ends_with("and 4 more. Nothing was moved."),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_placement_holding_the_anchors_symbol_twice_is_laid_out_once() {
    // R11 and R16 are both R3's counterparts on Channel 3: one placement,
    // reported once.
    let board = Board::new(MISMATCH);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R3" })).await;
    assert!(is_error, "{body:#}");
    board.assert_untouched();
    assert_eq!(
        reasons(&body),
        [
            "R10 has 1 pad where R2 has 2",
            "R11 and R16 are footprints of one symbol",
            "R12 is Resistor_SMD:R_0603 where R4 is Resistor_SMD:R_0402",
            "R15 has no counterpart on R3's placement",
            "R2 appears more than once on this board",
            "R4 has no counterpart on R7's placement",
            "R7 is on F.Cu where R3 is on B.Cu; flip it first",
        ],
        "{body:#}"
    );
}

#[tokio::test]
async fn an_anchor_or_target_with_nothing_to_repeat_is_refused_by_name() {
    let cases = [
        (
            json!({ "anchor_reference": "R13" }),
            "anchor_reference",
            "R13's symbol is on the root sheet, which is placed once",
        ),
        (
            json!({ "anchor_reference": "R4" }),
            "anchor_reference",
            "R4 is the only footprint on its sheet and the sheets below it, so there is \
             nothing to place",
        ),
        (
            json!({ "anchor_reference": "R14" }),
            "anchor_reference",
            "R14 has no symbol, so it is on no sheet",
        ),
        (
            json!({ "anchor_reference": "R99" }),
            "anchor_reference",
            "no footprint 'R99' on this board",
        ),
        (
            json!({ "anchor_reference": "R1", "target_references": ["R2"] }),
            "target_references",
            "R2 is not R1's symbol on another placement of its sheet",
        ),
        (
            json!({ "anchor_reference": "R1", "target_references": ["R99"] }),
            "target_references",
            "no footprint 'R99' on this board",
        ),
        (
            json!({ "anchor_reference": "R1", "target_references": ["R5", "R5"] }),
            "target_references",
            "names R5 more than once",
        ),
        // The schema's minItems refuses it before the handler runs.
        (
            json!({ "anchor_reference": "R1", "target_references": [] }),
            "target_references",
            "[] has less than 1 item",
        ),
    ];
    for (arguments, field, reason) in cases {
        let board = Board::new(BOARD);
        let (is_error, body) = call(&board, arguments.clone()).await;
        assert!(is_error, "{arguments}: {body:#}");
        board.assert_untouched();
        assert_eq!(
            body["error"]["kind"], "invalid_argument",
            "{arguments}: {body:#}"
        );
        assert_eq!(body["error"]["field"], field, "{arguments}: {body:#}");
        assert_eq!(body["error"]["reason"], reason, "{arguments}: {body:#}");
    }
}

/// Angles KiCad never writes, so no fixture holds them: the board with R5's
/// `(at 140 100 90)` given another angle. One that is not a number is refused
/// as unreadable, the board as it was; one too large to reduce by subtracting
/// 360 still turns.
#[tokio::test]
async fn an_angle_kicad_never_writes_is_refused_or_turned() {
    for (angle, refused) in [("inf", true), ("nan", true), ("1e20", false)] {
        let board = Board::new(BOARD);
        let content = board.original.replacen(
            "\t\t(at 140 100 90)",
            &format!("\t\t(at 140 100 {angle})"),
            1,
        );
        std::fs::write(&board.path, &content).unwrap();
        let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
        if refused {
            assert!(is_error, "{angle}: {body:#}");
            assert!(
                body.to_string()
                    .contains("R5 has no readable (at x y) placement"),
                "{angle}: {body:#}"
            );
            assert!(
                board.now() == content,
                "{angle}: the board must not be modified"
            );
        } else {
            assert!(!is_error, "{angle}: {body:#}");
        }
    }
}

// ─── The live board ───────────────────────────────────────────────────────────

/// The fixture's footprints as `FOOTPRINT::Serialize` (`pcbnew/footprint.cpp`,
/// KiCad 10.0.6) sends the fields the plan reads: KIID, position, orientation,
/// layer, library identifier, reference and symbol path. They carry no pads,
/// so the copper check, tested on KiCad's own captures in
/// `pcb_copper_check.rs`, has nothing to test and asks KiCad nothing more.
fn as_kicad_sends(content: &str) -> Vec<prost_types::Any> {
    use kiapi::board::types as board;
    use kiapi::common::types as common;
    footprint_nodes(content)
        .into_iter()
        .map(|node| {
            let at = node.find("at").unwrap();
            let (library, entry) = node
                .get(1)
                .and_then(SexpNode::as_str)
                .unwrap()
                .split_once(':')
                .unwrap();
            let reference = reference_of(&node);
            let field = board::Field {
                name: "Reference".to_string(),
                text: Some(board::BoardText {
                    text: Some(common::Text {
                        text: reference,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let footprint = board::FootprintInstance {
                id: Some(common::Kiid {
                    value: node.find_str("uuid").unwrap().to_string(),
                }),
                position: Some(common::Vector2 {
                    x_nm: mm_to_nm(at.get_f64(1).unwrap()),
                    y_nm: mm_to_nm(at.get_f64(2).unwrap()),
                }),
                orientation: Some(common::Angle {
                    value_degrees: at.get_f64(3).unwrap_or(0.0),
                }),
                layer: match node.find_str("layer") {
                    Some("B.Cu") => board::BoardLayer::BlBCu,
                    _ => board::BoardLayer::BlFCu,
                } as i32,
                definition: Some(board::Footprint {
                    id: Some(common::LibraryIdentifier {
                        library_nickname: library.to_string(),
                        entry_name: entry.to_string(),
                    }),
                    reference_field: Some(field.clone()),
                    ..Default::default()
                }),
                reference_field: Some(field),
                symbol_path: Some(common::SheetPath {
                    path: node
                        .find_str("path")
                        .unwrap_or_default()
                        .split('/')
                        .filter(|id| !id.is_empty())
                        .map(|id| common::Kiid {
                            value: id.to_string(),
                        })
                        .collect(),
                    path_human_readable: String::new(),
                }),
                ..Default::default()
            };
            pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        })
        .collect()
}

struct Kicad {
    _dir: tempfile::TempDir,
    board: PathBuf,
    store: Arc<Mutex<ItemStore>>,
    sent: Arc<Mutex<Vec<String>>>,
    handler: McpHandler,
    _server: MockIpcServer,
}

impl Kicad {
    async fn holding(fixture: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join(fixture);
        std::fs::copy(Path::new(FIXTURE_DIR).join(fixture), &board).unwrap();
        let content = std::fs::read_to_string(&board).unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: as_kicad_sends(&content),
            ..Default::default()
        }));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let server = spawn_kicad_holding_items(&board, vec![], store.clone(), |_| {}, {
            move |command, _| {
                let name = any_type_name(command).rsplit('.').next().unwrap();
                log.lock().unwrap().push(name.to_string());
                None
            }
        });
        let handler = handler_talking_to(server.address()).await;
        Self {
            _dir: dir,
            board,
            store,
            sent,
            handler,
            _server: server,
        }
    }

    /// Each footprint's pose and side as KiCad now holds it.
    fn held(&self) -> BTreeMap<String, Pose> {
        self.store
            .lock()
            .unwrap()
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.FootprintInstance"))
            .map(|item| {
                let fp =
                    kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
                let position = fp.position.unwrap();
                let reference = fp.reference_field.unwrap().text.unwrap().text.unwrap().text;
                let side = if fp.layer == kiapi::board::types::BoardLayer::BlBCu as i32 {
                    "B.Cu"
                } else {
                    "F.Cu"
                };
                (
                    reference,
                    (
                        position.x_nm,
                        position.y_nm,
                        fp.orientation.unwrap().value_degrees,
                        side.to_string(),
                    ),
                )
            })
            .collect()
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_live_board_takes_the_layout_in_one_commit() {
    let kicad = Kicad::holding(BOARD).await;
    let before = kicad.held();
    let original = std::fs::read_to_string(&kicad.board).unwrap();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["source"], "ipc", "{body:#}");
    assert_eq!(body["count"], 6, "{body:#}");
    assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
    let moved: BTreeMap<String, Pose> = kicad
        .held()
        .into_iter()
        .filter(|(reference, pose)| before.get(reference) != Some(pose))
        .collect();
    assert_eq!(moved, expected(&[&CHANNEL_2, &CHANNEL_3]));
    assert_eq!(kicad.store.lock().unwrap().updates, 1);
    let sent = kicad.sent();
    assert_eq!(
        sent.iter().filter(|name| *name == "BeginCommit").count(),
        1,
        "{sent:?}"
    );
    assert!(
        std::fs::read_to_string(&kicad.board).unwrap() == original,
        "a live layout must not write the file"
    );
}

#[tokio::test]
async fn a_live_dry_run_only_reads() {
    let kicad = Kicad::holding(BOARD).await;
    let before = kicad.held();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1", "dry_run": true }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["dry_run"], true, "{body:#}");
    assert_eq!(body["source"], "ipc", "{body:#}");
    assert_eq!(body["count"], 6, "{body:#}");
    assert_eq!(kicad.held(), before);
    for name in kicad.sent() {
        assert!(
            matches!(
                name.as_str(),
                "GetOpenDocuments" | "GetItems" | "GetVersion"
            ),
            "{name} reached KiCad"
        );
    }
}

#[tokio::test]
async fn a_live_mismatch_is_refused_before_anything_is_sent() {
    let kicad = Kicad::holding(MISMATCH).await;
    let before = kicad.held();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    // The double's footprints carry no pads, so R10's missing pad is not
    // among the live reasons.
    assert_eq!(
        reasons(&body),
        [
            "R11 and R16 are footprints of one symbol",
            "R12 is Resistor_SMD:R_0603 where R4 is Resistor_SMD:R_0402",
            "R15 has no counterpart on R1's placement",
            "R2 appears more than once on this board",
            "R4 has no counterpart on R5's placement",
            "R7 is on F.Cu where R3 is on B.Cu; flip it first",
        ],
        "{body:#}"
    );
    assert_eq!(kicad.held(), before);
    assert_eq!(kicad.store.lock().unwrap().updates, 0);
    assert!(!kicad.sent().iter().any(|name| name == "BeginCommit"));
}

#[tokio::test]
async fn a_target_footprint_that_cannot_turn_is_named_and_nothing_moves() {
    use kiapi::common::types as common;
    // R10 turns by 60°. A rounded rectangle on F.Fab cannot follow a turn off
    // the right angles, which `transform_footprint_children` refuses for
    // the footprint by name before anything is sent.
    let kicad = Kicad::holding(BOARD).await;
    {
        let mut store = kicad.store.lock().unwrap();
        let item = store
            .items
            .iter_mut()
            .find(|item| {
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                    .ok()
                    .and_then(|fp| fp.reference_field?.text?.text)
                    .is_some_and(|text| text.text == "R10")
            })
            .unwrap();
        let mut fp = kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let mut rect = copperpour_ipc::builders::board_rectangle(
            "F.Fab", 0.05, 154.0, 129.0, 156.0, 131.0, false,
        );
        if let Some(common::graphic_shape::Geometry::Rectangle(r)) = rect
            .shape
            .as_mut()
            .and_then(|shape| shape.geometry.as_mut())
        {
            r.corner_radius = Some(common::Distance { value_nm: 250_000 });
        }
        fp.definition
            .as_mut()
            .unwrap()
            .items
            .push(pack_any(&rect, "kiapi.board.types.BoardGraphicShape"));
        *item = pack_any(&fp, "kiapi.board.types.FootprintInstance");
    }
    let before = kicad.held();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    assert_eq!(body["error"]["operation"], "repeat_layout", "{body:#}");
    let reasons = reasons(&body);
    assert!(
        reasons.len() == 1 && reasons[0].contains("'R10'") && reasons[0].contains("F.Fab"),
        "{body:#}"
    );
    assert_eq!(kicad.held(), before);
    assert_eq!(kicad.store.lock().unwrap().updates, 0);
}
