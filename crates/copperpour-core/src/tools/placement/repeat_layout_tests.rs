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
        let path = dir.path().join(Path::new(fixture).file_name().unwrap());
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

// ─── Routing ──────────────────────────────────────────────────────────────────
//
// `repeat_layout_routing_kicad10.kicad_pcb` is the board above with Channel 1
// routed, Channel 2 routed where a sync left it, and R9 turned to 45°; its
// README says how pcbnew 10.0.6 made it. The expected copies are pcbnew's
// own: each of Channel 1's items on its own nets duplicated, given the net of
// the matched pad and turned and moved by the calls `copyRuleAreaContents`
// makes (the README's `oracle`).

const ROUTED: &str = "repeat_layout_routing_kicad10.kicad_pcb";
/// The routed board with R8 pad 1 and R17, on the root sheet, on Channel 2's
/// OUT, and R10 pad 2 and R11 pad 1 on Channel 3's MID (README).
const ROUTED_MISMATCH: &str = "repeat_layout_routing_mismatch_kicad10.kicad_pcb";

/// Channel 2's MID stub, OUT track and IN stub, in its area, which the copies
/// replace.
const MID_STUB: &str = "08cf5053-ed54-4206-b23a-3d693ed67fa3";
const OUT_TRACK: &str = "6c43e0b0-05cb-4524-9a73-d44fe83ec0b8";
const IN_STUB: &str = "83f7bbfe-51d3-426c-9c8e-3125fda56377";

/// The routed board with routing on nets Channel 1 shares, a VCC track, R4
/// grouped with a rectangle on Dwgs.User, and GND vias (README).
const AREA: &str = "repeat_layout_area_kicad10.kicad_pcb";
/// KiCad's own box of every item of the area board, which holds every item of
/// the routed board under its uuid there (README).
const BOXES: &str = "repeat_layout_area_kicad10.boxes.tsv";
/// The area board's GND via inside Channel 2's area.
const CHANNEL_2_VIA: &str = "71cf95d9-e20c-4b21-b757-6f5468516b03";

/// Channel 3's footprints where pcbnew 10.0.6 puts them with R9 at 45°.
const CHANNEL_3_AT_45: [Placed; 3] = [
    ("R10", 161_914_214, 118_835_787, 135.0, "F.Cu"),
    ("R11", 162_267_767, 122_017_767, -135.0, "B.Cu"),
    ("R12", 164_742_641, 119_896_447, 90.0, "F.Cu"),
];

/// A track, arc or via: its tag, net, layer (a via's first), whether it is
/// locked, its points in nm (start, mid, end; a via's position), and its
/// width, a via's size.
type Routed = (String, String, String, bool, Vec<(i64, i64)>, i64);

type Copy = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    &'static [(i64, i64)],
);

/// pcbnew's copies of Channel 1's routing on MID and OUT, onto Channel 2
/// (R5, 90°) and Channel 3 (R9, 45°). Every track is 0.2 mm, every via 0.6 mm.
const COPIES: [Copy; 14] = [
    (
        "segment",
        "/Channel 2/MID",
        "F.Cu",
        false,
        &[(140_500_000, 98_000_000), (143_103_553, 98_000_000)],
    ),
    (
        "segment",
        "/Channel 2/MID",
        "F.Cu",
        true,
        &[(140_000_000, 99_500_000), (140_500_000, 98_000_000)],
    ),
    (
        "arc",
        "/Channel 2/MID",
        "F.Cu",
        false,
        &[
            (143_103_553, 98_000_000),
            (143_403_553, 97_551_777),
            (143_103_553, 97_103_553),
        ],
    ),
    (
        "segment",
        "/Channel 2/OUT",
        "F.Cu",
        false,
        &[(139_500_000, 98_000_000), (139_500_000, 96_800_000)],
    ),
    (
        "via",
        "/Channel 2/OUT",
        "F.Cu",
        false,
        &[(139_500_000, 96_800_000)],
    ),
    (
        "segment",
        "/Channel 2/OUT",
        "B.Cu",
        false,
        &[(142_500_000, 96_800_000), (142_500_000, 99_500_000)],
    ),
    (
        "segment",
        "/Channel 2/OUT",
        "B.Cu",
        false,
        &[(139_500_000, 96_800_000), (142_500_000, 96_800_000)],
    ),
    (
        "segment",
        "/Channel 3/MID",
        "F.Cu",
        false,
        &[(162_267_767, 119_189_340), (164_108_757, 121_030_330)],
    ),
    (
        "segment",
        "/Channel 3/MID",
        "F.Cu",
        true,
        &[(160_853_553, 119_896_447), (162_267_767, 119_189_340)],
    ),
    (
        "arc",
        "/Channel 3/MID",
        "F.Cu",
        false,
        &[
            (164_108_757, 121_030_330),
            (164_637_831, 120_925_521),
            (164_742_641, 120_396_446),
        ],
    ),
    (
        "segment",
        "/Channel 3/OUT",
        "F.Cu",
        false,
        &[(161_560_660, 118_482_233), (162_409_188, 117_633_705)],
    ),
    (
        "via",
        "/Channel 3/OUT",
        "F.Cu",
        false,
        &[(162_409_188, 117_633_705)],
    ),
    (
        "segment",
        "/Channel 3/OUT",
        "B.Cu",
        false,
        &[(164_530_509, 119_755_025), (162_621_320, 121_664_214)],
    ),
    (
        "segment",
        "/Channel 3/OUT",
        "B.Cu",
        false,
        &[(162_409_188, 117_633_705), (164_530_509, 119_755_025)],
    ),
];

/// pcbnew's copies of the area board's routing on nets Channel 1 shares, and
/// on VCC, which no pad of it is on, onto Channels 2 and 3: three GND vias,
/// the GND track, the VCC track and the IN stub each.
const AREA_COPIES: [Copy; 12] = [
    ("via", "GND", "F.Cu", false, &[(142_800_000, 94_400_000)]),
    ("via", "GND", "F.Cu", false, &[(141_500_000, 100_500_000)]),
    ("via", "GND", "F.Cu", false, &[(141_000_000, 95_600_000)]),
    (
        "segment",
        "GND",
        "B.Cu",
        false,
        &[(142_500_000, 100_500_000), (141_500_000, 100_500_000)],
    ),
    (
        "segment",
        "VCC",
        "F.Cu",
        false,
        &[(141_600_000, 99_000_000), (142_000_000, 99_000_000)],
    ),
    (
        "segment",
        "/Channel 2/IN",
        "F.Cu",
        false,
        &[(140_000_000, 100_500_000), (140_800_000, 100_500_000)],
    ),
    ("via", "GND", "F.Cu", false, &[(166_439_697, 118_270_101)]),
    ("via", "GND", "F.Cu", false, &[(161_207_107, 121_664_214)]),
    ("via", "GND", "F.Cu", false, &[(164_318_377, 117_845_837)]),
    (
        "segment",
        "GND",
        "B.Cu",
        false,
        &[(161_914_214, 122_371_321), (161_207_107, 121_664_214)],
    ),
    (
        "segment",
        "VCC",
        "F.Cu",
        false,
        &[(162_338_478, 120_674_264), (162_621_320, 120_957_107)],
    ),
    (
        "segment",
        "/Channel 3/IN",
        "F.Cu",
        false,
        &[(160_146_447, 120_603_554), (160_712_132, 121_169_239)],
    ),
];

/// Every copy the area board takes: the routed board's and [`AREA_COPIES`].
fn expected_area_copies() -> Vec<Routed> {
    let mut copies = expected_copies();
    copies.extend(routed_copies(&AREA_COPIES));
    copies.sort();
    copies
}

/// KiCad's own box of each item of the area board, by uuid, as `(left, top,
/// right, bottom)` in nm.
fn kicad_boxes() -> BTreeMap<String, (i64, i64, i64, i64)> {
    std::fs::read_to_string(Path::new(FIXTURE_DIR).join(BOXES))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let nm = |index: usize| fields[index].parse::<i64>().unwrap();
            let (x, y) = (nm(1), nm(2));
            (fields[0].to_string(), (x, y, x + nm(3), y + nm(4)))
        })
        .collect()
}

fn expected_copies() -> Vec<Routed> {
    let mut copies = routed_copies(&COPIES);
    copies.sort();
    copies
}

fn routed_copies(table: &[Copy]) -> Vec<Routed> {
    table
        .iter()
        .map(|(kind, net, layer, locked, points)| {
            let width = if *kind == "via" { 600_000 } else { 200_000 };
            (
                kind.to_string(),
                net.to_string(),
                layer.to_string(),
                *locked,
                points.to_vec(),
                width,
            )
        })
        .collect()
}

/// Each top-level track, arc and via of a board's text, by uuid.
fn routing(content: &str) -> BTreeMap<String, Routed> {
    let nm = |node: &SexpNode| {
        (
            mm_to_nm(node.get_f64(1).unwrap()),
            mm_to_nm(node.get_f64(2).unwrap()),
        )
    };
    parse_sexp(content)
        .unwrap()
        .children()
        .unwrap()
        .iter()
        .filter_map(|node| {
            let kind = node.head()?;
            let points: &[&str] = match kind {
                "segment" => &["start", "end"],
                "arc" => &["start", "mid", "end"],
                "via" => &["at"],
                _ => return None,
            };
            let layer = match kind {
                "via" => node.find("layers").unwrap().get(1).unwrap().as_str(),
                _ => node.find_str("layer"),
            };
            let width = node.find(if kind == "via" { "size" } else { "width" });
            Some((
                node.find_str("uuid").unwrap().to_string(),
                (
                    kind.to_string(),
                    copperpour_sexp::net::net_name(node.find("net").unwrap())
                        .unwrap()
                        .to_string(),
                    layer.unwrap().to_string(),
                    node.find_str("locked") == Some("yes"),
                    points.iter().map(|p| nm(node.find(p).unwrap())).collect(),
                    mm_to_nm(width.unwrap().get_f64(1).unwrap()),
                ),
            ))
        })
        .collect()
}

/// The items `after` holds that `before` did not, sorted.
fn added(before: &BTreeMap<String, Routed>, after: &BTreeMap<String, Routed>) -> Vec<Routed> {
    let mut added: Vec<Routed> = after
        .iter()
        .filter(|(id, _)| !before.contains_key(*id))
        .map(|(_, item)| item.clone())
        .collect();
    added.sort();
    added
}

/// The uuids `before` holds that `after` does not.
fn gone<'a>(
    before: &'a BTreeMap<String, Routed>,
    after: &BTreeMap<String, Routed>,
) -> Vec<&'a str> {
    before
        .keys()
        .filter(|id| !after.contains_key(*id))
        .map(String::as_str)
        .collect()
}

/// A response's `routing`, its targets sorted: they come in board order.
fn reported(body: &Value) -> Value {
    let mut routing = body["routing"].clone();
    if let Some(targets) = routing["targets"].as_array_mut() {
        targets.sort_by_key(|target| target["target_reference"].to_string());
    }
    routing
}

fn routing_report() -> Value {
    json!({
        "copied_count": 14,
        "removed_count": 3,
        "targets": [
            {
                "target_reference": "R5",
                "copied_count": 7,
                "removed_count": 3,
                "nets": { "/Channel 1/MID": "/Channel 2/MID", "/Channel 1/OUT": "/Channel 2/OUT" },
            },
            {
                "target_reference": "R9",
                "copied_count": 7,
                "removed_count": 0,
                "nets": { "/Channel 1/MID": "/Channel 3/MID", "/Channel 1/OUT": "/Channel 3/OUT" },
            },
        ],
    })
}

fn area_report() -> Value {
    let nets = |channel: &str| {
        json!({
            "/Channel 1/IN": format!("/Channel {channel}/IN"),
            "/Channel 1/MID": format!("/Channel {channel}/MID"),
            "/Channel 1/OUT": format!("/Channel {channel}/OUT"),
            "GND": "GND",
            "VCC": "VCC",
        })
    };
    json!({
        "copied_count": 26,
        "removed_count": 4,
        "targets": [
            { "target_reference": "R5", "copied_count": 13, "removed_count": 4, "nets": nets("2") },
            { "target_reference": "R9", "copied_count": 13, "removed_count": 0, "nets": nets("3") },
        ],
    })
}

#[tokio::test]
async fn routing_in_the_references_area_is_copied_where_kicad_puts_it() {
    let board = Board::new(ROUTED);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(reported(&body), routing_report(), "{body:#}");
    let after = board.now();
    assert_eq!(
        moved(&board.original, &after),
        expected(&[&CHANNEL_2, &CHANNEL_3_AT_45])
    );
    let (before, after) = (routing(&board.original), routing(&after));
    // Channel 1's IN and GND tracks leave its area, so only MID and OUT
    // move; Channel 2's three stubs lie in its area.
    assert_eq!(gone(&before, &after), [MID_STUB, OUT_TRACK, IN_STUB]);
    assert_eq!(added(&before, &after), expected_copies());
}

/// The area board: Channel 1's area takes the routing on GND, IN and VCC that
/// lies in it, and R4's group's rectangle brings in one more GND via, as does
/// the corner of the hull KiCad 10.0.8's outline cuts off. The GND track and
/// via that leave the area stay. Channel 2's GND via, in its area, goes.
#[tokio::test]
async fn routing_on_shared_nets_in_the_area_is_copied_where_kicad_puts_it() {
    let board = Board::new(AREA);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(reported(&body), area_report(), "{body:#}");
    let after = board.now();
    assert_eq!(
        moved(&board.original, &after),
        expected(&[&CHANNEL_2, &CHANNEL_3_AT_45])
    );
    let (before, after) = (routing(&board.original), routing(&after));
    assert_eq!(
        gone(&before, &after),
        [MID_STUB, OUT_TRACK, CHANNEL_2_VIA, IN_STUB]
    );
    assert_eq!(added(&before, &after), expected_area_copies());
}

/// The area board with R9 at 0° beside R1 and R10 above Channel 2's GND via
/// (README): R9's areas hold nine of the items Channel 1 copies and Channel
/// 2's IN stub, which R5's area takes first.
#[tokio::test]
async fn a_target_area_never_takes_the_references_routing_or_an_item_twice() {
    let board = Board::new("repeat_layout_area_overlap_kicad10.kicad_pcb");
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["routing"]["copied_count"], 26, "{body:#}");
    let (before, after) = (routing(&board.original), routing(&board.now()));
    // R5's four, then Channel 1's GND track and via that leave its area and
    // lie in R9's.
    assert_eq!(
        gone(&before, &after),
        [
            MID_STUB,
            OUT_TRACK,
            CHANNEL_2_VIA,
            IN_STUB,
            "8d01e129-399b-4e09-b0c2-ea9ef9e38fbc",
            "dc645c8a-d064-4193-b992-b0d145b9b575",
        ]
    );
    assert_eq!(added(&before, &after).len(), 26);
}

/// A closed board's text has no box without KiCad's fonts, so an area that
/// holds one, here in R4's group, is refused naming it.
#[tokio::test]
async fn a_group_member_a_closed_board_cannot_measure_is_refused_by_name() {
    let board = Board::new("repeat_layout_area_text_kicad10.kicad_pcb");
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    assert_eq!(
        reasons(&body),
        [
            "ca7bc0ed-7ddc-4e33-ad1c-e2469afedf9b, in group 'R4 block' with R4, has no box in \
             this read: KiCad measures it, so open the board in KiCad, or pass copy_routing false"
        ],
        "{body:#}"
    );
    board.assert_untouched();
}

/// The outline of Channel 1's area on the area board and of Channel 2's before
/// it is laid out, from KiCad's own boxes of the items the README's `oracle`
/// takes for each, rectified by KiCad's own `SHAPE_LINE_CHAIN` with the hull
/// closed, and the first carried onto Channel 3 at 45° as
/// `SHAPE_LINE_CHAIN::Rotate` and `Move` carry it.
#[test]
fn the_area_is_the_outline_kicad_draws() {
    use super::repeat_layout::{area_outline, carried, Rect};
    use copperpour_ipc::transform::rotate_point;
    let boxes = kicad_boxes();
    let outline = |items: &[&str]| {
        let rects: Vec<Rect> = items.iter().map(|item| boxes[*item]).collect();
        area_outline(&rects)
    };
    // R1 to R4, R4's group's rectangle, and the seven items on MID and OUT.
    let channel_1 = outline(&[
        "1a59d96d-1d8c-460f-b863-40f1564308b8",
        "246a3eb1-5e9f-4ed5-bad3-9272659c1bb9",
        "2cc52ae4-bbf6-4b08-a654-5db627f89247",
        "34dfb1e3-9223-4dfe-ba56-b3dda1c8ee37",
        "492a8b98-88c6-4604-b847-3075ac91ad54",
        "6fdea30f-eba7-49e8-9d98-743aad07239f",
        "7504bdfe-53d7-4dc8-b2ca-4930bc02258a",
        "9ec35771-89a8-4ce1-b44d-a258d752a8b5",
        "b0e8547a-c751-474c-80fc-b203686b4441",
        "bb6ccaed-e077-4848-ae97-d1aa1c7267af",
        "c2329086-ba61-43d7-b777-9ff680866189",
        "e9fd3d26-9ffa-48a2-9d4a-615f6c52eee0",
    ]);
    // KiCad 10.0.8's own outline cuts off the corner (113.6, 99.1) to
    // (116.75, 101.85) of the hull, and (111.7, 103.7) to (109.1, 102.85).
    assert_eq!(
        channel_1,
        [
            (109_100_000, 99_650_000),
            (109_100_000, 99_100_000),
            (116_750_000, 99_100_000),
            (116_750_000, 103_750_000),
            (111_699_999, 103_750_000),
            (111_699_999, 103_703_555),
            (109_100_000, 103_703_555),
        ]
    );
    // R5 to R8 where they are, and Channel 2's MID stub, OUT track and IN stub.
    let channel_2 = outline(&[
        "08cf5053-ed54-4206-b23a-3d693ed67fa3",
        "31600868-f704-4c68-9771-b5d6939a409c",
        "4185e791-a0e4-41b0-9ec4-ee2d350ea060",
        "6c43e0b0-05cb-4524-9a73-d44fe83ec0b8",
        "83c5914a-6a09-43cf-847c-d4e8240c7e79",
        "83f7bbfe-51d3-426c-9c8e-3125fda56377",
        "cdd253fc-bf55-419f-9443-b5d59173f748",
    ]);
    assert_eq!(
        channel_2,
        [
            (134_100_000, 109_650_000),
            (134_100_000, 97_800_000),
            (140_900_001, 97_800_000),
            (140_900_001, 110_350_000),
            (139_900_000, 110_350_000),
            (139_900_000, 111_700_001),
            (134_100_000, 111_700_001),
        ]
    );
    // R1 at (110, 100) 0° onto R9 at (160.5, 120.25) 45°.
    let (x, y) = rotate_point(110_000_000, 100_000_000, 45.0);
    let offset = (160_500_000 - x, 120_250_000 - y);
    assert_eq!(
        carried(&channel_1, 45.0, offset),
        [
            (159_616_117, 120_638_909),
            (159_227_208, 120_250_000),
            (164_636_575, 114_840_633),
            (167_924_621, 118_128_680),
            (164_353_731, 121_699_570),
            (164_320_890, 121_666_728),
            (162_482_413, 123_505_205),
        ]
    );
}

/// R4's group inside another group, whose second rectangle brings one more
/// GND via inside Channel 1's area (README). The wider hull's slanted edge is
/// where KiCad 10.0.8's own outline would cut off Channel 1's MID track and
/// arc, on its own net; both are copied.
#[tokio::test]
async fn a_group_inside_a_group_widens_the_area() {
    let board = Board::new("repeat_layout_area_nested_kicad10.kicad_pcb");
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["routing"]["copied_count"], 28, "{body:#}");
    let (before, after) = (routing(&board.original), routing(&board.now()));
    let mut copies = expected_area_copies();
    copies.extend(routed_copies(&[
        ("via", "GND", "F.Cu", false, &[(144_600_000, 94_400_000)]),
        ("via", "GND", "F.Cu", false, &[(167_712_489, 119_542_893)]),
    ]));
    copies.sort();
    assert_eq!(added(&before, &after), copies);
}

#[tokio::test]
async fn a_second_call_replaces_the_copies_with_the_same_copies() {
    // On the area board, R4's group widens Channel 1's area and not the
    // targets', so a copy can lie outside a target's own area; the
    // reference's area carried onto it holds every copy.
    let board = Board::new(AREA);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    let first = board.now();
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["routing"]["removed_count"], 26, "{body:#}");
    let second = board.now();
    assert!(moved(&first, &second).is_empty());
    let (first, second) = (routing(&first), routing(&second));
    assert_eq!(gone(&first, &second).len(), 26);
    let mut kept: Vec<Routed> = first.into_values().collect();
    let mut again: Vec<Routed> = second.into_values().collect();
    kept.sort();
    again.sort();
    assert_eq!(again, kept);
}

#[tokio::test]
async fn named_targets_alone_take_the_routing() {
    let board = Board::new(ROUTED);
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R1", "target_references": ["R9"] }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["routing"]["copied_count"], 7, "{body:#}");
    assert_eq!(body["routing"]["removed_count"], 0, "{body:#}");
    let after = board.now();
    assert_eq!(
        moved(&board.original, &after),
        expected(&[&CHANNEL_3_AT_45])
    );
    let (before, after) = (routing(&board.original), routing(&after));
    assert!(gone(&before, &after).is_empty());
    let channel_3: Vec<Routed> = expected_copies()
        .into_iter()
        .filter(|(_, net, ..)| net.starts_with("/Channel 3/"))
        .collect();
    assert_eq!(added(&before, &after), channel_3);
}

#[tokio::test]
async fn copy_routing_false_places_the_footprints_alone() {
    let board = Board::new(ROUTED);
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R1", "copy_routing": false }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert!(body.get("routing").is_none(), "{body:#}");
    let after = board.now();
    assert_eq!(
        moved(&board.original, &after),
        expected(&[&CHANNEL_2, &CHANNEL_3_AT_45])
    );
    assert_eq!(routing(&after), routing(&board.original));

    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R1", "copy_routing": "yes" }),
    )
    .await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["field"], "copy_routing", "{body:#}");
}

#[tokio::test]
async fn a_dry_run_reports_the_routing_and_writes_nothing() {
    let board = Board::new(ROUTED);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1", "dry_run": true })).await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["dry_run"], true, "{body:#}");
    assert_eq!(reported(&body), routing_report(), "{body:#}");
    board.assert_untouched();
}

#[tokio::test]
async fn a_net_that_does_not_become_one_net_of_the_target_is_refused() {
    let board = Board::new(ROUTED_MISMATCH);
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    assert_eq!(
        reasons(&body),
        [
            "/Channel 1/MID and /Channel 1/OUT both become /Channel 3/MID on R9's placement",
            "/Channel 1/MID becomes /Channel 2/MID and /Channel 2/OUT on R5's placement",
            "/Channel 1/OUT becomes /Channel 2/OUT, which has pads off R5's placement",
        ],
        "{body:#}"
    );
    board.assert_untouched();

    // Without the routing, the footprints alone match.
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R1", "copy_routing": false, "dry_run": true }),
    )
    .await;
    assert!(!is_error, "{body:#}");
}

/// The area board with R7's and R8's pad 2 on VCC (README): Channel 1's GND
/// becomes VCC on R5's placement, which the copied VCC track keeps as its own.
#[tokio::test]
async fn a_net_kept_as_its_own_is_one_of_two_that_cannot_become_one() {
    let board = Board::new("repeat_layout_area_merge_kicad10.kicad_pcb");
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R1" })).await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
    assert_eq!(
        reasons(&body),
        ["GND and VCC both become VCC on R5's placement"],
        "{body:#}"
    );
    board.assert_untouched();
}

/// `models-3d/royalblue.kicad_pcb` is a board KiCad 9's pcbnew wrote (its
/// README): its tracks name their nets by number, which KiCad 10 never
/// writes, so their nets cannot be read by name.
#[tokio::test]
async fn a_board_naming_track_nets_by_number_is_refused_unless_routing_is_left() {
    let board = Board::new("models-3d/royalblue.kicad_pcb");
    let (is_error, body) = call(&board, json!({ "anchor_reference": "R5" })).await;
    assert!(is_error, "{body:#}");
    let text = body.to_string();
    assert!(
        text.contains("names its net by number") && text.contains("copy_routing false"),
        "{body:#}"
    );
    board.assert_untouched();

    // Without the routing the call reaches the plan, which refuses the
    // anchor: this board has no sheet placed more than once.
    let (is_error, body) = call(
        &board,
        json!({ "anchor_reference": "R5", "copy_routing": false }),
    )
    .await;
    assert!(is_error, "{body:#}");
    assert_eq!(body["error"]["field"], "anchor_reference", "{body:#}");
    board.assert_untouched();
}

/// pcbnew 10.0.6 turns these points by 45° with `EDA_ANGLE`'s `M_SQRT1_2`,
/// which puts `y` a nanometre from where the sine of 45° in radians would:
/// the README's `rotate_45`, through `PCB_TRACK::Rotate`.
#[test]
fn a_45_degree_turn_rounds_as_kicad_rounds_it() {
    use copperpour_ipc::transform::rotate_point;
    assert_eq!(
        rotate_point(150_000_000, 111_386_035, 45.0),
        (184_827_838, -27_304_197)
    );
    assert_eq!(
        rotate_point(150_000_001, 134_005_573, 45.0),
        (200_822_267, -11_309_769)
    );
}

// ─── Routing on a live board ──────────────────────────────────────────────────

/// The routed fixture as KiCad sends it: [`as_kicad_sends`]' footprints with
/// each pad's KIID, number and net, and every track, arc and via, a via's
/// padstack its one size on every layer.
fn as_kicad_sends_routed(content: &str) -> Vec<prost_types::Any> {
    use kiapi::board::types as board;
    use kiapi::common::types as common;
    let kiid = |node: &SexpNode| {
        Some(common::Kiid {
            value: node.find_str("uuid").unwrap().to_string(),
        })
    };
    let net = |node: &SexpNode| {
        node.find("net")
            .and_then(copperpour_sexp::net::net_name)
            .map(|name| board::Net {
                code: None,
                name: name.to_string(),
            })
    };
    let point = |node: &SexpNode, tag: &str| {
        let at = node.find(tag).unwrap();
        Some(common::Vector2 {
            x_nm: mm_to_nm(at.get_f64(1).unwrap()),
            y_nm: mm_to_nm(at.get_f64(2).unwrap()),
        })
    };
    let nm = |node: &SexpNode, tag: &str| mm_to_nm(node.find(tag).unwrap().get_f64(1).unwrap());
    let locked = |node: &SexpNode| {
        if node.find_str("locked") == Some("yes") {
            common::LockedState::LsLocked as i32
        } else {
            common::LockedState::LsUnlocked as i32
        }
    };
    let layer = |name: &str| copperpour_ipc::builders::layer_from_name(name) as i32;
    let mut items: Vec<prost_types::Any> = as_kicad_sends(content)
        .into_iter()
        .zip(footprint_nodes(content))
        .map(|(item, node)| {
            let mut fp = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let definition = fp.definition.as_mut().unwrap();
            for pad in node.find_all("pad") {
                let pad = board::Pad {
                    id: kiid(pad),
                    number: pad.get(1).and_then(SexpNode::as_str).unwrap().to_string(),
                    net: net(pad),
                    ..Default::default()
                };
                definition
                    .items
                    .push(pack_any(&pad, "kiapi.board.types.Pad"));
            }
            pack_any(&fp, "kiapi.board.types.FootprintInstance")
        })
        .collect();
    for node in parse_sexp(content).unwrap().children().unwrap() {
        match node.head() {
            Some("segment") => items.push(pack_any(
                &board::Track {
                    id: kiid(node),
                    start: point(node, "start"),
                    end: point(node, "end"),
                    width: Some(common::Distance {
                        value_nm: nm(node, "width"),
                    }),
                    locked: locked(node),
                    layer: layer(node.find_str("layer").unwrap()),
                    net: net(node),
                    ..Default::default()
                },
                "kiapi.board.types.Track",
            )),
            Some("arc") => items.push(pack_any(
                &board::Arc {
                    id: kiid(node),
                    start: point(node, "start"),
                    mid: point(node, "mid"),
                    end: point(node, "end"),
                    width: Some(common::Distance {
                        value_nm: nm(node, "width"),
                    }),
                    locked: locked(node),
                    layer: layer(node.find_str("layer").unwrap()),
                    net: net(node),
                    ..Default::default()
                },
                "kiapi.board.types.Arc",
            )),
            Some("via") => {
                let size = nm(node, "size");
                let copper = [board::BoardLayer::BlFCu, board::BoardLayer::BlBCu].map(|layer| {
                    board::PadStackLayer {
                        layer: layer as i32,
                        size: Some(common::Vector2 {
                            x_nm: size,
                            y_nm: size,
                        }),
                        ..Default::default()
                    }
                });
                items.push(pack_any(
                    &board::Via {
                        id: kiid(node),
                        position: point(node, "at"),
                        pad_stack: Some(board::PadStack {
                            r#type: board::PadStackType::PstNormal as i32,
                            copper_layers: copper.to_vec(),
                            ..Default::default()
                        }),
                        locked: locked(node),
                        net: net(node),
                        r#type: board::ViaType::VtThrough as i32,
                        ..Default::default()
                    },
                    "kiapi.board.types.Via",
                ));
            }
            Some("group") => items.push(pack_any(
                &board::Group {
                    id: kiid(node),
                    name: node.get(1).and_then(SexpNode::as_str).unwrap().to_string(),
                    items: node.find("members").unwrap().children().unwrap()[1..]
                        .iter()
                        .map(|member| common::Kiid {
                            value: member.as_str().unwrap().to_string(),
                        })
                        .collect(),
                    ..Default::default()
                },
                "kiapi.board.types.Group",
            )),
            _ => {}
        }
    }
    items
}

/// KiCad's answer to `GetBoundingBox`: its own boxes of the area board's items
/// ([`BOXES`]).
fn answer_with_kicads_boxes(command: &prost_types::Any) -> Option<prost_types::Any> {
    if any_type_name(command) != "kiapi.common.commands.GetBoundingBox" {
        return None;
    }
    let boxes = kicad_boxes();
    let mm = |nm: i64| nm as f64 / 1e6;
    Some(crate::tools::pcb_board::board_mock::kicad_bounding_boxes(
        command,
        |id| {
            let (left, top, right, bottom) = boxes[id];
            (mm(left), mm(top), mm(right - left), mm(bottom - top))
        },
    ))
}

/// What the copper check asks KiCad, answered for two copper layers and a
/// 0.1 mm square at the origin for every pad: an outline the moves carry far
/// from any copper, so the check passes and the routing is what is tested.
fn answer_the_copper_check(command: &prost_types::Any) -> Option<prost_types::Any> {
    use kiapi::board::commands;
    use kiapi::common::types as common;
    match any_type_name(command) {
        "kiapi.board.commands.GetBoardEnabledLayers" => Some(pack_any(
            &commands::BoardEnabledLayersResponse {
                copper_layer_count: 2,
                layers: vec![
                    kiapi::board::types::BoardLayer::BlFCu as i32,
                    kiapi::board::types::BoardLayer::BlBCu as i32,
                ],
            },
            "kiapi.board.commands.BoardEnabledLayersResponse",
        )),
        "kiapi.board.commands.CheckPadstackPresenceOnLayers" => Some(pack_any(
            &commands::PadstackPresenceResponse::default(),
            "kiapi.board.commands.PadstackPresenceResponse",
        )),
        "kiapi.board.commands.GetPadShapeAsPolygon" => {
            let request = commands::GetPadShapeAsPolygon::decode(command.value.as_slice()).unwrap();
            let corner = |x_nm, y_nm| common::PolyLineNode {
                geometry: Some(common::poly_line_node::Geometry::Point(common::Vector2 {
                    x_nm,
                    y_nm,
                })),
            };
            let square = common::PolygonWithHoles {
                outline: Some(common::PolyLine {
                    nodes: vec![
                        corner(0, 0),
                        corner(100_000, 0),
                        corner(100_000, 100_000),
                        corner(0, 100_000),
                    ],
                    closed: true,
                }),
                holes: vec![],
            };
            Some(pack_any(
                &commands::PadShapeAsPolygonResponse {
                    polygons: vec![square; request.pads.len()],
                    pads: request.pads,
                },
                "kiapi.board.commands.PadShapeAsPolygonResponse",
            ))
        }
        _ => None,
    }
}

impl Kicad {
    /// A KiCad holding the routed fixture. `settle` stands in for what KiCad
    /// does to an item on the commit's push; `create`, when it answers, for
    /// its answer to `CreateItems`. The log names an `EndCommit` by what it
    /// did, `EndCommit(commit)` or `EndCommit(drop)`.
    async fn routed(
        fixture: &str,
        settle: impl Fn(&mut prost_types::Any) + Send + 'static,
        create: impl Fn(&prost_types::Any, &mut ItemStore) -> Option<prost_types::Any> + Send + 'static,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join(fixture);
        std::fs::copy(Path::new(FIXTURE_DIR).join(fixture), &board).unwrap();
        let content = std::fs::read_to_string(&board).unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: as_kicad_sends_routed(&content),
            ..Default::default()
        }));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let server = spawn_kicad_holding_items(&board, vec![], store.clone(), settle, {
            move |command, store| {
                use kiapi::common::commands::{CommitAction, EndCommit};
                let mut name = any_type_name(command)
                    .rsplit('.')
                    .next()
                    .unwrap()
                    .to_string();
                if name == "EndCommit" {
                    let end = EndCommit::decode(command.value.as_slice()).unwrap();
                    name = match CommitAction::try_from(end.action) {
                        Ok(CommitAction::CmaDrop) => "EndCommit(drop)",
                        _ => "EndCommit(commit)",
                    }
                    .to_string();
                }
                log.lock().unwrap().push(name);
                if any_type_name(command) == "kiapi.common.commands.CreateItems" {
                    if let Some(answer) = create(command, store) {
                        return Some(answer);
                    }
                }
                answer_the_copper_check(command).or_else(|| answer_with_kicads_boxes(command))
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

    /// Each track, arc and via KiCad now holds, by KIID.
    fn routing(&self) -> BTreeMap<String, Routed> {
        use kiapi::board::types as board;
        let layer = |layer: i32| {
            copperpour_ipc::builders::layer_name(board::BoardLayer::try_from(layer).unwrap())
                .unwrap()
                .to_string()
        };
        let xy = |point: &Option<kiapi::common::types::Vector2>| {
            let point = point.unwrap();
            (point.x_nm, point.y_nm)
        };
        let locked = |state: i32| state == kiapi::common::types::LockedState::LsLocked as i32;
        let mut held = BTreeMap::new();
        for item in &self.store.lock().unwrap().items {
            let bytes = item.value.as_slice();
            let (id, routed) = if any_is(item, "kiapi.board.types.Track") {
                let t = board::Track::decode(bytes).unwrap();
                let routed = (
                    "segment",
                    t.net,
                    layer(t.layer),
                    locked(t.locked),
                    vec![xy(&t.start), xy(&t.end)],
                    t.width.unwrap().value_nm,
                );
                (t.id, routed)
            } else if any_is(item, "kiapi.board.types.Arc") {
                let a = board::Arc::decode(bytes).unwrap();
                let points = vec![xy(&a.start), xy(&a.mid), xy(&a.end)];
                let routed = (
                    "arc",
                    a.net,
                    layer(a.layer),
                    locked(a.locked),
                    points,
                    a.width.unwrap().value_nm,
                );
                (a.id, routed)
            } else if any_is(item, "kiapi.board.types.Via") {
                let v = board::Via::decode(bytes).unwrap();
                let size = v.pad_stack.unwrap().copper_layers[0].size.unwrap().x_nm;
                let routed = (
                    "via",
                    v.net,
                    "F.Cu".to_string(),
                    locked(v.locked),
                    vec![xy(&v.position)],
                    size,
                );
                (v.id, routed)
            } else {
                continue;
            };
            let (kind, net, layer, locked, points, width) = routed;
            held.insert(
                id.unwrap().value,
                (
                    kind.to_string(),
                    net.unwrap().name,
                    layer,
                    locked,
                    points,
                    width,
                ),
            );
        }
        held
    }
}

/// KiCad's own answer to `CreateItems`: the mock's.
fn as_kicad(_: &prost_types::Any, _: &mut ItemStore) -> Option<prost_types::Any> {
    None
}

#[tokio::test]
async fn a_live_board_takes_the_routing_in_the_layouts_one_commit() {
    let kicad = Kicad::routed(ROUTED, |_| {}, as_kicad).await;
    let (before, footprints) = (kicad.routing(), kicad.held());
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["source"], "ipc", "{body:#}");
    assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
    // The board's ten tracks less the three the same commit removes.
    assert_eq!(
        body["copper_check"]["coverage"]["track_count"], 7,
        "{body:#}"
    );
    assert_eq!(reported(&body), routing_report(), "{body:#}");
    assert!(body.get("warning").is_none(), "{body:#}");
    let moved: BTreeMap<String, Pose> = kicad
        .held()
        .into_iter()
        .filter(|(reference, pose)| footprints.get(reference) != Some(pose))
        .collect();
    assert_eq!(moved, expected(&[&CHANNEL_2, &CHANNEL_3_AT_45]));
    let after = kicad.routing();
    assert_eq!(gone(&before, &after), [MID_STUB, OUT_TRACK, IN_STUB]);
    assert_eq!(added(&before, &after), expected_copies());
    // The deletion and the copies go inside the placements' commit.
    let sent = kicad.sent();
    let writes: Vec<&str> = sent
        .iter()
        .map(String::as_str)
        .filter(|name| {
            name.starts_with("EndCommit")
                || matches!(
                    *name,
                    "BeginCommit" | "UpdateItems" | "DeleteItems" | "CreateItems"
                )
        })
        .collect();
    assert_eq!(
        writes,
        [
            "BeginCommit",
            "UpdateItems",
            "DeleteItems",
            "CreateItems",
            "EndCommit(commit)"
        ],
        "{sent:?}"
    );
    assert!(sent.iter().any(|name| name == "GetItemsById"), "{sent:?}");
}

#[tokio::test]
async fn a_copy_kicad_refuses_drops_the_whole_commit() {
    use kiapi::common::commands;
    let kicad = Kicad::routed(
        ROUTED,
        |_| {},
        |command, _| {
            let request = commands::CreateItems::decode(command.value.as_slice()).unwrap();
            let results = (0..request.items.len())
                .map(|index| commands::ItemCreationResult {
                    status: Some(commands::ItemStatus {
                        code: if index == 3 {
                            commands::ItemStatusCode::IscInvalidData
                        } else {
                            commands::ItemStatusCode::IscOk
                        } as i32,
                        error_message: String::new(),
                    }),
                    item: None,
                })
                .collect();
            Some(pack_any(
                &commands::CreateItemsResponse {
                    header: None,
                    status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                    created_items: results,
                },
                "kiapi.common.commands.CreateItemsResponse",
            ))
        },
    )
    .await;
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(is_error, "{body:#}");
    assert!(
        body.to_string().contains("refused 1 of the routing copies"),
        "{body:#}"
    );
    let sent = kicad.sent();
    assert!(
        sent.iter().any(|name| name == "EndCommit(drop)"),
        "{sent:?}"
    );
    assert!(
        !sent.iter().any(|name| name == "EndCommit(commit)"),
        "{sent:?}"
    );
}

#[tokio::test]
async fn a_copy_kicad_gives_no_kiid_for_is_named() {
    // KiCad may answer a created item without echoing it; the copy is made
    // but cannot be read back by KIID.
    use kiapi::common::commands;
    let kicad = Kicad::routed(
        ROUTED,
        |_| {},
        |command, store| {
            let request = commands::CreateItems::decode(command.value.as_slice()).unwrap();
            let mut results = Vec::new();
            for (index, item) in request.items.into_iter().enumerate() {
                use kiapi::board::types as board;
                store.next_id += 1;
                let id = Some(kiapi::common::types::Kiid {
                    value: format!("created-{}", store.next_id),
                });
                let bytes = item.value.as_slice();
                let item = match any_type_name(&item) {
                    "kiapi.board.types.Track" => {
                        let mut track = board::Track::decode(bytes).unwrap();
                        track.id = id;
                        pack_any(&track, "kiapi.board.types.Track")
                    }
                    "kiapi.board.types.Arc" => {
                        let mut arc = board::Arc::decode(bytes).unwrap();
                        arc.id = id;
                        pack_any(&arc, "kiapi.board.types.Arc")
                    }
                    _ => {
                        let mut via = board::Via::decode(bytes).unwrap();
                        via.id = id;
                        pack_any(&via, "kiapi.board.types.Via")
                    }
                };
                store.items.push(item.clone());
                results.push(commands::ItemCreationResult {
                    status: Some(commands::ItemStatus {
                        code: commands::ItemStatusCode::IscOk as i32,
                        error_message: String::new(),
                    }),
                    item: (index != 0).then_some(item),
                });
            }
            Some(pack_any(
                &commands::CreateItemsResponse {
                    header: None,
                    status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                    created_items: results,
                },
                "kiapi.common.commands.CreateItemsResponse",
            ))
        },
    )
    .await;
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    // The first copy came back without its KIID; the other 13 are read back.
    let unconfirmed = body["routing"]["unconfirmed"].as_array().unwrap();
    assert_eq!(unconfirmed.len(), 1, "{body:#}");
    assert!(unconfirmed[0]["kiid"].is_null(), "{body:#}");
    assert!(unconfirmed[0]["held_net"].is_null(), "{body:#}");
    assert!(unconfirmed[0]["net"].is_string(), "{body:#}");
    assert!(
        body["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("routing.unconfirmed")),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_live_board_without_copy_routing_takes_the_footprints_alone() {
    let kicad = Kicad::routed(ROUTED, |_| {}, as_kicad).await;
    let before = kicad.routing();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1", "copy_routing": false, "allow_copper_conflicts": true }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert!(body.get("routing").is_none(), "{body:#}");
    assert_eq!(kicad.routing(), before);
    let store = kicad.store.lock().unwrap();
    assert_eq!((store.updates, store.creates, store.deletes), (1, 0, 0));
}

#[tokio::test]
async fn a_copy_kicad_holds_on_another_net_is_named() {
    // KiCad puts an item on the net of a pad it touches when the commit is
    // pushed, and answers the create with the net it was asked for.
    let kicad = Kicad::routed(
        ROUTED,
        |item| {
            if any_is(item, "kiapi.board.types.Via") {
                let mut via = kiapi::board::types::Via::decode(item.value.as_slice()).unwrap();
                if via
                    .net
                    .as_ref()
                    .is_some_and(|net| net.name == "/Channel 3/OUT")
                {
                    via.net = Some(copperpour_ipc::builders::net("GND", 0));
                    *item = pack_any(&via, "kiapi.board.types.Via");
                }
            }
        },
        as_kicad,
    )
    .await;
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    let unconfirmed = body["routing"]["unconfirmed"].as_array().unwrap();
    assert_eq!(unconfirmed.len(), 1, "{body:#}");
    assert!(unconfirmed[0]["kiid"].is_string(), "{body:#}");
    assert_eq!(unconfirmed[0]["net"], "/Channel 3/OUT", "{body:#}");
    assert_eq!(unconfirmed[0]["held_net"], "GND", "{body:#}");
    assert!(
        body["warning"]
            .as_str()
            .is_some_and(|warning| warning.starts_with("1 of the copied")),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_live_routing_dry_run_only_reads() {
    let kicad = Kicad::routed(ROUTED, |_| {}, as_kicad).await;
    let before = kicad.routing();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1", "dry_run": true }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["dry_run"], true, "{body:#}");
    assert_eq!(reported(&body), routing_report(), "{body:#}");
    assert_eq!(kicad.routing(), before);
    let store = kicad.store.lock().unwrap();
    assert_eq!((store.updates, store.creates, store.deletes), (0, 0, 0));
    for name in kicad.sent() {
        assert!(
            matches!(
                name.as_str(),
                "GetOpenDocuments"
                    | "GetItems"
                    | "GetVersion"
                    | "GetBoundingBox"
                    | "GetBoardEnabledLayers"
                    | "CheckPadstackPresenceOnLayers"
                    | "GetPadShapeAsPolygon"
            ),
            "{name} reached KiCad"
        );
    }
}

/// The area board on a live board: KiCad lists R4's group and measures every
/// item, and the copies are those of the closed board.
#[tokio::test]
async fn a_live_board_draws_the_area_from_kicads_groups_and_boxes() {
    let kicad = Kicad::routed(AREA, |_| {}, as_kicad).await;
    let before = kicad.routing();
    let (is_error, body) = call_with(
        &kicad.handler,
        &kicad.board,
        json!({ "anchor_reference": "R1" }),
    )
    .await;
    assert!(!is_error, "{body:#}");
    assert_eq!(body["source"], "ipc", "{body:#}");
    assert_eq!(reported(&body), area_report(), "{body:#}");
    let after = kicad.routing();
    assert_eq!(
        gone(&before, &after),
        [MID_STUB, OUT_TRACK, CHANNEL_2_VIA, IN_STUB]
    );
    assert_eq!(added(&before, &after), expected_area_copies());
    let sent = kicad.sent();
    assert!(sent.iter().any(|name| name == "GetBoundingBox"), "{sent:?}");
}
