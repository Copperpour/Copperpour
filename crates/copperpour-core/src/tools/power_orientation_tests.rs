use super::batch_power_tests::{handler, pins, serve};
use super::*;
use crate::mcp::handler::McpHandler;
use copperpour_sexp::SexpNode;
use serde_json::Value;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

type BeforePowerReadback = Box<dyn FnOnce(&Path)>;

thread_local! {
    static LATE_SAVE: RefCell<Option<(PathBuf, BeforePowerReadback)>> = const { RefCell::new(None) };
}

fn save_before_power_readback(path: &Path, edit: impl FnOnce(&Path) + 'static) {
    LATE_SAVE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some((path.to_owned(), Box::new(edit)));
    });
}

/// Simulate an editor save after the writer's atomic byte readback, but before
/// the tool checks that the saved sheet still contains its requested edits.
pub(super) fn run_before_power_readback(path: &Path) {
    let edit = LATE_SAVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(target, _)| target == path) {
            slot.take().map(|(_, edit)| edit)
        } else {
            None
        }
    });
    if let Some(edit) = edit {
        edit(path);
    }
}

fn remove_one_new_saved_item(
    path: &Path,
    tag: &str,
    before: &SexpNode,
    is_target: impl Fn(&SexpNode) -> bool,
) {
    let saved = read_consistent(path).unwrap();
    let existing = before.find_all(tag);
    let mut spans = direct_children_with_tag(&saved, "kicad_sch", tag)
        .into_iter()
        .filter(|&(start, end)| {
            let node = parse_sexp(&saved[start..end]).unwrap();
            is_target(&node) && !existing.contains(&&node)
        });
    let (start, end) = spans.next().expect("the writer saved the new item");
    assert!(spans.next().is_none(), "the edit must remove exactly one item");
    let edited = apply_edits(saved.clone(), vec![SexpEdit::delete(start, end)]);
    write_atomic_if_unchanged(path, &saved, &edited).unwrap();
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("power_orientation.kicad_sch");
    std::fs::write(
        &path,
        include_bytes!(
            "../../tests/fixtures/power_orientation_kicad10/power_orientation.kicad_sch"
        ),
    )
    .unwrap();
    (dir, path)
}

async fn place(handler: &McpHandler, path: &Path, net: &str, pin: &str) -> Value {
    let (reference, pin_number) = pin.split_once('.').unwrap();
    let reply = serve(
        handler,
        "add_power_symbol",
        path,
        json!({
            "power_net": net, "reference": reference, "pin_number": pin_number,
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    reply
}

fn check_placement(
    path: &Path,
    reply: &Value,
    (x, y): (f64, f64),
    rotation: f64,
    start: (f64, f64),
) {
    let (_, tree) = read_schematic(path).unwrap();
    let placed = extract_symbol_instances(&tree)
        .into_iter()
        .find(|s| s.uuid.as_deref() == reply["uuid"].as_str())
        .unwrap();
    assert_eq!(
        (placed.x, placed.y, placed.rotation),
        (x, y, rotation),
        "{reply}"
    );
    assert_eq!(reply["x"], x, "{reply}");
    assert_eq!(reply["y"], y, "{reply}");
    assert_eq!(reply["rotation"], rotation, "{reply}");
    let wires = extract_wires(&tree);
    let w = wires
        .iter()
        .find(|w| w.uuid.as_deref() == reply["wire"]["uuid"].as_str())
        .unwrap();
    assert_eq!(((w.x1, w.y1), (w.x2, w.y2)), (start, (x, y)), "{reply}");
    assert_eq!(
        reply["wire"],
        json!({"uuid": w.uuid, "x1": start.0, "y1": start.1, "x2": x, "y2": y})
    );
}

#[tokio::test]
async fn adjacent_side_facing_pins_keep_the_library_direction_without_overlap_or_contact() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let count_before = read_schematic(&path).unwrap().1.find_all("wire").len();
    let supply = place(&handler, &path, "+3V3", "J1.1").await;
    let ground = place(&handler, &path, "GND", "J1.2").await;
    check_placement(&path, &supply, (71.12, 63.5), 0.0, (68.58, 63.5));
    check_placement(&path, &ground, (71.12, 66.04), 0.0, (68.58, 66.04));
    for reply in [supply, ground] {
        assert_eq!(reply["orientation"], "library", "{reply}");
        assert!(reply.get("orientation_reason").is_none(), "{reply}");
        assert_eq!(reply["text_overlaps"], json!([]), "{reply}");
        assert_eq!(reply["junctions_added"], json!([]), "{reply}");
    }
    let tree = read_schematic(&path).unwrap().1;
    assert_eq!(tree.find_all("wire").len(), count_before + 2);
    let contacts = serve(&handler, "find_pin_contacts", &path, json!({})).await;
    assert_eq!(contacts["contact_count"], 0, "{contacts}");
    let overlaps = serve(&handler, "check_schematic_overlaps", &path, json!({})).await;
    assert_eq!(overlaps["overlap_count"], 0, "{overlaps}");
}

async fn facing_rails(order: [(&str, &str, f64, f64); 2]) {
    let handler = handler().await;
    let (_dir, path) = fixture();
    for (index, (net, pin, y, rotation)) in order.into_iter().enumerate() {
        let reply = place(&handler, &path, net, pin).await;
        check_placement(&path, &reply, (71.12, y), rotation, (68.58, y));
        assert_eq!(reply["orientation"], "sideways", "{reply}");
        assert!(
            reply["orientation_reason"]
                .as_str()
                .unwrap()
                .contains(if index == 0 {
                    "stub corridor"
                } else {
                    "component_overlap"
                }),
            "{reply}"
        );
        let overlaps = reply["text_overlaps"].as_array().unwrap();
        assert_eq!(overlaps.len(), index, "{reply}");
        if let Some(finding) = overlaps.first() {
            assert_eq!(finding["type"], "text_overlap");
            assert!(
                ["a", "b"]
                    .into_iter()
                    .any(|side| finding[side]["text"] == order[0].0),
                "{finding}"
            );
        }
    }
}

const J2_SUPPLY: (&str, &str, f64, f64) = ("+3V3", "J2.1", 88.9, 270.0);
const J2_GROUND: (&str, &str, f64, f64) = ("GND", "J2.2", 86.36, 90.0);

#[tokio::test]
async fn the_empty_neighbour_corridor_blocks_supply_first() {
    facing_rails([J2_SUPPLY, J2_GROUND]).await;
}

#[tokio::test]
async fn the_empty_neighbour_corridor_blocks_ground_first() {
    facing_rails([J2_GROUND, J2_SUPPLY]).await;
}

#[tokio::test]
async fn left_facing_lone_batch_pins_use_stubs_and_report_the_label_covered_by_fallback() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let vcc = place(&handler, &path, "+3V3", "J6.1").await;
    check_placement(&path, &vcc, (114.3, 111.76), 0.0, (116.84, 111.76));
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "pins": pins(&["J6.3", "J6.6"]),
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    assert_eq!(reply["placed_count"], 2, "{reply}");
    assert_eq!(reply["wires_added_count"], 2, "{reply}");
    let pin3 = &reply["groups"][0];
    let pin6 = &reply["groups"][1];
    for group in [pin3, pin6] {
        assert_eq!(group["joined"], false, "{group}");
        assert_eq!(
            group["wire_uuids"],
            json!([group["symbol"]["wire"]["uuid"]]),
            "{group}"
        );
        assert!(
            group["not_joined_reason"]
                .as_str()
                .unwrap()
                .contains("touch"),
            "{group}"
        );
    }
    let pin3 = &pin3["symbol"];
    let pin6 = &pin6["symbol"];
    check_placement(&path, pin3, (114.3, 116.84), 270.0, (116.84, 116.84));
    check_placement(&path, pin6, (114.3, 124.46), 0.0, (116.84, 124.46));
    assert_eq!(pin3["orientation"], "sideways", "{pin3}");
    assert_eq!(pin6["orientation"], "library", "{pin6}");
    assert_eq!(pin6["text_overlaps"], json!([]), "{pin6}");
    assert!(
        pin3["text_overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                ["a", "b"]
                    .into_iter()
                    .any(|side| finding[side]["text"] == "SIG_B")
            }),
        "{pin3}"
    );
}

#[tokio::test]
async fn a_negative_rail_uses_its_library_pin_angle_not_its_name() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let reply = place(&handler, &path, "-5V", "J3.1").await;
    check_placement(&path, &reply, (71.12, 109.22), 0.0, (68.58, 109.22));
    assert_eq!(reply["orientation"], "library", "{reply}");
}

#[tokio::test]
async fn a_vertical_pin_against_the_library_direction_inverts_at_the_stub_end() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let reply = place(&handler, &path, "GND", "R2.1").await;
    check_placement(&path, &reply, (119.38, 57.15), 180.0, (119.38, 59.69));
    assert_eq!(reply["orientation"], "inverted", "{reply}");
    assert!(
        reply["orientation_reason"]
            .as_str()
            .unwrap()
            .contains("against"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_new_stub_to_the_same_rail_is_idempotent() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let first = place(&handler, &path, "+3V3", "R1.1").await;
    check_placement(&path, &first, (101.6, 57.15), 0.0, (101.6, 59.69));
    let before = std::fs::read(&path).unwrap();
    let again = place(&handler, &path, "+3V3", "R1.1").await;
    assert_eq!(
        again,
        json!({
            "is_error": false, "already_connected": true, "power_net": "+3V3",
            "existing_reference": first["reference"], "x": 101.6, "y": 59.69,
        })
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

async fn blocked_stub(tool: &str, args: Value) {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let junction = serve(
        &handler,
        "add_junction",
        &path,
        json!({"x": 71.12, "y": 124.46}),
    )
    .await;
    assert_eq!(junction["is_error"], false, "{junction}");
    let before = std::fs::read(&path).unwrap();
    let reply = serve(&handler, tool, &path, args).await;
    assert_eq!(reply["is_error"], true, "{reply}");
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert!(
        reply["error"]["reasons"].to_string().contains("junction"),
        "{reply}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn an_obstructed_single_stub_refuses_without_writing() {
    blocked_stub(
        "add_power_symbol",
        json!({"power_net": "GND", "reference": "J4", "pin_number": "1"}),
    )
    .await;
}

#[tokio::test]
async fn an_obstructed_lone_batch_stub_writes_no_part_of_the_batch() {
    blocked_stub(
        "batch_add_power_symbol",
        json!({"power_net": "GND", "pins": pins(&["J4.1"])}),
    )
    .await;
}

#[tokio::test]
async fn a_batch_reports_the_blocked_pin_and_commits_only_the_clear_stub() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let label = serve(
        &handler,
        "add_schematic_net_label",
        &path,
        json!({"net": "BLOCKED", "x": 71.12, "y": 124.46}),
    )
    .await;
    assert_eq!(label["is_error"], false, "{label}");
    let before = read_schematic(&path).unwrap().1;
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({"power_net": "GND", "pins": pins(&["R1.2", "J4.1"])}),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    assert_eq!(reply["placed_count"], 1, "{reply}");
    assert_eq!(reply["errors"].as_array().unwrap().len(), 1, "{reply}");
    let error = &reply["errors"][0];
    assert_eq!(error["reference"], "J4", "{reply}");
    assert_eq!(error["pin_number"], "1", "{reply}");
    assert_eq!(error["kind"], "plan_blocked", "{reply}");
    assert!(error["reasons"].to_string().contains("label"), "{reply}");
    let after = read_schematic(&path).unwrap().1;
    assert_eq!(
        after.find_all("wire").len(),
        before.find_all("wire").len() + 1
    );
    assert_eq!(
        after.find_all("symbol").len(),
        before.find_all("symbol").len() + 1
    );
    check_placement(
        &path,
        &reply["groups"][0]["symbol"],
        (101.6, 69.85),
        0.0,
        (101.6, 67.31),
    );
}

#[tokio::test]
async fn an_unjoined_batch_pin_uses_its_explicit_rotation() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "pins": [
                {"reference": "J4", "pin_number": "1", "rotation": 90}
            ],
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    let symbol = &reply["groups"][0]["symbol"];
    check_placement(&path, symbol, (71.12, 124.46), 90.0, (68.58, 124.46));
    assert!(symbol.get("orientation_reason").is_none(), "{symbol}");
}

#[tokio::test]
async fn a_joined_row_ignores_pin_rotation_overrides() {
    let handler = handler().await;
    for overridden in [&["J5.1", "J5.2"][..], &["J5.2"][..], &[][..]] {
        let (_dir, path) = fixture();
        let mut requested = pins(&["J5.1", "J5.2"]);
        if overridden.contains(&"J5.1") {
            requested[0]["rotation"] = json!(0);
        }
        if overridden.contains(&"J5.2") {
            requested[1]["rotation"] = json!(180);
        }
        let reply = serve(
            &handler,
            "batch_add_power_symbol",
            &path,
            json!({"power_net": "GND", "pins": requested}),
        )
        .await;
        assert_eq!(reply["is_error"], false, "{reply}");
        assert_eq!(reply["placed_count"], 1, "{reply}");
        let group = &reply["groups"][0];
        assert_eq!(group["joined"], true, "{reply}");
        assert_eq!(group["symbol"]["rotation"], 90.0, "{reply}");
        if overridden.is_empty() {
            assert!(group.get("ignored_rotations").is_none(), "{reply}");
        } else {
            assert_eq!(group["ignored_rotations"], pins(overridden), "{reply}");
        }
        assert_eq!(reply["wires_added_count"], 3, "{reply}");
    }
}

#[tokio::test]
async fn conflicting_unjoined_rotations_are_invalid_even_if_the_stub_is_blocked() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    obstacle_wire(&path, 69.85);
    let before = std::fs::read(&path).unwrap();
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "pins": [
                {"reference": "J4", "pin_number": "1", "rotation": 0},
                {"reference": "J4", "pin_number": "1", "rotation": 180},
            ]
        }),
    )
    .await;
    assert_eq!(reply["error"]["kind"], "invalid_argument", "{reply}");
    assert_eq!(reply["error"]["field"], "pins[1].rotation", "{reply}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn single_explicit_rotation_matches_on_tip_and_at_stub_end() {
    let handler = handler().await;
    for (reference, rotation, tip, end) in [
        ("J4", 270.0, (68.58, 124.46), (71.12, 124.46)),
        ("R3", 90.0, (137.16, 59.69), (137.16, 57.15)),
        ("J1", 0.0, (68.58, 63.5), (71.12, 63.5)),
    ] {
        for length in [0.0, 2.54] {
            let (_dir, path) = fixture();
            let reply = serve(
                &handler,
                "add_power_symbol",
                &path,
                json!({
                    "power_net": "+3V3", "reference": reference, "pin_number": "1",
                    "rotation": rotation, "stub_length": length,
                }),
            )
            .await;
            assert_eq!(reply["is_error"], false, "{reply}");
            assert_eq!(reply["rotation"], rotation, "{reply}");
            assert!(reply.get("orientation_reason").is_none(), "{reply}");
            if length == 0.0 {
                assert_eq!(
                    (reply["x"].as_f64(), reply["y"].as_f64()),
                    (Some(tip.0), Some(tip.1)),
                    "{reply}"
                );
                assert!(reply["wire"].is_null(), "{reply}");
            } else {
                check_placement(&path, &reply, end, rotation, tip);
            }
        }
    }
}

#[tokio::test]
async fn single_stub_lengths_reject_invalid_values_without_writing() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    for args in [
        json!({"reference": "J4", "pin_number": "1", "stub_length": 1.0}),
        json!({"x": 220.98, "y": 180.34, "stub_length": 2.54}),
    ] {
        let mut args = args;
        args["power_net"] = json!("GND");
        let reply = serve(&handler, "add_power_symbol", &path, args).await;
        assert_eq!(reply["error"]["kind"], "invalid_argument", "{reply}");
        assert_eq!(reply["error"]["field"], "stub_length", "{reply}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let reply = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "x": 220.98, "y": 180.34, "stub_length": 0
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    assert_eq!(reply["x"], 220.98, "{reply}");
    assert_eq!(reply["y"], 180.34, "{reply}");
}

async fn assert_power_length_refused(tool: &str, mut args: Value, reason: &str) {
    let handler = handler().await;
    let ctx = ToolContext::new(
        crate::tools::ServerConfig::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    );
    let (_dir, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    args["power_net"] = json!("GND");
    let served = serve(&handler, tool, &path, args.clone()).await;
    assert_eq!(served["is_error"], true, "{served}");
    assert_eq!(served["error"]["kind"], "invalid_argument", "{served}");
    assert_eq!(served["error"]["field"], "stub_length", "{served}");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // Dispatch already rejects these values. Call the handler without the
    // schema as well so this test reaches the handler's own range check.
    args["schematic"] = json!(path);
    let result = match tool {
        "add_power_symbol" => handle_add_power_symbol(&args, &ctx).await.unwrap(),
        "batch_add_power_symbol" => handle_batch_add_power_symbol(&args, &ctx).await.unwrap(),
        _ => unreachable!("not a power-symbol tool"),
    };
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected a structured invalid_argument reply");
    };
    let reply: Value = serde_json::from_str(text).unwrap();
    assert!(result.is_error, "{tool}: {reply}");
    assert_eq!(reply["error"]["kind"], "invalid_argument", "{tool}: {reply}");
    assert_eq!(reply["error"]["field"], "stub_length", "{tool}: {reply}");
    assert_eq!(reply["error"]["reason"], reason, "{tool}: {reply}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn a_negative_grid_aligned_power_stub_is_refused_without_writing() {
    assert_power_length_refused(
        "add_power_symbol",
        json!({"reference": "J4", "pin_number": "1", "stub_length": -2.54}),
        "must be a finite, nonnegative length in mm",
    )
    .await;
}

#[tokio::test]
async fn a_zero_length_batch_power_stub_is_refused_without_writing() {
    assert_power_length_refused(
        "batch_add_power_symbol",
        json!({"pins": pins(&["J5.1", "J5.2"]), "stub_length": 0}),
        "must be a finite length greater than zero",
    )
    .await;
}

fn fixture_with_missing_symbol() -> (tempfile::TempDir, PathBuf) {
    let (dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    sch.symbols.by_reference_mut("J3").unwrap().lib_id = "missing:Connector".into();
    sch.overwrite().unwrap();
    (dir, path)
}

#[tokio::test]
async fn an_unresolved_symbol_with_an_empty_reference_is_named_in_the_refusal() {
    let handler = handler().await;
    let (_dir, path) = fixture_with_missing_symbol();
    let source = std::fs::read_to_string(&path).unwrap();
    let before = source.replacen(
        "(property \"Reference\" \"J3\"",
        "(property \"Reference\" \"\"",
        1,
    );
    assert_ne!(source, before);
    std::fs::write(&path, &before).unwrap();
    let reply = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({"power_net": "+3V3", "reference": "J4", "pin_number": "1"}),
    )
    .await;
    assert_eq!(reply["is_error"], true, "{reply}");
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert!(
        reply["error"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("unresolved pin geometry for <unknown symbol>"),
        "{reply}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before.as_bytes());
}

#[tokio::test]
async fn an_unreadable_pin_transform_reports_the_placed_symbol_reference() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let content = std::fs::read_to_string(&path).unwrap().replacen(
        "(at 63.5 109.22 0)",
        "(at bad-coordinate 109.22 0)",
        1,
    );
    std::fs::write(&path, &content).unwrap();
    for (tool, args) in [
        (
            "add_power_symbol",
            json!({"power_net": "+3V3", "reference": "J3", "pin_number": "1"}),
        ),
        (
            "batch_add_power_symbol",
            json!({"power_net": "+3V3", "pins": pins(&["J3.1", "J4.1"])}),
        ),
    ] {
        let reply = serve(&handler, tool, &path, args).await;
        assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
        assert!(
            reply["error"]["reasons"][0]
                .as_str()
                .unwrap()
                .contains("unresolved pin geometry for J3"),
            "{reply}"
        );
        if tool == "batch_add_power_symbol" {
            assert_eq!(reply["errors"].as_array().unwrap().len(), 2, "{reply}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }
}

#[tokio::test]
async fn missing_pin_geometry_blocks_single_stub_but_not_the_explicit_tip_placement() {
    let handler = handler().await;
    let (_dir, path) = fixture_with_missing_symbol();
    let before = std::fs::read(&path).unwrap();
    let args = json!({"power_net": "+3V3", "reference": "J4", "pin_number": "1"});
    let blocked = serve(&handler, "add_power_symbol", &path, args.clone()).await;
    assert_eq!(blocked["is_error"], true, "{blocked}");
    assert_eq!(blocked["error"]["kind"], "plan_blocked", "{blocked}");
    let reason = blocked["error"]["reasons"][0].as_str().unwrap();
    assert!(
        reason.contains("unresolved pin geometry for J3"),
        "{blocked}"
    );
    assert!(reason.contains("stub_length: 0"), "{blocked}");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let unresolved = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({"power_net": "+3V3", "reference": "J3", "pin_number": "1"}),
    )
    .await;
    assert_eq!(unresolved["error"]["kind"], "plan_blocked", "{unresolved}");
    let reason = unresolved["error"]["reasons"][0].as_str().unwrap();
    assert!(
        reason.contains("unresolved pin geometry for J3"),
        "{unresolved}"
    );
    assert!(reason.contains("stub_length: 0"), "{unresolved}");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let mut args = args;
    args["stub_length"] = json!(0);
    let tip = serve(&handler, "add_power_symbol", &path, args).await;
    assert_eq!(tip["is_error"], false, "{tip}");
    assert_eq!(tip["wire"], Value::Null, "{tip}");
    assert_eq!(
        (
            tip["x"].as_f64(),
            tip["y"].as_f64(),
            tip["rotation"].as_f64()
        ),
        (Some(68.58), Some(124.46), Some(270.0)),
        "{tip}"
    );
    let tree = read_schematic(&path).unwrap().1;
    let original = parse_sexp(std::str::from_utf8(&before).unwrap()).unwrap();
    assert_eq!(tree.find_all("wire"), original.find_all("wire"));
}

#[tokio::test]
async fn missing_pin_geometry_blocks_each_batch_pin_without_writing() {
    let handler = handler().await;
    let (_dir, path) = fixture_with_missing_symbol();
    let before = std::fs::read(&path).unwrap();
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "+3V3", "pins": pins(&["J4.1", "J5.1", "J5.2", "J3.1"]),
        }),
    )
    .await;
    assert_eq!(reply["is_error"], true, "{reply}");
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert_eq!(reply["groups"], json!([]), "{reply}");
    let errors = reply["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 4, "{reply}");
    for (reference, pin) in [("J4", "1"), ("J5", "1"), ("J5", "2"), ("J3", "1")] {
        let error = errors
            .iter()
            .find(|e| e["reference"] == reference && e["pin_number"] == pin)
            .unwrap();
        assert_eq!(error["kind"], "plan_blocked", "{reply}");
        assert!(
            error["reasons"][0]
                .as_str()
                .unwrap()
                .contains("unresolved pin geometry for J3"),
            "{reply}"
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn known_pin_inventory_allows_a_stub_with_unmeasured_drawing_bounds() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    let part = sch.symbols.by_reference_mut("J3").unwrap();
    part.lib_id = "test:Graphic".into();
    part.raw_sub_nodes.retain(|node| node.tag() != Some("pin"));
    let libraries = sch
        .raw_other
        .iter_mut()
        .find(|node| node.tag() == Some("lib_symbols"))
        .unwrap();
    let cse::sexp::SexpNode::List(libraries) = libraries else {
        unreachable!()
    };
    // The pin has a measurable extent. It must not hide the unmeasured
    // library text, which is not an ordinary instance field.
    libraries.push(
        cse::sexp::parser::parse(
            r#"(symbol "test:Graphic" (symbol "Graphic_0_1"
             (text "MARK" (at 0 0 0) (effects (font (size 1 1))))
             (pin passive line (at -5.08 0 0) (length 2.54)
               (name "~") (number "1"))))"#,
        )
        .unwrap(),
    );
    sch.overwrite().unwrap();

    let reply = place(&handler, &path, "+3V3", "J4.1").await;
    check_placement(&path, &reply, (71.12, 124.46), 0.0, (68.58, 124.46));
    assert_eq!(reply["bounds_unresolved"], json!(["J3"]), "{reply}");
    assert_eq!(reply["text_overlaps"], json!([]), "{reply}");
}

#[tokio::test]
async fn an_inactive_body_style_pin_cannot_receive_a_stub() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    add_inactive_connector_pin(&path, -5.08);
    let before = std::fs::read(&path).unwrap();
    for (tool, selector) in [
        (
            "add_power_symbol",
            json!({"power_net": "+3V3", "reference": "J4", "pin_number": "2"}),
        ),
        (
            "batch_add_power_symbol",
            json!({"power_net": "+3V3", "pins": pins(&["J4.2"])}),
        ),
    ] {
        let refusal = serve(&handler, tool, &path, selector).await;
        assert_eq!(
            refusal["error"]["kind"], "stale_target",
            "{tool}: {refusal}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{tool} wrote");
    }
}

fn add_inactive_connector_pin(path: &Path, local_x: f64) {
    let mut sch = cse::Schematic::load(path).unwrap();
    let libs = sch
        .raw_other
        .iter_mut()
        .find(|node| node.tag() == Some("lib_symbols"))
        .unwrap();
    let cse::sexp::SexpNode::List(libs) = libs else {
        unreachable!()
    };
    let connector = libs
        .iter_mut()
        .find(|node| {
            node.tag() == Some("symbol")
                && node.scalar_args().first() == Some(&"Connector_Generic:Conn_01x01")
        })
        .unwrap();
    let cse::sexp::SexpNode::List(connector) = connector else {
        unreachable!()
    };
    connector.push(
        cse::sexp::parser::parse(&format!(
            r#"(symbol "Conn_01x01_1_2" (pin passive line (at {local_x} 0 0) (length 2.54)
              (name "~") (number "2")))"#,
        ))
        .unwrap(),
    );
    sch.overwrite().unwrap();
}

#[tokio::test]
async fn an_inactive_pin_does_not_create_a_stub_junction_or_block_placement() {
    let handler = handler().await;
    for (tool, selector) in [
        (
            "add_power_symbol",
            json!({"power_net": "+3V3", "reference": "J4", "pin_number": "1"}),
        ),
        (
            "batch_add_power_symbol",
            json!({"power_net": "+3V3", "pins": pins(&["J4.1"])}),
        ),
    ] {
        let (_dir, path) = fixture();
        add_inactive_connector_pin(&path, -6.35);
        let reply = serve(&handler, tool, &path, selector).await;
        assert_eq!(reply["is_error"], false, "{tool}: {reply}");
        assert_eq!(reply["junctions_added"], json!([]), "{tool}: {reply}");
        let tree = read_schematic(&path).unwrap().1;
        assert!(!copperpour_sexp::schematic::extract_junctions(&tree).contains(&(69.85, 124.46)));
    }
}

fn obstacle_wire(path: &Path, x: f64) {
    // The served wire tool would add a junction at the pin we need undotted.
    let mut sch = cse::Schematic::load(path).unwrap();
    sch.add_wire(x, 120.65, x, 128.27);
    sch.overwrite().unwrap();
    let tree = read_schematic(path).unwrap().1;
    assert!(!copperpour_sexp::schematic::extract_junctions(&tree).contains(&(x, 124.46)));
}

#[tokio::test]
async fn an_undotted_wire_through_the_requested_pin_is_not_joined() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    obstacle_wire(&path, 68.58);
    let label = serve(
        &handler,
        "add_schematic_net_label",
        &path,
        json!({
            "net": "SIG", "x": 68.58, "y": 128.27
        }),
    )
    .await;
    assert_eq!(label["is_error"], false, "{label}");
    let before = std::fs::read(&path).unwrap();
    for (tool, args) in [
        (
            "add_power_symbol",
            json!({"reference": "J4", "pin_number": "1", "power_net": "GND"}),
        ),
        (
            "batch_add_power_symbol",
            json!({"pins": pins(&["J4.1"]), "power_net": "GND"}),
        ),
    ] {
        let reply = serve(&handler, tool, &path, args).await;
        assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
        assert!(
            reply["error"]["reasons"].to_string().contains("wire"),
            "{reply}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let batch = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "pins": pins(&["J4.1", "R1.2"]), "power_net": "GND",
        }),
    )
    .await;
    assert_eq!(batch["is_error"], false, "{batch}");
    assert_eq!(batch["placed_count"], 1, "{batch}");
    assert_eq!(batch["errors"][0]["reference"], "J4", "{batch}");
    assert_eq!(batch["errors"][0]["kind"], "plan_blocked", "{batch}");
    let tree = read_schematic(&path).unwrap().1;
    assert!(!copperpour_sexp::schematic::extract_junctions(&tree).contains(&(68.58, 124.46)));
}

#[tokio::test]
async fn a_stub_cannot_cross_an_unconnected_wire_midspan() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    obstacle_wire(&path, 69.85);
    let before = std::fs::read(&path).unwrap();
    let reply = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({
            "reference": "J4", "pin_number": "1", "power_net": "GND",
        }),
    )
    .await;
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert!(
        reply["error"]["reasons"]
            .to_string()
            .contains("(69.85, 124.46)"),
        "{reply}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn a_wire_crossing_only_the_upright_value_causes_sideways_fallback() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let reply = serve(
        &handler,
        "add_wire",
        &path,
        json!({
            "x1": 66.04, "y1": 120.65, "x2": 76.2, "y2": 120.65,
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    let power = place(&handler, &path, "+3V3", "J4.1").await;
    check_placement(&path, &power, (71.12, 124.46), 270.0, (68.58, 124.46));
    assert!(
        power["orientation_reason"]
            .as_str()
            .unwrap()
            .contains("wire_label_overlap"),
        "{power}"
    );
}

#[tokio::test]
async fn a_label_placed_first_is_reported_when_the_new_stub_crosses_its_text() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let label = serve(
        &handler,
        "add_schematic_net_label",
        &path,
        json!({
            "net": "S", "x": 69.85, "y": 124.9, "rotation": 90,
        }),
    )
    .await;
    assert_eq!(label["is_error"], false, "{label}");
    let power = place(&handler, &path, "+3V3", "J4.1").await;
    assert_eq!(power["rotation"], 0.0, "{power}");
    assert!(
        power["text_overlaps"].as_array().unwrap().iter().any(|f| {
            f["type"] == "wire_label_overlap"
                && f["b"]["net"] == "S"
                && f["a"]["uuid"] == power["wire"]["uuid"]
        }),
        "{power}"
    );
}

#[tokio::test]
async fn a_diagonal_wire_crossing_a_stub_refuses_without_writing() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    sch.add_wire(67.31, 121.92, 72.39, 127.0);
    sch.overwrite().unwrap();
    let before = std::fs::read(&path).unwrap();
    let reply = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "reference": "J4", "pin_number": "1",
        }),
    )
    .await;
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert!(
        reply["error"]["reasons"]
            .to_string()
            .contains("(69.85, 124.46)"),
        "{reply}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn a_diagonal_wire_crossing_the_exact_stub_end_refuses_without_writing() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    sch.add_wire(73.66, 123.19, 68.58, 125.73);
    sch.overwrite().unwrap();
    let before = std::fs::read(&path).unwrap();
    let reply = serve(
        &handler,
        "add_power_symbol",
        &path,
        json!({
            "reference": "J4", "pin_number": "1", "power_net": "GND",
        }),
    )
    .await;
    assert_eq!(reply["error"]["kind"], "plan_blocked", "{reply}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn a_nearly_aligned_middle_pin_gets_its_own_stub() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    let neighbor = sch.symbols.by_reference_mut("J4").unwrap();
    neighbor.at.x = 63.505;
    neighbor.at.y = 64.77;
    sch.overwrite().unwrap();
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "pins": pins(&["J1.1", "J4.1", "J1.2"])
        }),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    assert_eq!(reply["placed_count"], 3, "{reply}");
    assert_eq!(reply["wires_added_count"], 3, "{reply}");
    let groups = reply["groups"].as_array().unwrap();
    assert!(
        groups.iter().all(|group| group["joined"] == false),
        "{reply}"
    );
    let separate = groups
        .iter()
        .find(|group| group["pins"] == pins(&["J4.1"]))
        .unwrap();
    assert_eq!(separate["symbol"]["wire"]["x1"], 68.585, "{reply}");
}

#[tokio::test]
async fn a_lone_batch_pin_sees_the_joined_rail_in_either_order() {
    let handler = handler().await;
    for order in [["J4.1", "J7.1", "J8.1"], ["J7.1", "J8.1", "J4.1"]] {
        let (_dir, path) = fixture();
        for (reference, y) in [("J7", 120.65), ("J8", 127.0)] {
            let placed = serve(
                &handler,
                "add_schematic_component",
                &path,
                json!({
                    "lib_id": "Connector_Generic:Conn_01x01", "reference": reference,
                    "x": 59.69, "y": y, "mirror": "y",
                }),
            )
            .await;
            assert_eq!(placed["is_error"], false, "{placed}");
        }
        let mut sch = cse::Schematic::load(&path).unwrap();
        for reference in ["J7", "J8"] {
            for field in &mut sch.symbols.by_reference_mut(reference).unwrap().properties {
                field
                    .sub_nodes
                    .push(cse::sexp::parser::parse("(hide yes)").unwrap());
            }
        }
        sch.overwrite().unwrap();
        let reply = serve(
            &handler,
            "batch_add_power_symbol",
            &path,
            json!({
                "power_net": "+3V3", "pins": pins(&order)
            }),
        )
        .await;
        assert_eq!(reply["is_error"], false, "{reply}");
        let groups = reply["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{reply}");
        assert_eq!(groups[0]["pins"][0]["reference"], &order[0][..2], "{reply}");
        let lone = groups
            .iter()
            .find(|group| group["joined"] == false)
            .unwrap();
        assert_eq!(lone["pins"][0]["reference"], "J4", "{reply}");
        assert_eq!(lone["symbol"]["orientation"], "sideways", "{reply}");
        assert!(
            lone["symbol"]["orientation_reason"]
                .as_str()
                .unwrap()
                .contains("text_overlap"),
            "{reply}"
        );
    }
}

#[tokio::test]
async fn a_nearby_but_distinct_label_blocks_only_its_batch_stub() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let label = serve(
        &handler,
        "add_schematic_net_label",
        &path,
        json!({
            "net": "SIG", "x": 68.585, "y": 124.46,
        }),
    )
    .await;
    assert_eq!(label["is_error"], false, "{label}");
    let reply = serve(
        &handler,
        "batch_add_power_symbol",
        &path,
        json!({
            "power_net": "GND", "pins": pins(&["J4.1", "R1.2"]),
        }),
    )
    .await;
    assert_eq!(reply["placed_count"], 1, "{reply}");
    assert_eq!(reply["errors"][0]["reference"], "J4", "{reply}");
    assert_eq!(reply["errors"][0]["kind"], "plan_blocked", "{reply}");
    assert!(
        reply["errors"][0]["reasons"].to_string().contains("label"),
        "{reply}"
    );
    check_placement(
        &path,
        &reply["groups"][0]["symbol"],
        (101.6, 69.85),
        0.0,
        (101.6, 67.31),
    );
}

#[tokio::test]
async fn a_lone_stub_cannot_preempt_an_eligible_bar_in_either_order() {
    let handler = handler().await;
    for order in [["J7.1", "J5.1", "J5.2"], ["J5.1", "J5.2", "J7.1"]] {
        let (_dir, path) = fixture();
        let neighbour = serve(
            &handler,
            "add_schematic_component",
            &path,
            json!({
                "lib_id": "Connector_Generic:Conn_01x01", "reference": "J7",
                "x": 71.12, "y": 130.81, "rotation": 90,
            }),
        )
        .await;
        assert_eq!(neighbour["is_error"], false, "{neighbour}");
        let batch = serve(
            &handler,
            "batch_add_power_symbol",
            &path,
            json!({
                "power_net": "GND", "pins": pins(&order),
            }),
        )
        .await;
        assert_eq!(batch["is_error"], false, "{batch}");
        assert_eq!(batch["placed_count"], 1, "{batch}");
        assert_eq!(batch["groups"][0]["joined"], true, "{batch}");
        assert_eq!(batch["errors"][0]["reference"], "J7", "{batch}");
    }
}

#[tokio::test]
async fn an_aligned_rail_still_reports_a_neighbouring_corridor() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let neighbour = serve(
        &handler,
        "add_schematic_component",
        &path,
        json!({
            "lib_id": "Connector_Generic:Conn_01x01", "reference": "J7",
            "x": 93.98, "y": 55.88, "mirror": "y",
        }),
    )
    .await;
    assert_eq!(neighbour["is_error"], false, "{neighbour}");
    let reply = place(&handler, &path, "+3V3", "R1.1").await;
    check_placement(&path, &reply, (101.6, 57.15), 0.0, (101.6, 59.69));
    assert_eq!(reply["orientation"], "library", "{reply}");
    assert!(
        reply["orientation_reason"]
            .as_str()
            .unwrap()
            .contains("J7 pin 1"),
        "{reply}"
    );
    assert!(
        reply["text_overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| { f["type"] == "pin_stub_corridor_overlap" && f["b"]["reference"] == "J7" }),
        "{reply}"
    );
}

#[tokio::test]
async fn a_pin_with_a_perpendicular_wire_still_reserves_its_free_outward_corridor() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let wire = serve(
        &handler,
        "add_wire",
        &path,
        json!({
            "x1": 68.58, "y1": 86.36, "x2": 68.58, "y2": 87.63,
        }),
    )
    .await;
    assert_eq!(wire["is_error"], false, "{wire}");
    let reply = place(&handler, &path, "+3V3", "J2.1").await;
    assert_eq!(reply["orientation"], "sideways", "{reply}");
    assert!(
        reply["orientation_reason"]
            .as_str()
            .unwrap()
            .contains("stub corridor"),
        "{reply}"
    );
}

#[tokio::test]
async fn planned_lone_stubs_do_not_hide_their_corridors_during_batch_orientation() {
    let handler = handler().await;
    for order in [["J2.1", "J2.2"], ["J2.2", "J2.1"]] {
        let (_dir, path) = fixture();
        let reply = serve(
            &handler,
            "batch_add_power_symbol",
            &path,
            json!({
                "power_net": "+3V3", "pins": pins(&order), "max_gap": 0,
            }),
        )
        .await;
        assert_eq!(reply["is_error"], false, "{reply}");
        let groups = reply["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{reply}");
        let lower = groups
            .iter()
            .find(|group| group["pins"][0]["pin_number"] == "1")
            .unwrap();
        assert_eq!(lower["symbol"]["orientation"], "sideways", "{reply}");
        assert!(
            lower["symbol"]["orientation_reason"]
                .as_str()
                .unwrap()
                .contains("stub corridor"),
            "{reply}"
        );
    }
}

#[tokio::test]
async fn an_unused_pin_marked_no_connect_reserves_no_stub_corridor() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let neighbour = serve(
        &handler,
        "add_schematic_component",
        &path,
        json!({
            "lib_id": "Connector_Generic:Conn_01x01", "reference": "J7",
            "x": 93.98, "y": 55.88, "mirror": "y",
        }),
    )
    .await;
    assert_eq!(neighbour["is_error"], false, "{neighbour}");
    let nc = serve(
        &handler,
        "add_no_connect",
        &path,
        json!({"x": 99.06, "y": 55.88}),
    )
    .await;
    assert_eq!(nc["is_error"], false, "{nc}");
    let reply = place(&handler, &path, "+3V3", "R1.1").await;
    check_placement(&path, &reply, (101.6, 57.15), 0.0, (101.6, 59.69));
    assert!(reply.get("orientation_reason").is_none(), "{reply}");
    assert!(
        reply["text_overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["type"] != "pin_stub_corridor_overlap"),
        "{reply}"
    );
}

async fn fixture_with_wire_exit(handler: &McpHandler) -> (tempfile::TempDir, PathBuf) {
    let (dir, path) = fixture();
    let wire = serve(
        handler,
        "add_wire",
        &path,
        json!({
            "x1": 68.58, "y1": 124.46, "x2": 68.58, "y2": 127.0,
        }),
    )
    .await;
    assert_eq!(wire["is_error"], false, "{wire}");
    (dir, path)
}

#[tokio::test]
async fn a_stub_adds_a_junction_at_an_existing_wire_exit() {
    let handler = handler().await;
    let (_dir, path) = fixture_with_wire_exit(&handler).await;
    let reply = place(&handler, &path, "+3V3", "J4.1").await;
    check_placement(&path, &reply, (71.12, 124.46), 0.0, (68.58, 124.46));
    let tree = read_schematic(&path).unwrap().1;
    assert!(copperpour_sexp::schematic::extract_junctions(&tree).contains(&(68.58, 124.46)));
}

#[tokio::test]
async fn a_missing_saved_power_stub_junction_reports_an_uncertain_mutation() {
    let handler = handler().await;
    let (_dir, path) = fixture_with_wire_exit(&handler).await;
    let (_, before) = read_schematic(&path).unwrap();
    let counts = (before.find_all("symbol").len(), before.find_all("wire").len());
    save_before_power_readback(&path, move |path| {
        remove_one_new_saved_item(path, "junction", &before, |dot| {
            parse_at(dot).is_some_and(|(x, y, _)| (x, y) == (68.58, 124.46))
        });
    });
    let tool = "add_power_symbol";
    let reply = serve(
        &handler,
        tool,
        &path,
        json!({"power_net": "+3V3", "reference": "J4", "pin_number": "1"}),
    )
    .await;
    let (_, after) = read_schematic(&path).unwrap();
    assert_eq!(after.find_all("symbol").len(), counts.0 + 1);
    assert_eq!(after.find_all("wire").len(), counts.1 + 1);
    assert!(!copperpour_sexp::schematic::extract_junctions(&after).contains(&(68.58, 124.46)));
    assert_eq!(reply["is_error"], true, "{reply}");
    assert_eq!(reply["error"]["kind"], "mutation_outcome_uncertain", "{reply}");
    assert_eq!(reply["error"]["operation"], tool, "{reply}");
    assert_eq!(reply["error"]["path"], path.display().to_string(), "{reply}");
    assert_eq!(
        reply["error"]["reason"],
        "a new junction was not found in the saved file",
        "{reply}"
    );
}

#[tokio::test]
async fn a_missing_saved_power_bar_wire_reports_an_uncertain_mutation() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let (_, before) = read_schematic(&path).unwrap();
    let counts = (before.find_all("symbol").len(), before.find_all("wire").len());
    save_before_power_readback(&path, move |path| {
        // Remove the vertical bar only. Both horizontal stubs, the symbol
        // and the planned junction must survive the simulated save.
        remove_one_new_saved_item(path, "wire", &before, |wire| {
            let xy = wire.find("pts").unwrap().find_all("xy");
            xy[0].get_f64(1).unwrap() == xy[1].get_f64(1).unwrap()
        });
    });
    let tool = "batch_add_power_symbol";
    let reply = serve(
        &handler,
        tool,
        &path,
        json!({"power_net": "GND", "pins": pins(&["J5.1", "J5.2"])}),
    )
    .await;
    let (_, after) = read_schematic(&path).unwrap();
    assert_eq!(after.find_all("symbol").len(), counts.0 + 1);
    assert_eq!(after.find_all("wire").len(), counts.1 + 2);
    assert!(copperpour_sexp::schematic::extract_junctions(&after).contains(&(71.12, 137.16)));
    assert_eq!(reply["is_error"], true, "{reply}");
    assert_eq!(reply["error"]["kind"], "mutation_outcome_uncertain", "{reply}");
    assert_eq!(reply["error"]["operation"], tool, "{reply}");
    assert_eq!(reply["error"]["path"], path.display().to_string(), "{reply}");
    let reason = reply["error"]["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("wire ") && reason.ends_with(" is not in the saved file"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_clear_off_grid_pin_gets_a_stub_without_snapping() {
    let handler = handler().await;
    let (_dir, path) = fixture();
    let mut sch = cse::Schematic::load(&path).unwrap();
    let part = sch.symbols.by_reference_mut("R1").unwrap();
    part.at.x += 0.2;
    part.at.y += 0.3;
    sch.overwrite().unwrap();
    let reply = place(&handler, &path, "+3V3", "R1.1").await;
    check_placement(&path, &reply, (101.8, 57.45), 0.0, (101.8, 59.99));
}

#[tokio::test]
async fn a_lone_batch_pin_checks_later_joined_wires() {
    let handler = handler().await;
    for order in [["J4.1", "J7.1", "J8.1"], ["J7.1", "J8.1", "J4.1"]] {
        let (_dir, path) = fixture();
        for (reference, x) in [("J7", 66.04), ("J8", 76.2)] {
            let reply = serve(
                &handler,
                "add_schematic_component",
                &path,
                json!({
                    "lib_id": "Connector_Generic:Conn_01x01", "reference": reference,
                    "x": x, "y": 115.57, "rotation": 90,
                }),
            )
            .await;
            assert_eq!(reply["is_error"], false, "{reply}");
        }
        let mut sch = cse::Schematic::load(&path).unwrap();
        for reference in ["J7", "J8"] {
            for field in &mut sch.symbols.by_reference_mut(reference).unwrap().properties {
                field.set_hidden(true);
            }
        }
        sch.overwrite().unwrap();
        let reply = serve(
            &handler,
            "batch_add_power_symbol",
            &path,
            json!({
                "power_net": "+3V3", "pins": pins(&order),
            }),
        )
        .await;
        assert_eq!(reply["is_error"], false, "{reply}");
        let groups = reply["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{reply}");
        assert!(groups.iter().any(|g| g["joined"] == true), "{reply}");
        let lone = groups.iter().find(|g| g["joined"] == false).unwrap();
        check_placement(
            &path,
            &lone["symbol"],
            (71.12, 124.46),
            270.0,
            (68.58, 124.46),
        );
        assert!(
            lone["symbol"]["orientation_reason"]
                .as_str()
                .unwrap()
                .contains("wire_overlap"),
            "{reply}"
        );
    }
}
