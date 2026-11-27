//! Shared fixtures and IPC mocks for schematic-to-board sync tests.

mod copper;
mod footprints;
mod live_board;
mod netlist;
mod planning;
mod responses;

use super::footprints::{
    additions_missing_pads, apply_footprint_fields, apply_symbol_fields, build_added_footprint,
    net_ties_not_carried, prepare_additions, prepare_footprint_source, prepare_replacements,
    read_replacement, set_field_text, sorted_difference, update_footprint_item,
    UnpreparedFootprint,
};
use super::live_board::{
    board_footprint_from_instance, board_symbol_path, field_text, footprint_shapes,
    not_carried_by_kiid, zone_net, NotCarried, ZoneNet,
};
use super::netlist::{apply_saved_symbol_flags, normalize_symbol_path, parse_exported_netlist};
use super::plan::{
    conflict, footprint_conflict, leave_out_updates, plan_copper_nets, plan_revision, plan_sync,
    plan_sync_acknowledging, planned_deletions, refresh_revision_with_staging, refuse_plan,
    skip_parts,
};
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::library::PadFabricationProperty;
use crate::tools::library_footprint::CarriedByKiCad;
use crate::tools::ToolContext;
use prost::Message;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::*;

const ONE_RESISTOR: &str = r#"
(export
  (components
    (comp
      (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2")))))))
  (nets
    (net (code "1") (name "/Power/VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))))
"#;

/// Real `kicad-cli sch export netlist` output (KiCad 10.0.8): an LM339
/// with three of its units placed, and a keyed header whose pin 2, at the
/// key, has a no-connect flag. Provenance in
/// `tests/fixtures/pcb_sync_multi_unit_keyed_kicad10/README.md`.
const MULTI_UNIT_KEYED: &str =
    include_str!("../../../tests/fixtures/pcb_sync_multi_unit_keyed_kicad10/kk_sync.net");

fn exported<'a>(design: &'a ExportedDesign, reference: &str) -> &'a DesignComponent {
    design
        .components
        .iter()
        .find(|component| component.reference == reference)
        .unwrap_or_else(|| panic!("{reference} is exported"))
}

/// Real `kicad-cli sch export netlist` output: one sheet file placed
/// twice, as `CH1` and `CH2`, with a capacitor on it, and a capacitor on
/// the root. Provenance in `tests/fixtures/sheet_fields_kicad10.README.md`.
const SHEET_FIELDS: &str = include_str!("../../../tests/fixtures/sheet_fields_kicad10.net");

/// Real `kicad-cli sch export netlist` output: an LM358 placed as its
/// three units, and a resistor. Provenance in
/// `tests/fixtures/pcb_sync_symbol_units_kicad10.README.md`.
const SYMBOL_UNITS: &str =
    include_str!("../../../tests/fixtures/pcb_sync_symbol_units_kicad10.net");

fn unit(name: &str, pin_numbers: &[&str]) -> SymbolUnit {
    SymbolUnit {
        name: name.to_string(),
        pin_numbers: pin_numbers.iter().map(|pin| pin.to_string()).collect(),
    }
}

/// Real `kicad-cli sch export netlist` output: `R1` never had a footprint
/// assigned, so the export carries no `(footprint …)` node for it and
/// still nets its pin 1 to `C1`. Provenance in
/// `tests/fixtures/unassigned_footprint.README.md`.
const UNASSIGNED: &str = include_str!("../../../tests/fixtures/unassigned_footprint.net");

fn symbol_field(name: &str, value: &str) -> SymbolField {
    SymbolField {
        name: name.to_string(),
        value: value.to_string(),
    }
}

fn unassigned_design() -> ExportedDesign {
    parse_exported_netlist(UNASSIGNED).unwrap()
}

/// The real export with one `(comp …)` block copied under another
/// reference's name, placed *after* `after`. kicad-cli cannot produce a
/// duplicate reference, so the order-sensitive inputs are the real file
/// with one block duplicated by hand.
fn with_duplicate_of(copied: &str, renamed_to: &str, after: &str) -> String {
    let block = comp_block(UNASSIGNED, copied).replace(
        &format!("(ref \"{copied}\")"),
        &format!("(ref \"{renamed_to}\")"),
    );
    let anchor = comp_block(UNASSIGNED, after);
    let at = UNASSIGNED.find(anchor).unwrap() + anchor.len();
    format!("{}{}{}", &UNASSIGNED[..at], block, &UNASSIGNED[at..])
}

fn comp_block<'a>(source: &'a str, reference: &str) -> &'a str {
    let start = source.find(&format!("(ref \"{reference}\")")).unwrap();
    let start = source[..start].rfind("(comp").unwrap();
    let mut depth = 0usize;
    for (offset, ch) in source[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..start + offset + 1];
                }
            }
            _ => {}
        }
    }
    unreachable!("unbalanced comp block")
}

fn refuses_duplicate(source: &str) {
    let error = parse_exported_netlist(source)
        .expect_err("a duplicate reference must refuse before any plan")
        .to_string();
    assert!(error.contains("duplicate component reference"), "{error}");
}

fn resistor(reference: &str, symbol_path: &str) -> DesignComponent {
    DesignComponent {
        reference: reference.to_string(),
        value: "10k".to_string(),
        footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
        symbol_path: symbol_path.to_string(),
        other_unit_paths: Vec::new(),
        dnp: false,
        exclude_from_bom: false,
        pad_nets: BTreeMap::from([
            ("1".to_string(), "VCC".to_string()),
            ("2".to_string(), "GND".to_string()),
        ]),
        unconnected_pins: BTreeSet::new(),
        fields: Vec::new(),
        sheet: SymbolSheet::default(),
    }
}

fn board_resistor(reference: &str, symbol_path: Option<&str>) -> BoardFootprint {
    BoardFootprint {
        kiid: format!("{reference}-kiid"),
        reference: reference.to_string(),
        value: "10k".to_string(),
        footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
        symbol_path: symbol_path.map(str::to_string),
        pad_nets: BTreeMap::from([
            ("1".to_string(), "VCC".to_string()),
            ("2".to_string(), "GND".to_string()),
        ]),
        pad_numbers: BTreeSet::from(["1".to_string(), "2".to_string()]),
        position: Point { x: 1.0, y: 2.0 },
        rotation: 0.0,
        layer: "F.Cu".to_string(),
        locked: false,
        dnp: false,
        exclude_from_bom: false,
        not_in_schematic: false,
        fields: BTreeMap::new(),
        sheet: SymbolSheet::default(),
    }
}

fn board_with(footprints: Vec<BoardFootprint>) -> BoardState {
    BoardState {
        footprints,
        routed_nets: BTreeMap::new(),
        bounds: Bounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 10.0,
            max_y: 10.0,
        },
    }
}

/// R1 and R2 on the board, both to be updated (R1 with a pad moving to
/// another net), and R3 and R4 to be added.
fn two_updates_and_two_additions() -> (SyncPlan, BoardState) {
    let mut r1 = resistor("R1", "/r1");
    r1.value = "4k7".to_string();
    r1.pad_nets.insert("1".to_string(), "VDD".to_string());
    let mut r2 = resistor("R2", "/r2");
    r2.value = "1k".to_string();
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![r1, r2, resistor("R3", "/r3"), resistor("R4", "/r4")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let board = board_with(vec![
        board_resistor("R1", Some("/r1")),
        board_resistor("R2", Some("/r2")),
    ]);
    let plan = plan_sync("", &design, &board);
    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.counts.added.planned, 2);
    assert_eq!(plan.counts.updated.planned, 2);
    assert_eq!(plan.counts.pads_reassigned.planned, 1);
    (plan, board)
}

fn planned_references(plan: &SyncPlan) -> Vec<&str> {
    let mut references: Vec<&str> = plan
        .changes
        .iter()
        .map(|change| match change {
            PlannedChange::Add { reference, .. }
            | PlannedChange::Update { reference, .. }
            | PlannedChange::Delete { reference, .. } => reference.as_str(),
        })
        .collect();
    references.sort_unstable();
    references
}

fn cannot_build(code: &str, references: &[&str]) -> SyncDiagnostic {
    footprint_conflict(
        code,
        format!("{} cannot be built", references.join(", ")),
        "Resistor_SMD:R_0603_1608Metric",
        references
            .iter()
            .map(|reference| reference.to_string())
            .collect(),
    )
}

/// Build a footprint carrying one pad and one child of every graphic kind,
/// the way a real library footprint arrives from KiCad. The existing sync
/// test passes `&[]` for graphics, which is precisely why #244 survived it.
fn footprint_with_artwork(reference: &str) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let silk = || "F.SilkS".to_string();
    let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
        "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm",
        reference,
        "NE555",
        &[copperpour_ipc::IpcPadDefinition {
            number: "1".to_string(),
            pad_type: "smd".to_string(),
            shape: "rect".to_string(),
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            size_x: 1.0,
            size_y: 1.0,
            drill_x: None,
            drill_y: None,
            drill_oval: false,
            layers: vec!["F.Cu".to_string()],
            roundrect_ratio: 0.0,
            custom: None,
        }],
        &[
            copperpour_ipc::IpcGraphicDefinition::Line {
                start: (-2.0, -2.5),
                end: (2.0, -2.5),
                layer: silk(),
                width: 0.12,
                line_style: copperpour_ipc::IpcLineStyle::Solid,
            },
            copperpour_ipc::IpcGraphicDefinition::Rect {
                start: (-2.6, -3.0),
                end: (2.6, 3.0),
                layer: "F.CrtYd".to_string(),
                width: 0.05,
                line_style: copperpour_ipc::IpcLineStyle::Solid,
                filled: false,
            },
            copperpour_ipc::IpcGraphicDefinition::Circle {
                center: (-1.8, -1.8),
                end: (-1.6, -1.8),
                layer: silk(),
                width: 0.12,
                line_style: copperpour_ipc::IpcLineStyle::Solid,
                filled: true,
            },
            copperpour_ipc::IpcGraphicDefinition::Arc {
                start: (-1.0, -2.5),
                mid: (0.0, -2.0),
                end: (1.0, -2.5),
                layer: "F.Fab".to_string(),
                width: 0.1,
                line_style: copperpour_ipc::IpcLineStyle::Solid,
            },
            copperpour_ipc::IpcGraphicDefinition::Poly {
                points: vec![(-1.0, 2.0), (1.0, 2.0), (0.0, 2.8)],
                layer: "F.Fab".to_string(),
                width: 0.1,
                line_style: copperpour_ipc::IpcLineStyle::Solid,
                filled: true,
            },
            copperpour_ipc::IpcGraphicDefinition::Text {
                text: "U1".to_string(),
                position: (0.0, -3.5),
                rotation: 0.0,
                layer: silk(),
                size: 1.0,
                stroke_width_mm: 0.15,
                keep_upright: true,
                style: Default::default(),
            },
        ],
        &copperpour_ipc::IpcFieldPlacement::default(),
        25.0,
        30.0,
        0.0,
        "F.Cu",
    )
    .unwrap();
    // Give it the KIID the update path matches against.
    let mut footprint =
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
    footprint.id = Some(kiapi::common::types::Kiid {
        value: format!("{}-kiid", reference.to_lowercase()),
    });
    copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
}

/// Tally a footprint definition's children by the protobuf type they
/// declare — the property #244 destroyed.
fn child_types(item: &prost_types::Any) -> BTreeMap<String, usize> {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let footprint = kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
    let mut counts = BTreeMap::new();
    for child in &footprint.definition.as_ref().unwrap().items {
        *counts
            .entry(copperpour_ipc::builders::any_type_name(child).to_string())
            .or_insert(0) += 1;
    }
    counts
}

/// KiCad 10's own `R1`, as its IPC API sent it (`issue_474_r1.ipc.bin`):
/// Datasheet and Description empty, hidden on F.Fab, and no custom field.
fn captured_r1() -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
        include_bytes!("../../../tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
    )
    .expect("the checked-in KiCad IPC capture must decode")
}

/// A custom field as someone might have left it: shown on the silkscreen
/// beside the part, in 2 mm text, turned. Built by hand, so the style the
/// sync gives an added field cannot leak into it.
fn shown_custom_field(
    name: &str,
    value: &str,
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> copperpour_ipc::gen::kiapi::board::types::Field {
    use copperpour_ipc::gen::kiapi::{board::types as board, common::types as common};

    let origin = footprint.position.expect("a placed footprint");
    board::Field {
        id: None,
        name: name.to_string(),
        text: Some(board::BoardText {
            id: None,
            text: Some(common::Text {
                position: Some(common::Vector2 {
                    x_nm: origin.x_nm + 2_500_000,
                    y_nm: origin.y_nm - 1_000_000,
                }),
                attributes: Some(common::TextAttributes {
                    size: Some(common::Vector2 {
                        x_nm: 2_000_000,
                        y_nm: 2_000_000,
                    }),
                    stroke_width: Some(common::Distance { value_nm: 300_000 }),
                    angle: Some(common::Angle {
                        value_degrees: 90.0,
                    }),
                    line_spacing: 1.0,
                    keep_upright: true,
                    visible: true,
                    ..Default::default()
                }),
                text: value.to_string(),
                hyperlink: String::new(),
            }),
            layer: board::BoardLayer::BlFSilkS as i32,
            knockout: false,
            locked: common::LockedState::LsLocked as i32,
            parent: None,
        }),
        visible: true,
    }
}

fn with_field_item(
    mut footprint: copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    field: &copperpour_ipc::gen::kiapi::board::types::Field,
) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    footprint
        .definition
        .as_mut()
        .unwrap()
        .items
        .push(copperpour_ipc::builders::pack_any(
            field,
            "kiapi.board.types.Field",
        ));
    footprint
}

/// The `Field` items of a footprint's definition, by name.
fn field_items(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> BTreeMap<String, copperpour_ipc::gen::kiapi::board::types::Field> {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field"))
        .map(|child| {
            let field =
                copperpour_ipc::gen::kiapi::board::types::Field::decode(child.value.as_slice())
                    .unwrap();
            (field.name.clone(), field)
        })
        .collect()
}

/// What a field shows, everything but its text.
fn presentation(
    field: &copperpour_ipc::gen::kiapi::board::types::Field,
) -> (
    i32,
    bool,
    i32,
    Option<copperpour_ipc::gen::kiapi::common::types::Vector2>,
    Option<copperpour_ipc::gen::kiapi::common::types::TextAttributes>,
) {
    let board_text = field.text.as_ref().unwrap();
    let text = board_text.text.as_ref().unwrap();
    (
        board_text.layer,
        field.visible,
        board_text.locked,
        text.position,
        text.attributes.clone(),
    )
}

/// `live` after a planned update that writes `fields`.
fn updated_with(
    live: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    fields: Vec<SymbolField>,
) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    let board = board_footprint_from_instance(live).unwrap();
    let change = PlannedChange::Update {
        kiid: board.kiid.clone(),
        reference: board.reference.clone(),
        value: board.value.clone(),
        symbol_path: "/root/r1".to_string(),
        dnp: false,
        exclude_from_bom: false,
        pad_nets: board.pad_nets.clone(),
        unconnected_pins: BTreeSet::new(),
        preserve: PreservedBoardState {
            position: board.position,
            rotation: board.rotation,
            layer: board.layer.clone(),
            locked: board.locked,
        },
        fields,
        sheet: board.sheet.clone(),
        sheet_only: false,
        replace_footprint: None,
    };
    let item = copperpour_ipc::builders::pack_any(live, "kiapi.board.types.FootprintInstance");
    let updated = update_footprint_item(&item, &change, &BTreeMap::new()).unwrap();
    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(updated.value.as_slice())
        .unwrap()
}

/// `R1` with pad 1 on `VCC` and pad 2 on `GND`, both nets routed, and a
/// schematic that moves pad 1 to `VCC_A` and pad 2 to `GND_A`: two
/// `routed_pad_net_change` conflicts (#38).
fn routed_r1_moving_both_pads() -> (ExportedDesign, BoardState) {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![DesignComponent {
            pad_nets: BTreeMap::from([
                ("1".to_string(), "VCC_A".to_string()),
                ("2".to_string(), "GND_A".to_string()),
            ]),
            ..resistor("R1", "/sheet/existing")
        }],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let mut board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);
    board.routed_nets = BTreeMap::from([("VCC".to_string(), 1), ("GND".to_string(), 3)]);
    (design, board)
}

fn acknowledging(reference: &str, pad: &str, from: &str, to: &str) -> AcknowledgedNetChange {
    AcknowledgedNetChange {
        reference: reference.to_string(),
        pad: pad.to_string(),
        from: from.to_string(),
        to: to.to_string(),
    }
}

/// The codes of a plan's diagnostics that refuse it.
fn refusing_codes(plan: &SyncPlan) -> Vec<&str> {
    plan.diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code != "routed_pad_net_change_acknowledged")
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

/// R1 marked Exclude from board, with `symbol_path` as its identity.
fn r1_excluded() -> ExportedDesign {
    ExportedDesign {
        root_sheet_uuid: None,
        components: Vec::new(),
        skipped: vec![SkippedComponent {
            reference: "R1".to_string(),
            symbol_path: "/sheet/existing".to_string(),
        }],
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    }
}

fn r1_without_a_symbol(reason: WithoutSymbolReason, locked: bool) -> FootprintWithoutSymbol {
    FootprintWithoutSymbol {
        reference: "R1".to_string(),
        kiid: "R1-kiid".to_string(),
        footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
        locked,
        reason,
    }
}

/// A real `FootprintInstance`, exactly as KiCad 10.0.6's IPC layer sent it
/// for a mounting hole with no schematic symbol behind it.
///
/// Captured from a running editor rather than hand-built, because a
/// hand-built one encodes whatever the author assumed. This one already
/// corrected two such assumptions: KiCad reports `symbol_path` as
/// **present and empty** (not absent, and `path_human_readable` is `""`,
/// not `"/"`), and it leaves `not_in_schematic` **false** on a board-only
/// mounting hole — marking it `exclude_from_position_files` and
/// `exclude_from_bill_of_materials` instead. The first hand-written
/// fixture set that flag true, which made the rename branch below
/// unreachable from its own tests.
///
/// Regenerate with `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` on the ignored live
/// test `kicad_reports_an_empty_sheet_path_for_a_board_only_footprint`;
/// provenance is in the fixture's README.
const BOARD_ONLY_CAPTURE: &[u8] =
    include_bytes!("../../../tests/fixtures/board_only_footprint.ipc.bin");

/// The captured footprint, with only its identity fields varied.
///
/// A board carries several board-only graphics, and the planner keys on
/// reference and KIID, so tests need more than one. Everything else — the
/// empty `SheetPath`, the attributes, the pads and graphics — is the real
/// message.
fn board_only_instance(
    kiid: &str,
    reference: &str,
) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    use copperpour_ipc::gen::kiapi;

    let mut instance = kiapi::board::types::FootprintInstance::decode(BOARD_ONLY_CAPTURE)
        .expect("the checked-in KiCad IPC capture must decode");
    instance.id = Some(kiapi::common::types::Kiid {
        value: kiid.to_string(),
    });
    set_field_text(&mut instance.reference_field, "Reference", reference);
    instance
}

/// A real `FootprintInstance` for a board-only footprint KiCad left
/// carrying the library's own default reference, `REF**`.
///
/// The capture above is a mounting hole someone had annotated `MH1`, so it
/// cannot witness the case #452 actually reported: unannotated graphics,
/// every one of them called `REF**`, colliding with each other. This one
/// comes from `copperpour-ipc/tests/fixtures/board_only_shared_reference.kicad_pcb`,
/// a board KiCad wrote holding two such footprints — and `REF**` is the
/// library's own string, not one an editing session left behind.
///
/// Regenerate with `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` on the ignored live
/// test `kicad_reports_the_same_reference_for_two_board_only_footprints`;
/// provenance is in the fixture's README.
const SHARED_REFERENCE_CAPTURE: &[u8] =
    include_bytes!("../../../tests/fixtures/board_only_shared_reference.ipc.bin");

/// One of that board's two `REF**` footprints, with only its KIID varied.
///
/// The reference is deliberately *not* varied — it is the shared value the
/// tests are about, and it is what KiCad really sent. That the board
/// carries two of them is checked without KiCad by
/// `the_shared_reference_fixture_still_carries_a_duplicate_board_only_reference`,
/// so the second instance is a KIID away from the first rather than an
/// assumption.
fn ref_star_instance(kiid: &str) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    use copperpour_ipc::gen::kiapi;

    let mut instance = kiapi::board::types::FootprintInstance::decode(SHARED_REFERENCE_CAPTURE)
        .expect("the checked-in KiCad IPC capture must decode");
    instance.id = Some(kiapi::common::types::Kiid {
        value: kiid.to_string(),
    });
    instance
}

/// The same real message, re-labelled as the resistor `resistor()` exports.
///
/// Only the library id and value change: these tests are about the planner
/// branches an absent identity unlocks, and they need a footprint whose
/// `footprint_id` and `value` can match a schematic component. The
/// `not_in_schematic` flag is left exactly as KiCad set it — false — which
/// is what makes the rename branch reachable at all.
fn unlinked_instance(
    kiid: &str,
    reference: &str,
) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
    use copperpour_ipc::gen::kiapi;

    let mut instance = board_only_instance(kiid, reference);
    if let Some(definition) = instance.definition.as_mut() {
        definition.id = Some(kiapi::common::types::LibraryIdentifier {
            library_nickname: "Resistor_SMD".to_string(),
            entry_name: "R_0603_1608Metric".to_string(),
        });
    }
    set_field_text(&mut instance.value_field, "Value", "10k");
    instance
}

// The two branches below test `symbol_path.is_none()`, so before #452 they
// were unreachable for a board-only footprint: every one of them wore the
// synthetic identity `/`. Reading absence correctly wakes both, which
// changes plans on boards that never hit the duplicate-identity bug at all.
// They are pinned here so the consequence is a decision rather than a
// discovery. Note `board_resistor` leaves `not_in_schematic` false — a
// footprint KiCad has not flagged — which is what makes them reachable.

/// R1 linked to its symbol, R3 whose symbol left the schematic, and MH1,
/// which has no symbol and the board-only attribute (#861).
fn footprints_without_symbols() -> (ExportedDesign, BoardState) {
    let mut mounting_hole = board_resistor("MH1", None);
    mounting_hole.not_in_schematic = true;
    let board = board_with(vec![
        board_resistor("R1", Some("/sheet/existing")),
        board_resistor("R3", Some("/sheet/removed")),
        mounting_hole,
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    (design, board)
}

fn r3_without_a_symbol(locked: bool) -> FootprintWithoutSymbol {
    FootprintWithoutSymbol {
        reference: "R3".to_string(),
        kiid: "R3-kiid".to_string(),
        footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
        locked,
        reason: WithoutSymbolReason::SymbolRemoved,
    }
}

fn r3_deleted() -> PlannedChange {
    PlannedChange::Delete {
        kiid: "R3-kiid".to_string(),
        reference: "R3".to_string(),
        footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
        reason: WithoutSymbolReason::SymbolRemoved,
    }
}

/// A via or copper zone on the planner's board (#862).
fn copper(kind: CopperKind, kiid: &str, net: &str) -> CopperItem {
    CopperItem {
        kind,
        kiid: kiid.to_string(),
        net: net.to_string(),
        name: String::new(),
    }
}

/// R1 and R2 on the board, pad 1 on VCC and pad 2 on GND; the schematic
/// moves R1's pad 2 to `r1_to` and R2's to `r2_to`, and has no GND.
fn plan_moving_gnd(r1_to: &str, r2_to: &str) -> (SyncPlan, BoardState, BTreeSet<String>) {
    let board = board_with(vec![
        board_resistor("R1", Some("/r1")),
        board_resistor("R2", Some("/r2")),
    ]);
    let mut r1 = resistor("R1", "/r1");
    r1.pad_nets.insert("2".to_string(), r1_to.to_string());
    let mut r2 = resistor("R2", "/r2");
    r2.pad_nets.insert("2".to_string(), r2_to.to_string());
    let net_names = ["VCC", r1_to, r2_to]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![r1, r2],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: net_names.clone(),
    };
    let plan = plan_sync("netlist", &design, &board);
    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    (plan, board, net_names)
}

fn moved(kind: CopperKind, kiid: &str, from: &str, to: &str) -> CopperNetChange {
    CopperNetChange {
        kind,
        kiid: kiid.to_string(),
        name: String::new(),
        from: from.to_string(),
        to: to.to_string(),
    }
}

fn note_codes(plan: &SyncPlan) -> Vec<(&str, Option<&str>)> {
    plan.diagnostics
        .iter()
        .map(|note| (note.code.as_str(), note.kiid.as_deref()))
        .collect()
}

/// KiCad's own `R1` (`issue_474_r1.ipc.bin`: pad 1 on `VCC`, pad 2 on
/// `GND`), bound to the `/Power/` symbol that [`ONE_RESISTOR`] exports.
fn schematic_backed_resistor() -> prost_types::Any {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    const CAPTURE: &[u8] = include_bytes!("../../../tests/fixtures/issue_474_r1.ipc.bin");
    let mut footprint = kiapi::board::types::FootprintInstance::decode(CAPTURE)
        .expect("the checked-in KiCad IPC capture must decode");
    footprint.symbol_path = Some(kiapi::common::types::SheetPath {
        path: vec![
            kiapi::common::types::Kiid {
                value: "sheet-uuid".to_string(),
            },
            kiapi::common::types::Kiid {
                value: "symbol-uuid".to_string(),
            },
        ],
        path_human_readable: "/Power/".to_string(),
    });
    set_field_text(&mut footprint.value_field, "Value", "1k");
    copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
}

/// KiCad's captured `GND` copper pour and a rule area (`issue_474_ipc.README.md`).
const GND_POUR: &[u8] = include_bytes!("../../../tests/fixtures/issue_474_copper_zone_0.ipc.bin");
const RULE_AREA: &[u8] = include_bytes!("../../../tests/fixtures/issue_474_zone_0.ipc.bin");

/// A zone as KiCad lists it: `bytes` is the `Zone` message.
fn zone_item(bytes: &[u8]) -> prost_types::Any {
    prost_types::Any {
        type_url: "type.googleapis.com/kiapi.board.types.Zone".to_string(),
        value: bytes.to_vec(),
    }
}

/// The captured pour with its net taken out.
fn pour_without_a_net() -> prost_types::Any {
    use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Zone};
    use prost::Message;
    let mut zone = Zone::decode(GND_POUR).expect("the captured pour decodes");
    let Some(Settings::CopperSettings(copper)) = zone.settings.as_mut() else {
        panic!("the captured pour is a copper zone");
    };
    copper.net = None;
    copperpour_ipc::builders::pack_any(&zone, "kiapi.board.types.Zone")
}

/// The captured pour on `net` instead of `GND`.
fn pour_on(net: &str) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Zone};
    use prost::Message;
    let mut zone = Zone::decode(GND_POUR).expect("the captured pour decodes");
    let Some(Settings::CopperSettings(copper)) = zone.settings.as_mut() else {
        panic!("the captured pour is a copper zone");
    };
    copper.net.as_mut().expect("the captured pour's net").name = net.to_string();
    copperpour_ipc::builders::pack_any(&zone, "kiapi.board.types.Zone")
}

// ─── #657: name what cannot be prepared, and let `ready` mean ready ──────

const TEXAS_VQFN: &str = "Package_DFN_QFN:Texas_RJE0020A_VQFN-20-1EP_3x3mm_P0.45mm_EP0.675x0.76mm";
const GENERIC_VQFN: &str = "Package_DFN_QFN:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";
const STOCK_0603: &str = "Capacitor_SMD:C_0603_1608Metric";
/// KiCad 10.0.8's stock DFN-14 whose eight thermal-via pads carry
/// `(property pad_prop_heatsink)` (`tests/fixtures/issue_283_ipc.README.md`).
const THERMAL_DFN: &str = "Package_DFN_QFN:DFN-14-1EP_3x4.5mm_P0.65mm_EP1.65x4.25mm_ThermalVias";

/// The stock generic VQFN-20 unchanged, in a library of its own: its
/// custom pads (circle anchors, outline clearance) are placed (#679).
const PLACEABLE_VQFN: &str = "Custom_Stock:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";

/// A project whose own `fp-lib-table` resolves stock KiCad 10 footprints:
/// the two with custom-shape pads that #657 met on a real board
/// (provenance in `tests/fixtures/custom_pads_kicad10.README.md`), a plain
/// 0603, the DFN-14 with heatsink pads, and the generic VQFN once more as
/// [`PLACEABLE_VQFN`]. Since custom pads are placed (#679), the two #657
/// footprints are written with `(clearance convexhull)`, which KiCad's pad
/// message cannot carry, so they stay unusable for a reason that holds. A
/// project table shadows the global one, so these files are the ones read
/// whether or not KiCad is installed.
fn project_with_stock_footprints() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("carrier.kicad_pcb");
    std::fs::write(
        &board,
        include_bytes!("../../../tests/fixtures/specctra_two_resistors.kicad_pcb"),
    )
    .unwrap();
    let stock: [(&str, &[u8]); 4] = [
        (
            TEXAS_VQFN,
            include_bytes!("../../../tests/fixtures/custom_pads_texas_rje0020a_kicad10.kicad_mod"),
        ),
        (
            GENERIC_VQFN,
            include_bytes!("../../../tests/fixtures/custom_pads_vqfn20_kicad10.kicad_mod"),
        ),
        (
            STOCK_0603,
            include_bytes!("../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"),
        ),
        (
            THERMAL_DFN,
            include_bytes!("../../../tests/fixtures/issue_283_dfn14_thermalvias_kicad10.kicad_mod"),
        ),
    ];
    let convexhull = |source: &[u8]| {
        let text = std::str::from_utf8(source).unwrap();
        assert!(text.contains("(clearance outline)"));
        text.replace("(clearance outline)", "(clearance convexhull)")
            .into_bytes()
    };
    let mut stock: Vec<(&str, Vec<u8>)> = stock
        .into_iter()
        .map(|(footprint_id, source)| {
            if footprint_id == TEXAS_VQFN || footprint_id == GENERIC_VQFN {
                (footprint_id, convexhull(source))
            } else {
                (footprint_id, source.to_vec())
            }
        })
        .collect();
    stock.push((
        PLACEABLE_VQFN,
        include_bytes!("../../../tests/fixtures/custom_pads_vqfn20_kicad10.kicad_mod").to_vec(),
    ));
    for (footprint_id, source) in stock {
        let (library, name) = footprint_id.split_once(':').unwrap();
        let directory = temp.path().join(format!("{library}.pretty"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("{name}.kicad_mod")), source).unwrap();
    }
    std::fs::write(
        temp.path().join("fp-lib-table"),
        "(fp_lib_table\n  (lib (name \"Package_DFN_QFN\") (type \"KiCad\") (uri \"${KIPRJMOD}/Package_DFN_QFN.pretty\") (options \"\") (descr \"\"))\n  (lib (name \"Capacitor_SMD\") (type \"KiCad\") (uri \"${KIPRJMOD}/Capacitor_SMD.pretty\") (options \"\") (descr \"\"))\n  (lib (name \"Custom_Stock\") (type \"KiCad\") (uri \"${KIPRJMOD}/Custom_Stock.pretty\") (options \"\") (descr \"\"))\n)\n",
    )
    .unwrap();
    (temp, board)
}

fn planned_add(reference: &str, footprint_id: &str, pads: &[&str]) -> PlannedChange {
    PlannedChange::Add {
        reference: reference.to_string(),
        value: "part".to_string(),
        footprint_id: footprint_id.to_string(),
        symbol_path: format!("/{reference}-uuid"),
        dnp: false,
        exclude_from_bom: false,
        pad_nets: pads
            .iter()
            .map(|pad| (pad.to_string(), format!("{reference}-{pad}")))
            .collect(),
        unconnected_pins: BTreeSet::new(),
        position: Point { x: 0.0, y: 0.0 },
        fields: Vec::new(),
        sheet: SymbolSheet::default(),
    }
}

fn plan_adding(changes: Vec<PlannedChange>) -> SyncPlan {
    SyncPlan {
        status: PlanStatus::Ready,
        plan_revision: "reviewed".to_string(),
        counts: SyncCounts {
            added: CountPair {
                planned: changes.len(),
                applied: 0,
            },
            ..SyncCounts::default()
        },
        changes,
        diagnostics: Vec::new(),
        unassigned: Vec::new(),
        skipped: Vec::new(),
        refused_updates: Vec::new(),
        without_symbols: Vec::new(),
        copper_net_changes: Vec::new(),
        staging: None,
        acknowledged_apply: None,
    }
}

/// One `(comp …)` per part and one net per pin, in the shape
/// `kicad-cli sch export netlist --format kicadsexpr` writes.
fn exported_netlist(components: &[(&str, &str, &[&str])]) -> String {
    let mut parts = String::new();
    let mut nets = String::new();
    let mut code = 0;
    for (reference, footprint_id, pins) in components {
        let pin_list = pins
            .iter()
            .map(|pin| format!("(pin (num \"{pin}\"))"))
            .collect::<String>();
        parts.push_str(&format!(
            "    (comp\n      (ref \"{reference}\")\n      (value \"part\")\n      (footprint \"{footprint_id}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      (tstamps \"{reference}-uuid\")\n      (units (unit (name \"A\") (pins {pin_list}))))\n"
        ));
        for pin in *pins {
            code += 1;
            nets.push_str(&format!(
                "    (net (code \"{code}\") (name \"{reference}-{pin}\") (class \"Default\")\n      (node (ref \"{reference}\") (pin \"{pin}\") (pintype \"passive\")))\n"
            ));
        }
    }
    format!("(export\n  (components\n{parts}  )\n  (nets\n{nets}  ))\n")
}

/// Everything a served dry run needs without KiCad: a stand-in
/// `kicad-cli` that hands back `netlist`, and a protobuf mock holding the
/// project's board open with nothing on it. It can also apply: the mock
/// keeps what `CreateItems` sent and lists it back as the board's
/// footprints, shaped by `readback`, and `UpdateItems` replaces the
/// footprint with the KIID sent, as KiCad does.
struct ServedSync {
    _temp: tempfile::TempDir,
    _kicad: crate::test_support::MockIpcServer,
    handler: crate::mcp::handler::McpHandler,
    schematic: PathBuf,
    board: PathBuf,
    exported: PathBuf,
    /// The report the stand-in `kicad-cli pcb drc` writes; it fails while
    /// no test has written one.
    drc_report: PathBuf,
    /// The zones the mock lists once a commit has ended, when set.
    zones_after_commit: std::sync::Arc<std::sync::Mutex<Option<Vec<prost_types::Any>>>>,
    created: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
    /// The held footprints as the mock's board now has them.
    held: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
    /// What `SaveDocumentToString` answers with.
    board_text: std::sync::Arc<std::sync::Mutex<String>>,
    /// The command each request carried, in order.
    commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// What `GetVersion` answers: `(major, minor, patch)`.
    kicad_version: std::sync::Arc<std::sync::Mutex<(u32, u32, u32)>>,
    /// When set, `DeleteItems` answers OK and deletes nothing.
    keeps_deleted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When set, `DeleteItems` reports each requested item, as KiCad
    /// 10.0.7+ does (KiCad `77e2d53da2`); unset, it answers as KiCad
    /// before 10.0.7, with no per-item results.
    reports_deleted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Listing the footprints after a commit fails from this listing on,
    /// counted from 0; `usize::MAX` (the default) never fails.
    fails_readback: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The vias the mock's board holds, as `GetItems` lists them (#862).
    vias: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
    /// The zones the mock's board holds, as `UpdateItems` left them.
    zones: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
    /// What `GetConnectedItems` answers, by the KIID asked about.
    connected: std::sync::Arc<std::sync::Mutex<BTreeMap<String, Vec<prost_types::Any>>>>,
    /// When set, `UpdateItems` answers OK and leaves vias and zones as
    /// they were.
    keeps_copper: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When set, listing the vias or zones fails once a commit has ended.
    fails_copper_readback: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A listed via's or zone's KIID, decoded as its own type (#862).
fn copper_kiid(item: &prost_types::Any) -> String {
    use copperpour_ipc::gen::kiapi::board::types::{Via, Zone};
    let id = if copperpour_ipc::builders::any_is(item, "kiapi.board.types.Via") {
        Via::decode(item.value.as_slice())
            .ok()
            .and_then(|via| via.id)
    } else {
        Zone::decode(item.value.as_slice())
            .ok()
            .and_then(|zone| zone.id)
    };
    id.map(|id| id.value).unwrap_or_default()
}

/// A listed footprint's KIID.
fn footprint_kiid(item: &prost_types::Any) -> String {
    use prost::Message;
    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
        .ok()
        .and_then(|footprint| footprint.id)
        .map(|id| id.value)
        .unwrap_or_default()
}

/// A held board table in `ServedSync`: its KIID and bounding box `(x, y, width, height)` in mm.
type HeldTable = (&'static str, (f64, f64, f64, f64));

/// What the mock's board holds before the sync touches it.
#[derive(Clone, Default)]
struct HeldBoard {
    footprints: Vec<prost_types::Any>,
    zones: Vec<prost_types::Any>,
    nets: Vec<&'static str>,
}

/// What the mock's board holds after `CreateItems`.
#[derive(Clone, Copy)]
enum Readback {
    /// Refuse creation before there is any new footprint to read back.
    RefuseCreation,
    /// Exactly what was sent, as a KiCad that kept everything does.
    AsSent,
    /// What was sent without the instance attributes and 3D models: the
    /// board the sync built before #789.
    WithoutLibraryData,
    /// What was sent with its mounting style and models, but DNP cleared:
    /// one flag lost while everything the old check looked at survives.
    WithoutDnp,
    /// What was sent without its custom fields, and with Value moved back
    /// to the hidden silkscreen field the builder used to give it.
    WithoutLibraryFields,
    /// What was sent with every pad's fabrication property cleared, as
    /// KiCad before 10.0.7 would read it.
    WithoutPadProperties,
    /// What was sent, with each pad sent without a fabrication property
    /// reported as `PFP_NONE`, as KiCad 10.0.7+ reports it.
    UnknownPadPropertiesAsNone,
    /// What was sent without its sheet name, sheet file and footprint
    /// filters: the board the sync built before #237.
    WithoutSheet,
    /// What was sent without its symbol units, as KiCad before 10.0.8
    /// reads it (#682).
    WithoutSymbolUnits,
}

impl ServedSync {
    async fn new() -> Self {
        Self::holding_outline(None).await
    }

    /// As [`Self::new`], with KiCad also holding one board graphic whose
    /// box is `outline`, `(x, y, width, height)` in mm.
    async fn holding_outline(outline: Option<(f64, f64, f64, f64)>) -> Self {
        Self::build(
            outline,
            Vec::new(),
            Vec::new(),
            Readback::AsSent,
            HeldBoard::default(),
        )
        .await
    }

    /// As [`Self::holding_outline`], with KiCad refusing to list each class
    /// in `refused`, as KiCad 10.0.5 refuses tables and generators.
    async fn refusing(
        outline: Option<(f64, f64, f64, f64)>,
        refused: Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType>,
    ) -> Self {
        Self::build(
            outline,
            Vec::new(),
            refused,
            Readback::AsSent,
            HeldBoard::default(),
        )
        .await
    }

    /// As [`Self::new`], with the board read back as `readback` says.
    async fn reading_back(readback: Readback) -> Self {
        Self::build(None, Vec::new(), Vec::new(), readback, HeldBoard::default()).await
    }

    /// As [`Self::new`], with KiCad already holding `held`.
    async fn holding(held: HeldBoard) -> Self {
        Self::build(None, Vec::new(), Vec::new(), Readback::AsSent, held).await
    }

    async fn build(
        outline: Option<(f64, f64, f64, f64)>,
        held_tables: Vec<HeldTable>,
        refused: Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType>,
        readback: Readback,
        held: HeldBoard,
    ) -> Self {
        use crate::tools::cli::test_support::write_script;
        use copperpour_ipc::gen::kiapi;

        let created = std::sync::Arc::new(std::sync::Mutex::new(Vec::<prost_types::Any>::new()));
        let board_items = created.clone();
        let held_kiids = held
            .footprints
            .iter()
            .map(footprint_kiid)
            .chain(held.zones.iter().map(|item| {
                kiapi::board::types::Zone::decode(item.value.as_slice())
                    .ok()
                    .and_then(|zone| zone.id)
                    .map(|id| id.value)
                    .unwrap_or_default()
            }))
            .collect::<Vec<_>>();
        let held_footprints = std::sync::Arc::new(std::sync::Mutex::new(held.footprints));
        let board_held = held_footprints.clone();
        let zones_held = std::sync::Arc::new(std::sync::Mutex::new(held.zones));
        let held_zones = zones_held.clone();
        let zones_updated = zones_held.clone();
        let vias_held = std::sync::Arc::new(std::sync::Mutex::new(Vec::<prost_types::Any>::new()));
        let held_vias = vias_held.clone();
        let vias_updated = vias_held.clone();
        let vias_measured = vias_held.clone();
        let connected = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::<
            String,
            Vec<prost_types::Any>,
        >::new()));
        let connected_items = connected.clone();
        let keeps_copper = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ignores_copper = keeps_copper.clone();
        let fails_copper_readback = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let copper_readback_fails = fails_copper_readback.clone();
        let zones_after_commit =
            std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<prost_types::Any>>));
        let after_commit = zones_after_commit.clone();
        let committed = std::sync::atomic::AtomicBool::new(false);
        let held_nets = held.nets;
        let board_text = std::sync::Arc::new(std::sync::Mutex::new(
            String::from_utf8_lossy(include_bytes!(
                "../../../tests/fixtures/specctra_two_resistors.kicad_pcb"
            ))
            .into_owned(),
        ));
        let text = board_text.clone();
        let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = commands.clone();
        let kicad_version = std::sync::Arc::new(std::sync::Mutex::new((10, 0, 8)));
        let version = kicad_version.clone();
        let keeps_deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ignores_deletes = keeps_deleted.clone();
        let reports_deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reports_each_deletion = reports_deleted.clone();
        let fails_readback = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let readback_fails = fails_readback.clone();
        let listed_after_commit = std::sync::atomic::AtomicUsize::new(0);

        let (temp, board) = project_with_stock_footprints();
        let schematic = temp.path().join("carrier.kicad_sch");
        std::fs::write(
            &schematic,
            include_bytes!("../../../tests/fixtures/structural_scans_kicad10.kicad_sch"),
        )
        .unwrap();
        let exported = temp.path().join("carrier.net");
        let unix_source = exported.to_string_lossy().replace('\'', "'\\''");
        let windows_source = exported.to_string_lossy();
        let drc_report = temp.path().join("drc-report.json");
        let unix_drc = drc_report.to_string_lossy().replace('\'', "'\\''");
        let windows_drc = drc_report.to_string_lossy();
        let cli = write_script(
            temp.path(),
            "fake-kicad-cli-657",
            &format!(
                "#!/bin/sh\nsource='{unix_source}'\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"drc\" ]; then\n    source='{unix_drc}'\n  fi\n  if [ \"$1\" = \"--output\" ]; then\n    shift\n    cp \"$source\" \"$1\"\n    exit $?\n  fi\n  shift\ndone\nexit 2\n"
            ),
            &format!(
                "@echo off\r\nset \"source={windows_source}\"\r\n:loop\r\nif \"%~1\"==\"\" exit /b 2\r\nif \"%~1\"==\"drc\" set \"source={windows_drc}\"\r\nif \"%~1\"==\"--output\" goto found\r\nshift\r\ngoto loop\r\n:found\r\nshift\r\ncopy /Y \"%source%\" \"%~1\" >nul\r\nexit /b %ERRORLEVEL%\r\n"
            ),
        );
        let kicad = crate::tools::pcb_board::board_mock::spawn_kicad_holding_board_refusing(
            &board,
            refused,
            move |command| {
                let name = command.type_url.rsplit('.').next().unwrap_or_default();
                seen.lock().unwrap().push(name.to_string());
                if command.type_url.ends_with("GetVersion") {
                    let (major, minor, patch) = *version.lock().unwrap();
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetVersionResponse {
                            version: Some(kiapi::common::types::KiCadVersion {
                                major,
                                minor,
                                patch,
                                full_version: format!("{major}.{minor}.{patch}"),
                            }),
                        },
                        "kiapi.common.commands.GetVersionResponse",
                    ));
                }
                if command.type_url.ends_with("GetItems") {
                    let request =
                        kiapi::common::commands::GetItems::decode(command.value.as_slice())
                            .expect("GetItems request");
                    let shapes = kiapi::common::types::KiCadObjectType::KotPcbShape as i32;
                    let footprints = kiapi::common::types::KiCadObjectType::KotPcbFootprint as i32;
                    let zones = kiapi::common::types::KiCadObjectType::KotPcbZone as i32;
                    let vias = kiapi::common::types::KiCadObjectType::KotPcbVia as i32;
                    let tables = kiapi::common::types::KiCadObjectType::KotPcbTable as i32;
                    let items = match outline {
                        Some(_) if request.types.contains(&shapes) => {
                            vec![crate::tools::pcb_board::board_mock::listed_item(
                                kiapi::common::types::KiCadObjectType::KotPcbShape,
                                "outline",
                            )]
                        }
                        _ if request.types.contains(&footprints)
                            && committed.load(std::sync::atomic::Ordering::SeqCst)
                            && listed_after_commit
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                >= readback_fails.load(std::sync::atomic::Ordering::SeqCst) =>
                        {
                            return Some(copperpour_ipc::builders::pack_any(
                                &kiapi::common::commands::GetItemsResponse {
                                    header: None,
                                    status:
                                        kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound
                                            as i32,
                                    items: Vec::new(),
                                },
                                "kiapi.common.commands.GetItemsResponse",
                            ));
                        }
                        _ if request.types.contains(&footprints) => board_held
                            .lock()
                            .unwrap()
                            .iter()
                            .cloned()
                            .chain(
                                board_items
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .map(|item| read_back(item, readback)),
                            )
                            .collect(),
                        _ if (request.types.contains(&zones) || request.types.contains(&vias))
                            && copper_readback_fails.load(std::sync::atomic::Ordering::SeqCst)
                            && committed.load(std::sync::atomic::Ordering::SeqCst) =>
                        {
                            return Some(copperpour_ipc::builders::pack_any(
                                &kiapi::common::commands::GetItemsResponse {
                                    header: None,
                                    status:
                                        kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound
                                            as i32,
                                    items: Vec::new(),
                                },
                                "kiapi.common.commands.GetItemsResponse",
                            ));
                        }
                        _ if request.types.contains(&zones) => {
                            match &*after_commit.lock().unwrap() {
                                Some(after)
                                    if committed.load(std::sync::atomic::Ordering::SeqCst) =>
                                {
                                    after.clone()
                                }
                                _ => held_zones.lock().unwrap().clone(),
                            }
                        }
                        _ if request.types.contains(&vias) => held_vias.lock().unwrap().clone(),
                        _ if request.types.contains(&tables) => held_tables
                            .iter()
                            .map(|(kiid, _)| {
                                crate::tools::pcb_board::board_mock::listed_item(
                                    kiapi::common::types::KiCadObjectType::KotPcbTable,
                                    kiid,
                                )
                            })
                            .collect(),
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
                            nets: held_nets
                                .iter()
                                .zip(1..)
                                .map(|(name, code)| kiapi::board::types::Net {
                                    code: Some(kiapi::board::types::NetCode { value: code }),
                                    name: name.to_string(),
                                })
                                .collect(),
                        },
                        "kiapi.board.commands.NetsResponse",
                    ));
                }
                if command.type_url.ends_with("GetBoardEnabledLayers") {
                    // A two-layer board, as `specctra_two_resistors` is.
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::board::commands::BoardEnabledLayersResponse {
                            copper_layer_count: 2,
                            layers: vec![
                                kiapi::board::types::BoardLayer::BlFCu as i32,
                                kiapi::board::types::BoardLayer::BlBCu as i32,
                                kiapi::board::types::BoardLayer::BlEdgeCuts as i32,
                            ],
                        },
                        "kiapi.board.commands.BoardEnabledLayersResponse",
                    ));
                }
                if command.type_url.ends_with("GetBoundingBox") {
                    return Some(crate::tools::pcb_board::board_mock::kicad_bounding_boxes(
                        command,
                        |kiid| {
                            if kiid == "outline" {
                                return outline.expect("an outline to measure");
                            }
                            if let Some((_, bounds)) =
                                held_tables.iter().find(|(id, _)| *id == kiid)
                            {
                                return *bounds;
                            }
                            let held_via = vias_measured
                                .lock()
                                .unwrap()
                                .iter()
                                .any(|via| copper_kiid(via) == kiid);
                            assert!(
                                held_via || held_kiids.iter().any(|held| held == kiid),
                                "only the outline and held items are listed, not {kiid}"
                            );
                            (0.0, 0.0, 10.0, 10.0)
                        },
                    ));
                }
                if command.type_url.ends_with("SaveDocumentToString") {
                    // The board as KiCad holds it: the saved file, unless a
                    // test holds another.
                    let request = kiapi::common::commands::SaveDocumentToString::decode(
                        command.value.as_slice(),
                    )
                    .expect("SaveDocumentToString request");
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::SavedDocumentResponse {
                            document: request.document,
                            contents: text.lock().unwrap().clone(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ));
                }
                if command.type_url.ends_with("BeginCommit") {
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::BeginCommitResponse {
                            id: Some(kiapi::common::types::Kiid {
                                value: "served-sync-commit".into(),
                            }),
                        },
                        "kiapi.common.commands.BeginCommitResponse",
                    ));
                }
                if command.type_url.ends_with("CreateItems") {
                    use kiapi::common::commands as cmd;
                    let request = cmd::CreateItems::decode(command.value.as_slice())
                        .expect("CreateItems request");
                    let refused = matches!(readback, Readback::RefuseCreation);
                    if !refused {
                        board_items
                            .lock()
                            .unwrap()
                            .extend(request.items.iter().cloned());
                    }
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::CreateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            created_items: request
                                .items
                                .into_iter()
                                .map(|item| kiapi::common::commands::ItemCreationResult {
                                    status: Some(kiapi::common::commands::ItemStatus {
                                        code: if refused {
                                            cmd::ItemStatusCode::IscInvalidData
                                        } else {
                                            cmd::ItemStatusCode::IscOk
                                        } as i32,
                                        error_message: if refused {
                                            "footprint refused".into()
                                        } else {
                                            String::new()
                                        },
                                    }),
                                    item: (!refused).then_some(item),
                                })
                                .collect(),
                        },
                        "kiapi.common.commands.CreateItemsResponse",
                    ));
                }
                if command.type_url.ends_with("GetConnectedItems") {
                    let request =
                        kiapi::board::commands::GetConnectedItems::decode(command.value.as_slice())
                            .expect("GetConnectedItems request");
                    let connected = connected_items.lock().unwrap();
                    let items = request
                        .items
                        .iter()
                        .flat_map(|id| connected.get(&id.value).cloned().unwrap_or_default())
                        .collect();
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items,
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ));
                }
                if command.type_url.ends_with("UpdateItems") {
                    let request =
                        kiapi::common::commands::UpdateItems::decode(command.value.as_slice())
                            .expect("UpdateItems request");
                    if !ignores_copper.load(std::sync::atomic::Ordering::SeqCst) {
                        for updated in &request.items {
                            let copper = if copperpour_ipc::builders::any_is(
                                updated,
                                "kiapi.board.types.Zone",
                            ) {
                                &zones_updated
                            } else if copperpour_ipc::builders::any_is(
                                updated,
                                "kiapi.board.types.Via",
                            ) {
                                &vias_updated
                            } else {
                                continue;
                            };
                            for item in copper.lock().unwrap().iter_mut() {
                                if copper_kiid(item) == copper_kiid(updated) {
                                    *item = updated.clone();
                                }
                            }
                        }
                    }
                    let mut held = board_held.lock().unwrap();
                    for updated in &request.items {
                        for footprint in held.iter_mut() {
                            if footprint_kiid(footprint) == footprint_kiid(updated) {
                                *footprint = updated.clone();
                            }
                        }
                    }
                    drop(held);
                    return Some(copperpour_ipc::builders::pack_any(
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
                    ));
                }
                if command.type_url.ends_with("DeleteItems") {
                    // OK, with no per-item results as KiCad before 10.0.7
                    // answers, or one per item as 10.0.7+ does.
                    use kiapi::common::commands::{ItemDeletionResult, ItemDeletionStatus};
                    let request =
                        kiapi::common::commands::DeleteItems::decode(command.value.as_slice())
                            .expect("DeleteItems request");
                    let held = board_held
                        .lock()
                        .unwrap()
                        .iter()
                        .map(footprint_kiid)
                        .collect::<BTreeSet<_>>();
                    if !ignores_deletes.load(std::sync::atomic::Ordering::SeqCst) {
                        board_held.lock().unwrap().retain(|footprint| {
                            !request
                                .item_ids
                                .iter()
                                .any(|id| id.value == footprint_kiid(footprint))
                        });
                    }
                    let deleted_items =
                        if reports_each_deletion.load(std::sync::atomic::Ordering::SeqCst) {
                            request
                                .item_ids
                                .iter()
                                .map(|id| ItemDeletionResult {
                                    id: Some(id.clone()),
                                    status: if held.contains(&id.value) {
                                        ItemDeletionStatus::IdsOk
                                    } else {
                                        ItemDeletionStatus::IdsNonexistent
                                    } as i32,
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::DeleteItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            deleted_items,
                        },
                        "kiapi.common.commands.DeleteItemsResponse",
                    ));
                }
                if command.type_url.ends_with("EndCommit") {
                    if matches!(readback, Readback::RefuseCreation) {
                        let request =
                            kiapi::common::commands::EndCommit::decode(command.value.as_slice())
                                .unwrap();
                        assert_eq!(
                            request.action(),
                            kiapi::common::commands::CommitAction::CmaDrop
                        );
                    }
                    committed.store(true, std::sync::atomic::Ordering::SeqCst);
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    ));
                }
                None
            },
        );
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            kicad_cli: cli.to_string_lossy().to_string(),
            kicad_binary: String::new(),
            ipc_address: kicad.address().to_string(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        Self {
            _temp: temp,
            _kicad: kicad,
            handler,
            schematic,
            board,
            exported,
            drc_report,
            zones_after_commit,
            created,
            held: held_footprints,
            board_text,
            commands,
            kicad_version,
            keeps_deleted,
            reports_deleted,
            fails_readback,
            vias: vias_held,
            zones: zones_held,
            connected,
            keeps_copper,
            fails_copper_readback,
        }
    }

    /// Have `GetVersion` answer `major.minor.patch` from now on.
    fn report_kicad_version(&self, major: u32, minor: u32, patch: u32) {
        *self.kicad_version.lock().unwrap() = (major, minor, patch);
    }

    /// Have `SaveDocumentToString` answer with `text` from now on.
    fn hold_board_text(&self, text: &str) {
        *self.board_text.lock().unwrap() = text.to_string();
    }

    /// Have the stand-in `kicad-cli pcb drc` write `report` from now on.
    fn hold_drc_report(&self, report: &str) {
        std::fs::write(&self.drc_report, report).unwrap();
    }

    /// Have the mock list `zones` once a commit has ended.
    fn list_zones_after_commit(&self, zones: Vec<prost_types::Any>) {
        *self.zones_after_commit.lock().unwrap() = Some(zones);
    }

    /// How many requests carried `command`.
    fn sent(&self, command: &str) -> usize {
        let commands = self.commands.lock().unwrap();
        commands.iter().filter(|name| *name == command).count()
    }

    /// One `tools/call`, with the result's `isError` beside the body.
    async fn call(&self, arguments: serde_json::Value) -> (bool, serde_json::Value) {
        let (is_error, text) = self.call_text(arguments).await;
        (is_error, serde_json::from_str(&text).unwrap())
    }

    /// As [`Self::call`], with the body as the text the client receives.
    async fn call_text(&self, arguments: serde_json::Value) -> (bool, String) {
        let response = self
            .handler
            .handle_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 789,
                "method": "tools/call",
                "params": { "name": "update_pcb_from_schematic", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("successful JSON-RPC response");
        (
            result["isError"] == serde_json::json!(true),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    /// A sync of `netlist` through `tools/call` with `extra` beside the
    /// paths, as the text the client receives.
    async fn sync_text(&self, netlist: &str, extra: serde_json::Value) -> (bool, String) {
        std::fs::write(&self.exported, netlist).unwrap();
        let mut arguments = serde_json::json!({
            "schematic": self.schematic.to_string_lossy(),
            "board": self.board.to_string_lossy(),
        });
        let serde_json::Value::Object(extra) = extra else {
            panic!("extra arguments are an object")
        };
        arguments.as_object_mut().unwrap().extend(extra);
        self.call_text(arguments).await
    }

    /// As [`Self::sync_text`], parsed.
    async fn sync(&self, netlist: &str, extra: serde_json::Value) -> (bool, serde_json::Value) {
        let (is_error, text) = self.sync_text(netlist, extra).await;
        (is_error, serde_json::from_str(&text).unwrap())
    }

    /// A dry run and then the apply of its plan, both through
    /// `tools/call`. Returns the apply's body.
    async fn apply(&self, netlist: &str) -> serde_json::Value {
        let plan = self.dry_run(netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let (is_error, applied) = self
            .call(serde_json::json!({
                "schematic": self.schematic.to_string_lossy(),
                "board": self.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }))
            .await;
        assert!(!is_error, "{applied:#}");
        applied
    }

    /// The footprints `CreateItems` received.
    fn sent_footprints(&self) -> Vec<copperpour_ipc::gen::kiapi::board::types::FootprintInstance> {
        self.created
            .lock()
            .unwrap()
            .iter()
            .map(|item| {
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint")
            })
            .collect()
    }

    /// A dry run through `tools/call`, for a schematic exporting `netlist`.
    async fn dry_run(&self, netlist: &str) -> serde_json::Value {
        self.dry_run_result(netlist).await.1
    }

    /// As [`Self::dry_run`], with the result's `isError` beside the body.
    async fn dry_run_result(&self, netlist: &str) -> (bool, serde_json::Value) {
        std::fs::write(&self.exported, netlist).unwrap();
        let response = self
            .handler
            .handle_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 657,
                "method": "tools/call",
                "params": {
                    "name": "update_pcb_from_schematic",
                    "arguments": {
                        "schematic": self.schematic.to_string_lossy(),
                        "board": self.board.to_string_lossy()
                    }
                }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("successful JSON-RPC response");
        (
            result["isError"] == serde_json::json!(true),
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        )
    }
}

/// A created item as the mock's board lists it back.
fn read_back(item: &prost_types::Any, readback: Readback) -> prost_types::Any {
    match readback {
        Readback::AsSent | Readback::RefuseCreation => item.clone(),
        Readback::WithoutLibraryData => {
            let mut footprint =
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint");
            footprint.attributes = None;
            if let Some(definition) = footprint.definition.as_mut() {
                definition.items.retain(|child| {
                    !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Footprint3DModel")
                });
            }
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
        Readback::WithoutPadProperties | Readback::UnknownPadPropertiesAsNone => {
            use copperpour_ipc::gen::kiapi::board::types::{
                FootprintInstance, Pad, PadFabricationProperty,
            };
            let mut footprint =
                FootprintInstance::decode(item.value.as_slice()).expect("a footprint");
            for child in &mut footprint.definition.as_mut().unwrap().items {
                if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
                    let mut pad = Pad::decode(child.value.as_slice()).unwrap();
                    pad.fab_property = match readback {
                        Readback::WithoutPadProperties => 0,
                        _ if pad.fab_property == 0 => PadFabricationProperty::PfpNone as i32,
                        _ => pad.fab_property,
                    };
                    *child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
                }
            }
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
        Readback::WithoutDnp => {
            let mut footprint =
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint");
            if let Some(attributes) = footprint.attributes.as_mut() {
                attributes.do_not_populate = false;
            }
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
        Readback::WithoutSheet => {
            let mut footprint =
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint");
            footprint.symbol_sheet_name.clear();
            footprint.symbol_sheet_filename.clear();
            footprint.symbol_footprint_filters.clear();
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
        Readback::WithoutSymbolUnits => {
            let mut footprint =
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint");
            footprint.symbol_units.clear();
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
        Readback::WithoutLibraryFields => {
            use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
            let mut footprint =
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .expect("a footprint");
            if let Some(value) = footprint.value_field.as_mut() {
                value.visible = false;
                if let Some(text) = value.text.as_mut() {
                    text.layer = BoardLayer::BlFSilkS as i32;
                }
            }
            if let Some(attributes) = footprint
                .reference_field
                .as_mut()
                .and_then(|field| field.text.as_mut())
                .and_then(|text| text.text.as_mut())
                .and_then(|text| text.attributes.as_mut())
            {
                attributes.keep_upright = false;
            }
            if let Some(definition) = footprint.definition.as_mut() {
                definition.items.retain(|child| {
                    !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field")
                });
            }
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        }
    }
}

/// `netlist` with `reference` marked DNP the way `kicad-cli` exports it.
fn with_dnp(netlist: &str, reference: &str) -> String {
    // Anchored on the component entry: the net nodes name the reference too.
    let anchor = format!("(comp\n      (ref \"{reference}\")");
    assert_eq!(
        netlist.matches(&anchor).count(),
        1,
        "{reference} is exported once"
    );
    netlist.replace(
        &anchor,
        &format!("{anchor}\n      (property (name \"dnp\"))"),
    )
}

/// Served dry run of `R1` ([`schematic_backed_resistor`]) on a board
/// that also holds `zones`, after the schematic moves its pad on `net`
/// to the copper-less `NEW_NET`.
async fn plan_moving_off(net: &str, zones: Vec<prost_types::Any>) -> serde_json::Value {
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones,
        nets: vec!["VCC", "GND"],
    })
    .await;
    served.dry_run(&netlist_moving_off(net)).await
}

/// [`ONE_RESISTOR`] for the captured `R1`, with its pad on `net` moved to
/// the copper-less `NEW_NET`.
fn netlist_moving_off(net: &str) -> String {
    ONE_RESISTOR
        .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
        .replace("/Power/VCC", "VCC")
        .replace(&format!("(name \"{net}\")"), "(name \"NEW_NET\")")
}

/// The messages of a served dry run's `routed_pad_net_change` conflicts.
fn routed_pad_conflicts(plan: &serde_json::Value) -> Vec<String> {
    plan["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic["code"] == "routed_pad_net_change")
        .map(|diagnostic| diagnostic["message"].as_str().unwrap().to_string())
        .collect()
}

/// KiCad's captured `R1` under its `GND` pour, with the stand-in DRC
/// reporting one short.
async fn served_r1_under_the_gnd_pour() -> ServedSync {
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: vec![zone_item(GND_POUR)],
        nets: vec!["VCC", "GND"],
    })
    .await;
    served.hold_drc_report(
        r#"{"violations":[{"type":"shorting_items","severity":"error","description":"Items shorting two nets"}],"unconnected_items":[],"schematic_parity":[]}"#,
    );
    served
}

/// Each `routed_pad_net_change` of a served dry run, as an entry of
/// `acknowledged_net_changes`.
fn acknowledging_each(plan: &serde_json::Value) -> serde_json::Value {
    plan["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic["code"] == "routed_pad_net_change")
        .map(|diagnostic| {
            let change = &diagnostic["net_change"];
            serde_json::json!({
                "reference": diagnostic["reference"],
                "pad": change["pad"],
                "from": change["from"],
                "to": change["to"],
            })
        })
        .collect()
}

/// R1's pad 2 net on the mock's board.
fn held_r1_pad_2(served: &ServedSync) -> Option<String> {
    let held = served.held.lock().unwrap();
    let footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
        held[0].value.as_slice(),
    )
    .expect("the board's R1");
    board_footprint_from_instance(&footprint)
        .expect("R1 identity")
        .pad_nets
        .get("2")
        .cloned()
}

/// The classes KiCad 10.0.5 refuses to list on every board.
fn kicad_10_refusals() -> Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType> {
    use copperpour_ipc::gen::kiapi::common::types::KiCadObjectType as Kind;
    vec![Kind::KotPcbTable, Kind::KotPcbGenerator]
}

/// One refused class, as `staging.unavailable_item_classes` reports it.
fn refused_class(class: &str) -> serde_json::Value {
    serde_json::json!({
        "class": class,
        "reason": "refused",
        "kiapi_status": "AS_BAD_REQUEST",
        "message": crate::tools::pcb_board::board_mock::KICAD_CLASS_REFUSAL,
    })
}

/// The two diagnostics for the unusable VQFN footprints, as #657 names
/// them: every part and both footprints.
fn unusable_vqfn_diagnostics() -> serde_json::Value {
    serde_json::json!([
        {
            "code": "unsupported_library_footprint",
            "message": format!(
                "{TEXAS_VQFN} cannot be placed (needed by U1, U2): custom pad '1': its \
                     (clearance convexhull) has no field in KiCad 10's pad message, which \
                     gives the pad the outline clearance"
            ),
            "reference": null,
            "references": ["U1", "U2"],
            "footprint_id": TEXAS_VQFN
        },
        {
            "code": "unsupported_library_footprint",
            "message": format!(
                "{GENERIC_VQFN} cannot be placed (needed by U3): custom pad '1': its \
                     (clearance convexhull) has no field in KiCad 10's pad message, which \
                     gives the pad the outline clearance"
            ),
            "reference": "U3",
            "references": ["U3"],
            "footprint_id": GENERIC_VQFN
        }
    ])
}

/// The captured R1 (`schematic_backed_resistor`, pads 1 and 2) at `value`
/// on `footprint_id`, its pins 1 and 2 on VCC and GND, with `components`
/// and `nets` added to the export.
fn r1_netlist_with(value: &str, footprint_id: &str, components: &str, nets: &str) -> String {
    format!(
        r#"(export
  (components
    (comp (ref "R1") (value "{value}") (footprint "{footprint_id}")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid"))
{components}  )
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))
{nets}  ))
"#
    )
}

/// R1's pin 3 alone on its net, as KiCad exports a pin with a
/// no-connect flag.
const R1_PIN_3_NO_CONNECT: &str = r#"    (net (code "3") (name "unconnected-(R1-Pad3)") (class "Default")
      (node (ref "R1") (pin "3") (pintype "passive+no_connect")))
"#;

fn holding_r1() -> HeldBoard {
    HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    }
}

/// The sheet name, sheet file and footprint filters of each added
/// footprint, by reference, as `CreateItems` received them.
fn sent_sheets(served: &ServedSync) -> Vec<(String, String, String, String)> {
    served
        .sent_footprints()
        .iter()
        .map(|footprint| {
            (
                field_text(&footprint.reference_field),
                footprint.symbol_sheet_name.clone(),
                footprint.symbol_sheet_filename.clone(),
                footprint.symbol_footprint_filters.clone(),
            )
        })
        .collect()
}

/// The schematic's R1 on `footprint_id`, on the captured R1's nets.
fn r1_on(footprint_id: &str) -> String {
    ONE_RESISTOR
        .replace("Resistor_SMD:R_0603_1608Metric", footprint_id)
        .replace("/Power/VCC", "VCC")
}

/// KiCad's own R1 (`Resistor_SMD:R_0402`, linked to the `/Power/`
/// symbol), its pads given the schematic pins Update PCB writes, a custom
/// property its old library gave it, and the symbol's `MPN` field.
fn r1_with_pins_and_an_old_property() -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance
{
    use copperpour_ipc::gen::kiapi;
    let mut footprint = kiapi::board::types::FootprintInstance::decode(
        schematic_backed_resistor().value.as_slice(),
    )
    .unwrap();
    let definition = footprint.definition.as_mut().unwrap();
    for item in &mut definition.items {
        if item.type_url.ends_with("kiapi.board.types.Pad") {
            let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
            pad.symbol_pin = Some(kiapi::board::types::SymbolPinInfo {
                name: format!("P{}", pad.number),
                r#type: kiapi::common::types::ElectricalPinType::EptPassive as i32,
                no_connect: false,
            });
            item.value = pad.encode_to_vec();
        }
    }
    for (name, value) in [("Old_Library_Generator", "old"), ("MPN", "RC0402")] {
        let mut property = footprint.value_field.clone().unwrap();
        property.id = None;
        property.name = name.to_string();
        if let Some(text) = property.text.as_mut() {
            text.id = None;
            if let Some(text) = text.text.as_mut() {
                text.text = value.to_string();
            }
        }
        definition.items.push(copperpour_ipc::builders::pack_any(
            &property,
            "kiapi.board.types.Field",
        ));
    }
    footprint
}

/// `footprint`'s custom property names.
fn custom_property_names(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> Vec<String> {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.Field"))
        .map(|item| {
            copperpour_ipc::gen::kiapi::board::types::Field::decode(item.value.as_slice())
                .unwrap()
                .name
        })
        .collect()
}

/// A library footprint in the older `fp_text reference` / `fp_text value`
/// form, as KiCad 7 wrote them and many libraries still are: two 0603
/// pads, a silkscreen line, and Reference and Value as texts.
const OLDER_FORM_0603: &str = r#"(footprint "R_0603_OlderForm"
  (version 20240108)
  (generator "pcbnew")
  (layer "F.Cu")
  (descr "0603 in the fp_text form")
  (attr smd)
  (pad "1" smd roundrect (at -0.825 0) (size 0.8 0.95) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "2" smd roundrect (at 0.825 0) (size 0.8 0.95) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (fp_line (start -0.16 -0.51) (end 0.16 -0.51) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_text reference "REF**" (at 0 -1.43 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "R_0603_OlderForm" (at 0 1.43 0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
)
"#;

/// R1 to be replaced by `footprint_id`, beside a C1 to be added.
fn r1_on_beside_c1(footprint_id: &str) -> String {
    let netlist = r1_on(footprint_id);
    let closing = "(pin (num \"2\")))))))\n  (nets";
    assert!(netlist.contains(closing), "ONE_RESISTOR changed shape");
    netlist.replace(
        closing,
        &format!(
            "(pin (num \"2\"))))))\n    (comp\n      (ref \"C1\")\n      (value \"100n\")\n      \
                 (footprint \"{STOCK_0603}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      \
                 (tstamps \"C1-uuid\")\n      (units (unit (name \"A\") (pins (pin (num \"1\")) \
                 (pin (num \"2\")))))))\n  (nets"
        ),
    )
}

/// KiCad's own R1 linked to its symbol, R3 with no symbol, and MH1 with
/// none and the board-only attribute, as the mock's board holds them
/// (#861). R3 and MH1 are the captured board-only footprint, which KiCad
/// sent locked and without the attribute; MH1 is given it, and R3 the
/// lock the test asks for.
fn held_with_footprints_without_symbols(r3_locked: bool) -> HeldBoard {
    use copperpour_ipc::gen::kiapi::common::types::LockedState;
    let mut r3 = board_only_instance("r3-kiid", "R3");
    r3.locked = if r3_locked {
        LockedState::LsLocked
    } else {
        LockedState::LsUnlocked
    } as i32;
    let mut mounting_hole = board_only_instance("mh1-kiid", "MH1");
    mounting_hole
        .attributes
        .get_or_insert_with(Default::default)
        .not_in_schematic = true;
    HeldBoard {
        footprints: vec![
            schematic_backed_resistor(),
            copperpour_ipc::builders::pack_any(&r3, "kiapi.board.types.FootprintInstance"),
            copperpour_ipc::builders::pack_any(
                &mounting_hole,
                "kiapi.board.types.FootprintInstance",
            ),
        ],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    }
}

/// The references of the footprints the mock's board now holds.
fn held_references(served: &ServedSync) -> Vec<String> {
    served
        .held
        .lock()
        .unwrap()
        .iter()
        .map(|item| {
            let footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                item.value.as_slice(),
            )
            .unwrap();
            field_text(&footprint.reference_field)
        })
        .collect()
}

/// The footprints a response names as kept without a symbol, as
/// `(reference, kiid, locked)`.
fn named_without_symbols(response: &serde_json::Value) -> Vec<(String, String, bool)> {
    response["footprints_without_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("no footprints_without_symbols: {response:#}"))
        .iter()
        .map(|footprint| {
            (
                footprint["reference"].as_str().unwrap().to_string(),
                footprint["kiid"].as_str().unwrap().to_string(),
                footprint["locked"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn r3_named(locked: bool) -> Vec<(String, String, bool)> {
    vec![("R3".to_string(), "r3-kiid".to_string(), locked)]
}

/// [`r1_on`] with R3 beside it, and the served schematic's R3 (KiCad's
/// own `structural_scans_kicad10.kicad_sch`) marked Exclude from board,
/// as `edit_schematic_component` with `on_board: false` writes it.
fn excluding_r3(served: &ServedSync) -> String {
    let schematic = std::fs::read_to_string(&served.schematic).unwrap();
    let r3 = schematic
        .find("(reference \"R3\")")
        .expect("the fixture places R3");
    let flag = schematic[..r3]
        .rfind("(on_board yes)")
        .expect("R3 is on the board in the fixture");
    let mut excluded = schematic;
    excluded.replace_range(flag..flag + "(on_board yes)".len(), "(on_board no)");
    std::fs::write(&served.schematic, excluded).unwrap();

    let netlist = r1_on(STOCK_0603);
    let closing = "(pin (num \"2\")))))))\n  (nets";
    assert!(netlist.contains(closing), "ONE_RESISTOR changed shape");
    netlist.replace(
        closing,
        &format!(
            "(pin (num \"2\"))))))\n    (comp\n      (ref \"R3\")\n      (value \"10k\")\n      \
                 (footprint \"{STOCK_0603}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      \
                 (tstamps \"R3-uuid\")\n      (units (unit (name \"A\") (pins (pin (num \"1\")) \
                 (pin (num \"2\")))))))\n  (nets"
        ),
    )
}

/// A via as KiCad lists it (the #96 repro board's first via), given
/// `kiid` and `net` (#862).
fn via_on(kiid: &str, net: &str) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi;
    let listed = kiapi::common::commands::GetItemsResponse::decode(
        include_bytes!("../../../tests/fixtures/issue_96_vias.ipc.bin").as_slice(),
    )
    .expect("the captured vias decode")
    .items;
    let mut via = kiapi::board::types::Via::decode(listed[0].value.as_slice())
        .expect("the first captured item is a via");
    via.id = Some(kiapi::common::types::Kiid {
        value: kiid.to_string(),
    });
    via.net = Some(kiapi::board::types::Net {
        code: None,
        name: net.to_string(),
    });
    copperpour_ipc::builders::pack_any(&via, "kiapi.board.types.Via")
}

/// The net of each via and zone the mock's board holds, by KIID.
fn held_copper_nets(served: &ServedSync) -> BTreeMap<String, String> {
    use copperpour_ipc::gen::kiapi::board::types::Via;
    let mut nets = BTreeMap::new();
    for via in served.vias.lock().unwrap().iter() {
        let net = Via::decode(via.value.as_slice()).unwrap().net.unwrap().name;
        nets.insert(copper_kiid(via), net);
    }
    for zone in served.zones.lock().unwrap().iter() {
        let ZoneNet::Copper(net) = zone_net(zone.value.as_slice()) else {
            panic!("a copper zone");
        };
        nets.insert(copper_kiid(zone), net.name);
    }
    nets
}

/// The captured pour's KIID.
fn pour_kiid() -> String {
    copper_kiid(&zone_item(GND_POUR))
}

/// R1 under the GND pour, as [`served_r1_under_the_gnd_pour`], with a via
/// on GND (which the netlist renames) and a via on VCC (which it keeps).
async fn served_with_gnd_copper() -> ServedSync {
    let served = served_r1_under_the_gnd_pour().await;
    *served.vias.lock().unwrap() = vec![via_on("via-gnd", "GND"), via_on("via-vcc", "VCC")];
    served
}

/// The dry run that acknowledges each routed pad change of `netlist`.
async fn acknowledged_dry_run(
    served: &ServedSync,
    netlist: &str,
) -> (serde_json::Value, serde_json::Value) {
    let refused = served.dry_run(netlist).await;
    assert_eq!(refused["status"], "conflict", "{refused:#}");
    let acknowledged = acknowledging_each(&refused);
    let (_, plan) = served
        .sync(
            netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;
    (plan, acknowledged)
}

/// The apply of `plan`, made with `acknowledged`.
async fn apply_acknowledged(
    served: &ServedSync,
    netlist: &str,
    plan: &serde_json::Value,
    acknowledged: &serde_json::Value,
) -> (bool, serde_json::Value) {
    served
        .sync(
            netlist,
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
                "acknowledged_net_changes": acknowledged,
            }),
        )
        .await
}

/// The `(kind, kiid, from, to)` of each planned copper move.
fn copper_moves(response: &serde_json::Value) -> Vec<(String, String, String, String)> {
    response["copper_net_changes"]
        .as_array()
        .unwrap_or_else(|| panic!("no copper_net_changes: {response:#}"))
        .iter()
        .map(|change| {
            let text = |key: &str| change[key].as_str().unwrap().to_string();
            (text("kind"), text("kiid"), text("from"), text("to"))
        })
        .collect()
}

fn move_of(kind: &str, kiid: &str, from: &str, to: &str) -> (String, String, String, String) {
    (
        kind.to_string(),
        kiid.to_string(),
        from.to_string(),
        to.to_string(),
    )
}

/// R1's pad `number` as KiCad lists it inside the footprint.
fn r1_pad(number: &str) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, Pad};
    let r1 = FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
    r1.definition
        .unwrap()
        .items
        .into_iter()
        .find(|child| {
            copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad")
                && Pad::decode(child.value.as_slice()).unwrap().number == number
        })
        .unwrap_or_else(|| panic!("R1 has a pad {number}"))
}

// ─── #149: pad fabrication properties ────────────────────────────────────

/// Each pad's number and fabrication property, sorted.
fn pad_properties_of(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> Vec<(String, i32)> {
    let mut pads = footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
        .map(|item| {
            let pad = copperpour_ipc::gen::kiapi::board::types::Pad::decode(item.value.as_slice())
                .unwrap();
            (pad.number, pad.fab_property)
        })
        .collect::<Vec<_>>();
    pads.sort();
    pads
}

// ─── #836: refuse an update that would drop what a footprint owns ───────

/// A board KiCad wrote: U1 owns a group, a point and a variant, R1 owns
/// none of them, and a board-level group holds both.
/// Provenance in `tests/fixtures/footprint_children_kicad10.README.md`.
const FOOTPRINT_CHILDREN: &str =
    include_str!("../../../tests/fixtures/footprint_children_kicad10.kicad_pcb");
const OWNS_ALL_THREE: &str = "c7901245-d0c4-476f-8215-85f1183d04de";
const OWNS_NONE: &str = "20a44d6b-d6bb-4ec1-ac0c-f1d3d7e20db2";
const REFUSED_FOR: &str = "R1 owns 1 group, 1 point and variant 'NoTimer'";

/// The captured R1 as `reference` at `value` under `kiid`, linked to the
/// symbol `symbol` on `/Power/`, with `sheet_name` as its sheet name.
fn captured_resistor(
    kiid: &str,
    reference: &str,
    symbol: &str,
    value: &str,
    sheet_name: &str,
) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi;

    let mut footprint = kiapi::board::types::FootprintInstance::decode(
        include_bytes!("../../../tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
    )
    .unwrap();
    footprint.id = Some(kiapi::common::types::Kiid {
        value: kiid.to_string(),
    });
    footprint.symbol_path = Some(kiapi::common::types::SheetPath {
        path: ["sheet-uuid", symbol]
            .into_iter()
            .map(|value| kiapi::common::types::Kiid {
                value: value.to_string(),
            })
            .collect(),
        path_human_readable: "/Power/".to_string(),
    });
    footprint.symbol_sheet_name = sheet_name.to_string();
    set_field_text(&mut footprint.reference_field, "Reference", reference);
    set_field_text(&mut footprint.value_field, "Value", value);
    copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
}

/// KiCad holding `footprints`, which answers `SaveDocumentToString` with
/// [`FOOTPRINT_CHILDREN`].
async fn served_children(footprints: Vec<prost_types::Any>) -> ServedSync {
    let served = ServedSync::holding(HeldBoard {
        footprints,
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    served.hold_board_text(FOOTPRINT_CHILDREN);
    served
}

/// KiCad holding the captured R1 at `value` under `kiid`, a KIID of
/// [`FOOTPRINT_CHILDREN`], with the sheet name and units KiCad's Update
/// PCB gives it (#237, #682).
async fn served_r1(kiid: &str, value: &str) -> ServedSync {
    served_children(vec![with_r1_units(captured_resistor(
        kiid,
        "R1",
        "symbol-uuid",
        value,
        "/Power/",
    ))])
    .await
}

/// `item`, a footprint, with the units [`ONE_RESISTOR`]'s R1 has (#682).
fn with_r1_units(item: prost_types::Any) -> prost_types::Any {
    use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, FootprintUnitInfo};

    let mut footprint = FootprintInstance::decode(item.value.as_slice()).unwrap();
    footprint.symbol_units = vec![FootprintUnitInfo {
        name: "A".to_string(),
        pin_numbers: vec!["1".to_string(), "2".to_string()],
    }];
    copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
}

/// The schematic's R1, at 10k, on the captured footprint and its nets.
fn r1_at_10k() -> String {
    ONE_RESISTOR
        .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
        .replace("/Power/VCC", "VCC")
}

/// A dry run, then the apply of its plan revision, whatever its status.
async fn dry_run_then_apply(served: &ServedSync) -> (serde_json::Value, serde_json::Value) {
    let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
    let (_, applied) = served
        .call(serde_json::json!({
            "schematic": served.schematic.to_string_lossy(),
            "board": served.board.to_string_lossy(),
            "dry_run": false,
            "expected_plan_revision": plan["plan_revision"],
        }))
        .await;
    (plan, applied)
}

fn assert_refused(body: &serde_json::Value, because: &str) {
    assert_eq!(body["status"], "conflict", "{body:#}");
    assert_eq!(body["changes"], serde_json::json!([]));
    assert_eq!(body["coverage"]["footprints_updated"]["planned"], 0);
    let diagnostic = &body["diagnostics"][0];
    assert_eq!(diagnostic["code"], "footprint_children_not_carried");
    assert_eq!(diagnostic["reference"], "R1");
    assert_eq!(diagnostic["footprint_id"], "Resistor_SMD:R_0402");
    let message = diagnostic["message"].as_str().unwrap();
    assert!(message.starts_with(because), "{message}");
}

/// R1 and R2 at 10k on `/Power/`, each on the captured footprint's nets.
const TWO_RESISTORS: &str = r#"
(export
  (components
    (comp
      (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0402")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2"))))))
    (comp
      (ref "R2")
      (value "10k")
      (footprint "Resistor_SMD:R_0402")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid-2")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2")))))))
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "R2") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive"))
      (node (ref "R2") (pin "2") (pintype "passive")))))
"#;

// ─── #69: a bounded response, verdict first ────────────────────────────

/// `count` parts the board lacks, each a stock 0603.
fn additions(count: usize) -> String {
    let references = (1..=count).map(|n| format!("C{n}")).collect::<Vec<_>>();
    let parts = references
        .iter()
        .map(|reference| (reference.as_str(), STOCK_0603, &["1", "2"][..]))
        .collect::<Vec<_>>();
    exported_netlist(&parts)
}

// ─── #85: net-tie pad groups ────────────────────────────────────────────

/// KiCad 10.0.8's stock three-pad net tie (`net_tie_kicad10.README.md`).
const NET_TIE_3_THT_SOURCE: &str =
    include_str!("../../../tests/fixtures/net_tie_3_tht_pad0.3mm_kicad10.kicad_mod");
const NET_TIE_3_THT: &str = "NetTie:NetTie-3_THT_Pad0.3mm";

fn kicad_10_0(patch: u32) -> copperpour_ipc::IpcKiCadVersion {
    copperpour_ipc::IpcKiCadVersion {
        major: 10,
        minor: 0,
        patch,
        full_version: format!("10.0.{patch}"),
    }
}

/// Write the stock net tie to a `NetTie.pretty` library in `served`'s
/// project, with its `fp-lib-table` row.
fn net_tie_library(served: &ServedSync) {
    let project_dir = served.board.parent().unwrap();
    let pretty = project_dir.join("NetTie.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    std::fs::write(
        pretty.join("NetTie-3_THT_Pad0.3mm.kicad_mod"),
        NET_TIE_3_THT_SOURCE,
    )
    .unwrap();
    let fp_table = project_dir.join("fp-lib-table");
    let existing_table = std::fs::read_to_string(&fp_table)
        .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
    let updated_table = existing_table.replacen(
        "\n)",
        "\n  (lib (name \"NetTie\")(type \"KiCad\")(uri \"${KIPRJMOD}/NetTie.pretty\")(options \"\")(descr \"\"))\n)",
        1,
    );
    std::fs::write(&fp_table, updated_table).unwrap();
}

/// Write each `(name, text)` footprint to a `Margins.pretty` library in
/// `served`'s project, with its `fp-lib-table` row.
fn margins_library(served: &ServedSync, footprints: &[(&str, &str)]) {
    let project_dir = served.board.parent().unwrap();
    let pretty = project_dir.join("Margins.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    for (name, source) in footprints {
        std::fs::write(pretty.join(format!("{name}.kicad_mod")), source).unwrap();
    }
    let fp_table = project_dir.join("fp-lib-table");
    let existing_table = std::fs::read_to_string(&fp_table)
        .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
    let updated_table = existing_table.replacen(
        "\n)",
        "\n  (lib (name \"Margins\")(type \"KiCad\")(uri \"${KIPRJMOD}/Margins.pretty\")(options \"\")(descr \"\"))\n)",
        1,
    );
    std::fs::write(&fp_table, updated_table).unwrap();
}
