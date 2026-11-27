use super::*;

/// #244. A footprint's pads, graphics and text all live in one repeated
/// `Any` field, and proto3 skips field numbers it does not recognise rather
/// than failing — so a `BoardGraphicShape` decodes cleanly as a near-empty
/// `Pad`. Filtering that list with `Pad::decode(..).ok()` therefore matched
/// every graphic, and packing the decoded value back re-typed it. In
/// neusse's benchmark an 8-pad SOIC-8 came out of a sync with 28 pads —
/// the 20 extras nameless, at (0,0), one per lost graphic — and no artwork.
#[test]
fn syncing_a_footprint_leaves_its_graphics_as_graphics() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let item = footprint_with_artwork("U1");
    let before = child_types(&item);

    // Sanity: the fixture must actually carry the mixture, or this test
    // proves nothing — which is the trap the pre-existing sync test fell
    // into by passing `&[]` graphics.
    assert_eq!(before.get("kiapi.board.types.Pad"), Some(&1));
    assert_eq!(before.get("kiapi.board.types.BoardGraphicShape"), Some(&5));
    assert_eq!(before.get("kiapi.board.types.BoardText"), Some(&1));

    let change = PlannedChange::Update {
        kiid: "u1-kiid".to_string(),
        reference: "U1".to_string(),
        value: "NE555".to_string(),
        symbol_path: "/root/u1".to_string(),
        dnp: false,
        exclude_from_bom: false,
        pad_nets: BTreeMap::from([("1".to_string(), "GND".to_string())]),
        unconnected_pins: BTreeSet::new(),
        preserve: PreservedBoardState {
            position: Point { x: 25.0, y: 30.0 },
            rotation: 0.0,
            layer: "F.Cu".to_string(),
            locked: false,
        },
        fields: Vec::new(),
        sheet: SymbolSheet::default(),
        sheet_only: false,
        replace_footprint: None,
    };
    let updated =
        update_footprint_item(&item, &change, &BTreeMap::from([("GND".to_string(), 1)])).unwrap();

    assert_eq!(
        child_types(&updated),
        before,
        "sync re-typed footprint children; graphics must survive as graphics"
    );

    // And the pad still got the net it was there to get.
    let footprint =
        kiapi::board::types::FootprintInstance::decode(updated.value.as_slice()).unwrap();
    let pad = footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad"))
        .map(|child| kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap())
        .next()
        .expect("the pad survived");
    assert_eq!(pad.net.as_ref().unwrap().name, "GND");
}

/// KiCad's Update PCB keeps a part excluded from the BOM on the board and
/// marks its footprint; only Exclude from board keeps it off (#875).
#[test]
fn a_part_excluded_from_the_bom_stays_on_the_board() {
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("flags.kicad_sch");
    let symbol = |reference: &str, uuid: &str, in_bom: &str, on_board: &str| {
        format!(
            "(symbol (lib_id \"x:y\") (in_bom {in_bom}) (on_board {on_board}) (dnp no) \
                 (uuid \"{uuid}\") (instances (project \"p\" (path \"/root\" \
                 (reference \"{reference}\") (unit 1)))))"
        )
    };
    std::fs::write(
        &schematic,
        format!(
            "(kicad_sch (uuid \"root\") {} {} {})",
            symbol("TP1", "tp1", "no", "yes"),
            symbol("R1", "r1", "yes", "yes"),
            symbol("H1", "h1", "no", "no"),
        ),
    )
    .unwrap();
    let mut design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![
            resistor("TP1", "/root/tp1"),
            resistor("R1", "/root/r1"),
            resistor("H1", "/root/h1"),
        ],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    apply_saved_symbol_flags(&[schematic], &mut design).unwrap();

    let kept = design
        .components
        .iter()
        .map(|component| (component.reference.as_str(), component.exclude_from_bom))
        .collect::<Vec<_>>();
    assert_eq!(kept, [("TP1", true), ("R1", false)]);
    let skipped = design
        .skipped
        .iter()
        .map(|component| component.reference.as_str())
        .collect::<Vec<_>>();
    assert_eq!(skipped, ["H1"]);
}

/// The flags `edit_schematic_component` sets reach the plan through the
/// sheet this module reads them from (#34): U1 excluded from the board is
/// skipped by flag instead of added, and R1 excluded from the BOM is added
/// with its footprint excluded from the BOM. The sheet is KiCad-saved.
#[tokio::test]
async fn edited_board_and_bom_flags_change_the_plan() {
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("flags.kicad_sch");
    std::fs::write(
        &schematic,
        include_str!("../../../../tests/fixtures/dnp_kicad10.kicad_sch"),
    )
    .unwrap();
    let plan = || {
        let mut design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("U1", "/u1"), resistor("R1", "/r1")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        apply_saved_symbol_flags(std::slice::from_ref(&schematic), &mut design).unwrap();
        let plan = plan_sync("netlist", &design, &board_with(Vec::new()));
        (design, plan)
    };

    let (design, before) = plan();
    assert_eq!(before.counts.added.planned, 2);
    assert_eq!(before.counts.skipped_by_flag.planned, 0);
    assert!(design.components.iter().all(|c| !c.exclude_from_bom));

    let edit = crate::tools::sch_components::tools()
        .into_iter()
        .find(|tool| tool.name == "edit_schematic_component")
        .unwrap();
    let context = std::sync::Arc::new(crate::tools::ToolContext::new(
        crate::tools::ServerConfig::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    ));
    for (reference, flag) in [("U1", "on_board"), ("R1", "in_bom")] {
        let args = serde_json::json!({
            "schematic": schematic.display().to_string(),
            "reference": reference,
            flag: false
        });
        let result = (edit.handler)(&args, context.clone()).await.unwrap();
        assert!(!result.is_error, "{:?}", result.content);
    }

    let (design, after) = plan();
    let skipped = design
        .skipped
        .iter()
        .map(|component| component.reference.as_str())
        .collect::<Vec<_>>();
    assert_eq!(skipped, ["U1"]);
    assert_eq!(after.counts.skipped_by_flag.planned, 1);
    assert_eq!(after.counts.added.planned, 1);
    let r1 = design
        .components
        .iter()
        .find(|component| component.reference == "R1")
        .unwrap();
    assert!(r1.exclude_from_bom);
}

#[test]
fn a_footprint_carries_its_symbol_s_exclude_from_bom() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let item = footprint_with_artwork("TP1");
    let mut footprint =
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();

    for exclude_from_bom in [true, false] {
        apply_footprint_fields(
            &mut footprint,
            "TP1",
            "TP",
            "/root/tp1",
            false,
            exclude_from_bom,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        let instance = footprint.attributes.as_ref().unwrap();
        let definition = footprint
            .definition
            .as_ref()
            .unwrap()
            .attributes
            .as_ref()
            .unwrap();
        assert_eq!(instance.exclude_from_bill_of_materials, exclude_from_bom);
        assert_eq!(definition.exclude_from_bill_of_materials, exclude_from_bom);
    }
}

/// The add path calls `apply_footprint_fields` too (`build_mutation_items`),
/// so a brand-new footprint was corrupted before it ever reached KiCad.
#[test]
fn a_newly_added_footprint_keeps_its_graphics_too() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let item = footprint_with_artwork("U2");
    let before = child_types(&item);
    let mut footprint =
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();

    apply_footprint_fields(
        &mut footprint,
        "U2",
        "NE555",
        "/root/u2",
        false,
        false,
        &BTreeMap::from([("1".to_string(), "VCC".to_string())]),
        &BTreeMap::from([("VCC".to_string(), 3)]),
    )
    .unwrap();

    let repacked =
        copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
    assert_eq!(child_types(&repacked), before);
}

/// The invariant that would have caught #244 on its own.
///
/// `create_items`/`update_items` only confirm KiCad *accepted* each item,
/// and the reported counts are copied from the plan — so the corruption
/// travelled all the way to a success message. Here the exact damage is
/// reproduced (every drawing re-typed as a pad, which is what the old
/// `Pad::decode` filter did) and the shape comparison is shown to see it.
#[test]
fn the_post_apply_check_sees_drawings_turned_into_pads() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;

    let sent = footprint_with_artwork("U4");
    let expected = footprint_shapes(std::iter::once(&sent));
    // The pad and drawing counts are what #244 got wrong.
    assert_eq!((expected["U4"].pads, expected["U4"].drawings), (1, 6));

    // Exactly #244: decode every child as a Pad and pack it back as one.
    let mut corrupted =
        kiapi::board::types::FootprintInstance::decode(sent.value.as_slice()).unwrap();
    for child in &mut corrupted.definition.as_mut().unwrap().items {
        if let Ok(pad) = kiapi::board::types::Pad::decode(child.value.as_slice()) {
            *child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
        }
    }
    let corrupted =
        copperpour_ipc::builders::pack_any(&corrupted, "kiapi.board.types.FootprintInstance");
    let actual = footprint_shapes(std::iter::once(&corrupted));

    // The reported symptom, reproduced: the five graphic shapes each become
    // a pad. The text survives — `BoardText`'s bytes genuinely fail to
    // decode as a `Pad`, while `BoardGraphicShape`'s do not — which is why
    // #239 reported footprints losing their *graphics* while their
    // reference and value text stayed put.
    assert_eq!((actual["U4"].pads, actual["U4"].drawings), (6, 1));
    assert_ne!(actual["U4"], expected["U4"]);
}

/// A child that declares itself a pad and will not decode is a real
/// failure, and has to be reported as *that*.
///
/// Skipping it silently does still end in an error — the "footprint has no
/// pad N" check downstream fires, because the pad never made it into
/// `seen_pads` — but that error sends the reader looking for a missing pad
/// that is in fact present and unreadable. So this asserts the specific
/// message, not merely that something failed: a neuter that restored the
/// silent skip passed an assertion that only checked for the reference.
#[test]
fn an_undecodable_pad_is_reported_not_skipped() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;
    let item = footprint_with_artwork("U3");
    let mut footprint =
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
    for child in &mut footprint.definition.as_mut().unwrap().items {
        if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
            // Wire type 7 does not exist; nothing can decode this.
            child.value = vec![0xff, 0xff, 0xff];
        }
    }

    let error = apply_footprint_fields(
        &mut footprint,
        "U3",
        "NE555",
        "/root/u3",
        false,
        false,
        &BTreeMap::from([("1".to_string(), "VCC".to_string())]),
        &BTreeMap::new(),
    )
    .expect_err("an unreadable pad must not pass silently");
    let text = format!("{error:#}");
    assert!(
        text.contains("U3") && text.contains("cannot read"),
        "must say the pad is unreadable, not that it is missing: {text}"
    );
}

#[test]
fn update_item_changes_only_schematic_owned_fields() {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;

    let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
        "Resistor_SMD:R_0603_1608Metric",
        "R1",
        "1k",
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
        &[],
        &copperpour_ipc::IpcFieldPlacement::default(),
        25.0,
        30.0,
        90.0,
        "F.Cu",
    )
    .unwrap();
    let mut footprint =
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
    footprint.id = Some(kiapi::common::types::Kiid {
        value: "keep-kiid".to_string(),
    });
    footprint.locked = kiapi::common::types::LockedState::LsLocked as i32;
    let item =
        copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
    let change = PlannedChange::Update {
        kiid: "keep-kiid".to_string(),
        reference: "R2".to_string(),
        value: "10k".to_string(),
        symbol_path: "/root/symbol".to_string(),
        dnp: true,
        exclude_from_bom: false,
        pad_nets: BTreeMap::from([("1".to_string(), "VCC".to_string())]),
        unconnected_pins: BTreeSet::new(),
        preserve: PreservedBoardState {
            position: Point { x: 25.0, y: 30.0 },
            rotation: 90.0,
            layer: "F.Cu".to_string(),
            locked: true,
        },
        fields: Vec::new(),
        sheet: SymbolSheet::default(),
        sheet_only: false,
        replace_footprint: None,
    };

    let updated =
        update_footprint_item(&item, &change, &BTreeMap::from([("VCC".to_string(), 7)])).unwrap();
    let updated = kiapi::board::types::FootprintInstance::decode(updated.value.as_slice()).unwrap();

    assert_eq!(updated.id.as_ref().unwrap().value, "keep-kiid");
    assert_eq!(updated.position, footprint.position);
    assert_eq!(updated.orientation, footprint.orientation);
    assert_eq!(updated.layer, footprint.layer);
    assert_eq!(updated.locked, footprint.locked);
    assert!(updated.attributes.as_ref().unwrap().do_not_populate);
    let pad = updated
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find_map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).ok())
        .unwrap();
    assert_eq!(pad.net.as_ref().unwrap().name, "VCC");
    assert_eq!(pad.net.as_ref().unwrap().code.as_ref().unwrap().value, 7);
}

/// The snapshot reads what the planner compares: Datasheet and Description
/// from their own slots, and each custom field by name (#788).
#[test]
fn the_snapshot_reads_a_footprints_fields_by_name() {
    let r1 = captured_r1();
    let shown = shown_custom_field("LCSC", "C1", &r1);

    let board = board_footprint_from_instance(&with_field_item(r1, &shown)).unwrap();

    assert_eq!(
        board.fields,
        BTreeMap::from([
            ("Datasheet".to_string(), String::new()),
            ("Description".to_string(), String::new()),
            ("LCSC".to_string(), "C1".to_string()),
        ])
    );
}

/// A field the snapshot cannot read refuses it, naming the footprint. Read
/// as absent, it would plan a second field of the same name.
#[test]
fn an_undecodable_field_refuses_the_snapshot() {
    let r1 = captured_r1();
    let shown = shown_custom_field("LCSC", "C1", &r1);
    let mut footprint = with_field_item(r1, &shown);
    for child in &mut footprint.definition.as_mut().unwrap().items {
        if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field") {
            // Wire type 7 does not exist; nothing can decode this.
            child.value = vec![0xff, 0xff, 0xff];
        }
    }

    let error = board_footprint_from_instance(&footprint)
        .expect_err("an unreadable field must not pass silently");
    let text = format!("{error:#}");
    assert!(
        text.contains("R1") && text.contains("cannot read"),
        "must name the footprint and say the field is unreadable: {text}"
    );
}

/// On an update, a field the footprint has takes the symbol's text and
/// keeps everything else: layer, visibility, lock, position and font. Both
/// KiCad's own Description and a custom field on the silkscreen (#788).
#[test]
fn an_update_changes_only_the_text_of_a_field_the_footprint_has() {
    let r1 = captured_r1();
    let shown = shown_custom_field("LCSC", "C1", &r1);
    let live = with_field_item(r1, &shown);

    let updated = updated_with(
        &live,
        vec![
            symbol_field("LCSC", "C788002"),
            symbol_field("Description", "Resistor"),
        ],
    );

    assert_eq!(field_text(&updated.description_field), "Resistor");
    assert_eq!(
        presentation(updated.description_field.as_ref().unwrap()),
        presentation(live.description_field.as_ref().unwrap())
    );
    assert_eq!(
        updated.datasheet_field, live.datasheet_field,
        "a field the update does not name is as KiCad sent it"
    );
    let custom = field_items(&updated);
    assert_eq!(custom.len(), 1, "{:?}", custom.keys());
    assert_eq!(field_text(&Some(custom["LCSC"].clone())), "C788002");
    assert_eq!(presentation(&custom["LCSC"]), presentation(&shown));
}

/// A field the footprint lacks is added as Update PCB adds it: hidden on
/// F.Fab at the footprint's position in default fabrication text, and on
/// the back on B.Fab, mirrored, at the footprint's normalised angle (#788).
#[test]
fn an_update_adds_a_field_the_footprint_lacks_as_update_pcb_adds_it() {
    use copperpour_ipc::gen::kiapi::{board::types::BoardLayer, common::types as common};

    let front = captured_r1();
    let added = field_items(&updated_with(
        &front,
        vec![symbol_field("MPN", "MPN-788-R1")],
    ))
    .remove("MPN")
    .expect("an added field");
    assert_eq!(field_text(&Some(added.clone())), "MPN-788-R1");
    let (layer, visible, locked, position, attributes) = presentation(&added);
    let attributes = attributes.unwrap();
    assert_eq!(layer, BoardLayer::BlFFab as i32);
    assert!(!visible);
    assert_eq!(locked, common::LockedState::LsUnlocked as i32);
    assert_eq!(position, front.position);
    assert_eq!(
        attributes.size,
        Some(common::Vector2 {
            x_nm: 1_000_000,
            y_nm: 1_000_000
        })
    );
    assert_eq!(attributes.stroke_width.unwrap().value_nm, 150_000);
    assert_eq!(attributes.angle.unwrap().value_degrees, 0.0);
    assert!(!attributes.mirrored && !attributes.keep_upright && !attributes.italic);
    assert!(attributes.multiline);

    let mut back = captured_r1();
    back.layer = BoardLayer::BlBCu as i32;
    back.orientation = Some(common::Angle {
        value_degrees: -90.0,
    });
    let added = field_items(&updated_with(
        &back,
        vec![symbol_field("MPN", "MPN-788-R1")],
    ))
    .remove("MPN")
    .expect("an added field");
    let (layer, _, _, _, attributes) = presentation(&added);
    let attributes = attributes.unwrap();
    assert_eq!(layer, BoardLayer::BlBFab as i32);
    assert!(attributes.mirrored);
    assert_eq!(attributes.angle.unwrap().value_degrees, 270.0);
}

/// A missing Datasheet is made as KiCad's footprint constructor makes it,
/// checked against KiCad's own captured empty Datasheet (#788).
#[test]
fn a_missing_datasheet_is_made_as_kicads_footprint_constructor_makes_it() {
    let kicad = captured_r1();
    let mut built = captured_r1();
    built.datasheet_field = None;

    apply_symbol_fields(&mut built, &[symbol_field("Datasheet", "r1-datasheet")]).unwrap();

    assert_eq!(field_text(&built.datasheet_field), "r1-datasheet");
    assert_eq!(
        presentation(built.datasheet_field.as_ref().unwrap()),
        presentation(kicad.datasheet_field.as_ref().unwrap())
    );
}

/// #657: the first unusable footprint stopped preparation, so a plan that
/// needed two of them heard about one, and about neither by name.
#[test]
fn every_unusable_footprint_is_named_with_the_parts_that_need_it() {
    let (_temp, board) = project_with_stock_footprints();
    let plan = plan_adding(vec![
        planned_add("U1", TEXAS_VQFN, &["1"]),
        planned_add("C1", STOCK_0603, &["1", "2"]),
        planned_add("U3", GENERIC_VQFN, &["1"]),
        planned_add("U2", TEXAS_VQFN, &["1"]),
    ]);

    let (prepared, unprepared) = prepare_additions(&board, &plan);

    assert_eq!(
        prepared.keys().map(String::as_str).collect::<Vec<_>>(),
        [STOCK_0603],
        "a footprint that can be placed is still prepared"
    );
    let diagnostics = unprepared
        .into_iter()
        .map(UnpreparedFootprint::into_diagnostic)
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");

    let texas = &diagnostics[0];
    assert_eq!(texas.code, "unsupported_library_footprint");
    assert_eq!(texas.footprint_id.as_deref(), Some(TEXAS_VQFN));
    assert_eq!(texas.references, ["U1", "U2"]);
    assert_eq!(
        texas.reference, None,
        "two parts are concerned, so no single one is named"
    );
    assert!(
        texas.message.contains(TEXAS_VQFN)
            && texas.message.contains("U1, U2")
            && texas.message.contains("custom pad '1'")
            && texas.message.contains("(clearance convexhull)"),
        "{}",
        texas.message
    );

    let generic = &diagnostics[1];
    assert_eq!(generic.code, "unsupported_library_footprint");
    assert_eq!(generic.footprint_id.as_deref(), Some(GENERIC_VQFN));
    assert_eq!(generic.references, ["U3"]);
    assert_eq!(generic.reference.as_deref(), Some("U3"));
}

/// A stock footprint with custom pads is prepared for Update PCB's
/// additions (#679): each custom pad carries its anchor and primitives.
#[test]
fn a_stock_footprint_with_custom_pads_is_prepared() {
    let (_temp, board) = project_with_stock_footprints();
    let plan = plan_adding(vec![planned_add("U9", PLACEABLE_VQFN, &["1"])]);

    let (prepared, unprepared) = prepare_additions(&board, &plan);

    assert!(unprepared.is_empty(), "{unprepared:?}");
    let part = prepared.get(PLACEABLE_VQFN).expect("the VQFN is prepared");
    let custom = part
        .pads
        .iter()
        .filter_map(|pad| {
            pad.custom
                .as_ref()
                .map(|custom| (pad.number.as_str(), custom))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        custom.iter().map(|(number, _)| *number).collect::<Vec<_>>(),
        ["1", "5", "6", "10", "11", "15", "16", "20"]
    );
    let (_, first) = custom[0];
    assert_eq!(first.anchor, copperpour_ipc::IpcCustomPadAnchor::Circle);
    assert_eq!(
        first.primitives,
        [copperpour_ipc::IpcPadPrimitive::Polygon {
            points: vec![
                (-0.375, -0.075),
                (0.254289, -0.075),
                (0.375, 0.045711),
                (0.375, 0.075),
                (-0.375, 0.075)
            ],
            width: 0.1,
            filled: true
        }]
    );
}

/// The caller's next step differs with the stage that failed: fix the
/// library table, fix the file, or substitute the footprint. The codes are
/// the ones `update_footprints_from_library` reports for the same three.
#[test]
fn each_stage_of_preparation_fails_under_its_own_code() {
    let (temp, board) = project_with_stock_footprints();
    std::fs::write(
        temp.path()
            .join("Capacitor_SMD.pretty/Unreadable.kicad_mod"),
        [0xff, 0xfe, 0x00, 0x28],
    )
    .unwrap();
    let plan = plan_adding(vec![
        planned_add("J1", "Konnect_No_Such_Library:Nothing", &["1"]),
        planned_add("C9", "Capacitor_SMD:Unreadable", &["1"]),
        planned_add("U1", TEXAS_VQFN, &["1"]),
    ]);

    let (prepared, unprepared) = prepare_additions(&board, &plan);

    assert!(prepared.is_empty());
    let by_reference = unprepared
        .iter()
        .map(|failed| (failed.references[0].as_str(), failed.code))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        by_reference,
        BTreeMap::from([
            ("C9", "footprint_library_read_failed"),
            ("J1", "footprint_library_resolution_failed"),
            ("U1", "unsupported_library_footprint"),
        ])
    );
}

/// A message names at most eight parts; `references` names them all.
#[test]
fn a_message_is_bounded_and_the_reference_list_is_complete() {
    let references = (1..=30).map(|n| format!("C{n}")).collect::<Vec<_>>();
    let diagnostic = UnpreparedFootprint {
        footprint_id: TEXAS_VQFN.to_string(),
        references: references.clone(),
        code: "unsupported_library_footprint",
        reason: "custom-shape pads".to_string(),
    }
    .into_diagnostic();

    assert_eq!(diagnostic.references, references);
    assert!(
        diagnostic.message.contains("C8 and 22 more") && !diagnostic.message.contains("C9,"),
        "{}",
        diagnostic.message
    );
}

/// Add path: `footprint C1 has no pad 3` used to be raised only inside
/// the apply, after a dry run that said `ready`.
#[test]
fn a_connected_pad_the_library_footprint_lacks_is_found_while_planning() {
    let (_temp, board) = project_with_stock_footprints();
    let plan = plan_adding(vec![
        planned_add("C1", STOCK_0603, &["1", "2", "3", "4"]),
        planned_add("C2", STOCK_0603, &["1", "2"]),
    ]);
    let (prepared, unprepared) = prepare_additions(&board, &plan);
    assert!(unprepared.is_empty());

    let diagnostics = additions_missing_pads(&plan, &prepared);

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    let missing = &diagnostics[0];
    assert_eq!(missing.code, "footprint_pad_missing");
    assert_eq!(missing.reference.as_deref(), Some("C1"));
    assert_eq!(missing.references, ["C1"]);
    assert_eq!(missing.footprint_id.as_deref(), Some(STOCK_0603));
    assert!(missing.message.contains("pads 3, 4"), "{}", missing.message);
}

/// Update path: the same rule against the pads of the live footprint.
/// Before #657, this planned an update with one pad reassigned and said
/// `ready`; the apply then failed on `footprint R1 has no pad 3`. Then it
/// refused the whole plan. Now only that update is refused, for the
/// handler to skip the part (`refused_updates`), as it skips an added
/// part that lacks a pad.
#[test]
fn a_connected_pad_the_live_footprint_lacks_is_found_while_planning() {
    let mut component = resistor("R1", "/sheet/existing");
    component
        .pad_nets
        .insert("3".to_string(), "SENSE".to_string());
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![component],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Noop);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.updated.planned, 0);
    assert_eq!(plan.diagnostics, Vec::new());
    assert_eq!(plan.refused_updates.len(), 1, "{:#?}", plan.refused_updates);
    let missing = &plan.refused_updates[0];
    assert_eq!(missing.code, "footprint_pad_missing");
    assert_eq!(missing.reference.as_deref(), Some("R1"));
    assert_eq!(
        missing.footprint_id.as_deref(),
        Some("Resistor_SMD:R_0603_1608Metric")
    );
    assert!(missing.message.contains("pad 3"), "{}", missing.message);
}

/// A keyed header's footprint has no pad at the key, and the schematic's
/// pin there has a no-connect flag. That pin connects nothing, so the
/// footprint is updated, and its pad nets name only the pads it has: the
/// apply sets each of them (`apply_footprint_fields` refuses any other).
/// With nothing else to change, the plan is a no-op, not an update of a
/// pad that does not exist.
#[test]
fn a_live_footprint_needs_no_pad_for_a_pin_kicad_left_unconnected() {
    let keyed = |value: &str| {
        let mut component = resistor("R1", "/sheet/existing");
        component.value = value.to_string();
        component
            .pad_nets
            .insert("3".to_string(), "unconnected-(R1-Pad3)".to_string());
        component.unconnected_pins.insert("3".to_string());
        ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        }
    };
    let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);

    let unchanged = plan_sync("netlist", &keyed("10k"), &board);
    assert_eq!(unchanged.status, PlanStatus::Noop, "{unchanged:#?}");
    assert_eq!(unchanged.refused_updates, Vec::new());

    let plan = plan_sync("netlist", &keyed("22k"), &board);
    assert_eq!(plan.status, PlanStatus::Ready, "{plan:#?}");
    assert_eq!(plan.diagnostics, Vec::new());
    assert_eq!(plan.refused_updates, Vec::new());
    assert_eq!(plan.counts.pads_reassigned.planned, 0);
    let [PlannedChange::Update {
        value, pad_nets, ..
    }] = plan.changes.as_slice()
    else {
        panic!("one update: {:#?}", plan.changes);
    };
    assert_eq!(value, "22k");
    assert_eq!(pad_nets, &board.footprints[0].pad_nets);
}

/// The netlist lists one UUID per unit of a multi-unit symbol, and a
/// footprint may be linked to any of them: KiCad's Update PCB matches it
/// by each. One linked to the second unit used to be taken for another
/// symbol's (`reference_identity_conflict`), which refused the plan.
#[test]
fn a_footprint_linked_to_a_later_unit_of_its_symbol_is_matched() {
    let mut component = resistor("U1", "/sheet/unit-a");
    component.other_unit_paths = vec!["/sheet/unit-b".to_string(), "/sheet/unit-e".to_string()];
    component.value = "LM339".to_string();
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![component],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let board = board_with(vec![board_resistor("U1", Some("/sheet/unit-b"))]);

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Ready, "{plan:#?}");
    assert_eq!(plan.diagnostics, Vec::new());
    assert_eq!(plan.without_symbols, Vec::new());
    let [PlannedChange::Update {
        kiid,
        value,
        symbol_path,
        ..
    }] = plan.changes.as_slice()
    else {
        panic!("one update: {:#?}", plan.changes);
    };
    assert_eq!(kiid, "U1-kiid");
    assert_eq!(value, "LM339");
    // Its link is kept, as every matched footprint's is.
    assert_eq!(symbol_path, "/sheet/unit-b");
}

/// Two footprints, each linked to a unit of one symbol, are not one
/// symbol's footprint: neither is picked, and the plan is refused with
/// both named.
#[test]
fn footprints_linked_to_two_units_of_one_symbol_are_a_conflict() {
    let mut component = resistor("U1", "/sheet/unit-a");
    component.other_unit_paths = vec!["/sheet/unit-b".to_string()];
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![component],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let mut second = board_resistor("U2", Some("/sheet/unit-b"));
    second.kiid = "U2-kiid".to_string();
    let board = board_with(vec![board_resistor("U1", Some("/sheet/unit-a")), second]);

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert_eq!(plan.changes, Vec::new());
    assert_eq!(plan.diagnostics.len(), 1, "{:#?}", plan.diagnostics);
    assert_eq!(plan.diagnostics[0].code, "duplicate_board_identity");
    assert_eq!(
        plan.diagnostics[0].message,
        "board footprints U1, U2 are each linked to a unit of U1"
    );
    // Neither is reported as a footprint whose symbol was removed.
    assert_eq!(plan.without_symbols, Vec::new());
}

/// The pad set is every pad KiCad sent, netted or not. The 0402 from the
/// #474 capture has pads 1 and 2. The captured mounting hole has one pad,
/// with the empty number and no net: exactly the pad `pad_nets` never
/// held, which is why it cannot answer whether a pad exists.
#[test]
fn the_live_footprint_records_every_pad_kicad_sent() {
    use copperpour_ipc::gen::kiapi;
    const RESISTOR: &[u8] = include_bytes!("../../../../tests/fixtures/issue_474_r1.ipc.bin");
    let resistor = kiapi::board::types::FootprintInstance::decode(RESISTOR)
        .expect("the checked-in KiCad IPC capture must decode");
    let mounting_hole = kiapi::board::types::FootprintInstance::decode(BOARD_ONLY_CAPTURE)
        .expect("the checked-in KiCad IPC capture must decode");

    let resistor = board_footprint_from_instance(&resistor).unwrap();
    let mounting_hole = board_footprint_from_instance(&mounting_hole).unwrap();

    assert_eq!(
        resistor.pad_numbers,
        BTreeSet::from(["1".to_string(), "2".to_string()])
    );
    assert_eq!(mounting_hole.pad_numbers, BTreeSet::from([String::new()]));
    assert!(mounting_hole.pad_nets.is_empty());
}

/// A single-part diagnostic lists that part in `references` too, so a
/// caller can read one field for every diagnostic.
#[test]
fn a_single_part_diagnostic_lists_its_part() {
    let diagnostic = conflict("duplicate_board_reference", "message".into(), Some("R1"));
    assert_eq!(diagnostic.reference.as_deref(), Some("R1"));
    assert_eq!(diagnostic.references, ["R1"]);
    assert_eq!(diagnostic.footprint_id, None);

    let board_level = conflict("stale_plan_revision", "message".into(), None);
    assert!(board_level.references.is_empty());
}

/// #657 through the served boundary. The run that found it needed two
/// unusable footprints and was told of one, with `reference: null` and no
/// footprint: every part and both footprints are named now. Those parts
/// are skipped, as KiCad's Update PCB skips a part it cannot add, and the
/// part that can be placed is still planned.
#[tokio::test]
async fn two_unusable_footprints_are_both_named_and_skipped_through_the_served_dispatch() {
    let served = ServedSync::new().await;

    let plan = served
        .dry_run(&exported_netlist(&[
            ("U1", TEXAS_VQFN, &["1", "2"]),
            ("C1", STOCK_0603, &["1", "2"]),
            ("U3", GENERIC_VQFN, &["1", "2"]),
            ("U2", TEXAS_VQFN, &["1", "2"]),
        ]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
    assert_eq!(plan["coverage"]["footprints_added"]["planned"], 1);
    assert_eq!(plan["coverage"]["skipped_footprint"]["planned"], 3);
    assert_eq!(plan["coverage"]["conflicts"]["planned"], 0);
    assert_eq!(plan["skipped_footprints"], unusable_vqfn_diagnostics());
    assert_eq!(plan["diagnostics"], serde_json::json!([]));
}

/// With nothing left that can be placed there is nothing to apply, and the
/// plan is refused whole, as it was before parts were skipped: the same
/// diagnostics, as conflicts.
#[tokio::test]
async fn a_plan_of_only_unusable_footprints_is_still_refused() {
    let served = ServedSync::new().await;

    let refused = served
        .dry_run(&exported_netlist(&[
            ("U1", TEXAS_VQFN, &["1", "2"]),
            ("U3", GENERIC_VQFN, &["1", "2"]),
            ("U2", TEXAS_VQFN, &["1", "2"]),
        ]))
        .await;

    assert_eq!(refused["status"], "conflict", "{refused:#}");
    assert_eq!(refused["changes"], serde_json::json!([]));
    assert_eq!(refused["coverage"]["footprints_added"]["planned"], 0);
    assert_eq!(refused["coverage"]["skipped_footprint"]["planned"], 0);
    assert_eq!(refused["coverage"]["conflicts"]["planned"], 2);
    assert_eq!(refused["skipped_footprints"], serde_json::json!([]));
    assert_eq!(refused["diagnostics"], unusable_vqfn_diagnostics());
}

/// A dry run said `ready` for a part whose schematic connects a pad its
/// footprint does not have, and the apply then failed its preflight. The
/// dry run finds it by name, and that part is skipped: the apply places
/// the other part alone and counts the skipped one.
#[tokio::test]
async fn a_part_missing_a_pad_is_skipped_and_the_rest_applied_through_the_served_dispatch() {
    let served = ServedSync::new().await;
    let netlist = exported_netlist(&[
        ("C1", STOCK_0603, &["1", "2", "3"]),
        ("C2", STOCK_0603, &["1", "2"]),
    ]);

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["reference"], "C2", "{plan:#}");
    assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
    let skipped = plan["skipped_footprints"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{plan:#}");
    assert_eq!(skipped[0]["code"], "footprint_pad_missing");
    assert_eq!(skipped[0]["reference"], "C1");
    assert_eq!(skipped[0]["references"], serde_json::json!(["C1"]));
    assert_eq!(skipped[0]["footprint_id"], STOCK_0603);
    assert_eq!(
        skipped[0]["message"],
        format!("the schematic connects C1 pad 3, which footprint {STOCK_0603} does not have")
    );

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(
        applied["coverage"]["skipped_footprint"],
        serde_json::json!({ "planned": 1, "applied": 1 })
    );
    // The apply answer lists the skipped part as the dry run did (#596).
    assert_eq!(
        applied["skipped_footprints"], plan["skipped_footprints"],
        "{applied:#}"
    );
    assert_eq!(applied["coverage"]["footprints_added"]["applied"], 1);
    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 1);
    assert_eq!(field_text(&sent[0].reference_field), "C2");
}

/// A footprint on the board without a pad the schematic connects used to
/// refuse the whole plan, unlike an added part without one. It is
/// skipped now, as that part is, and everything else is applied.
#[tokio::test]
async fn a_board_footprint_missing_a_connected_pad_is_skipped_and_the_rest_applied() {
    let served = ServedSync::holding(holding_r1()).await;
    let netlist = r1_netlist_with(
        "10k",
        "Resistor_SMD:R_0402",
        &format!(
            r#"    (comp (ref "C1") (value "100n") (footprint "{STOCK_0603}")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "c1-uuid"))
"#
        ),
        r#"    (net (code "3") (name "SENSE") (class "Default")
      (node (ref "R1") (pin "3") (pintype "passive"))
      (node (ref "C1") (pin "1") (pintype "passive")))
    (net (code "4") (name "C1-2") (class "Default")
      (node (ref "C1") (pin "2") (pintype "passive")))
"#,
    );

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["kind"], "add", "{plan:#}");
    assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
    assert_eq!(
        plan["skipped_footprints"],
        serde_json::json!([{
            "code": "footprint_pad_missing",
            "message": "the schematic connects R1 pad 3, which footprint Resistor_SMD:R_0402 does not have",
            "reference": "R1",
            "references": ["R1"],
            "footprint_id": "Resistor_SMD:R_0402",
        }]),
        "{plan:#}"
    );
    assert_eq!(plan["coverage"]["skipped_footprint"]["planned"], 1);

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["skipped_footprints"], plan["skipped_footprints"]);
    assert_eq!(served.sent("UpdateItems"), 0, "R1 is left as it is");
    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 1);
    assert_eq!(field_text(&sent[0].reference_field), "C1");
}

/// The real-board case: a keyed header's footprint has no pad at the
/// key, where the schematic's pin has a no-connect flag. The footprint is
/// updated, and the apply sets the nets of the pads it has.
#[tokio::test]
async fn a_keyed_board_footprint_is_updated_without_a_pad_at_its_key() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let served = ServedSync::holding(holding_r1()).await;
    let netlist = r1_netlist_with("10k", "Resistor_SMD:R_0402", "", R1_PIN_3_NO_CONNECT);

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
    assert_eq!(
        plan["skipped_footprints"],
        serde_json::json!([]),
        "{plan:#}"
    );
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
    assert_eq!(plan["changes"][0]["value"], "10k", "{plan:#}");
    assert_eq!(
        plan["changes"][0]["pad_nets"],
        serde_json::json!({ "1": "VCC", "2": "GND" }),
        "{plan:#}"
    );
    assert_eq!(
        plan["coverage"]["pads_reassigned"]["planned"], 0,
        "{plan:#}"
    );

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    assert_eq!(field_text(&held.value_field), "10k");
}

/// An added footprint without a pad for a pin wired to nothing, which
/// KiCad puts alone on an `unconnected-…` net, is placed, not skipped.
#[tokio::test]
async fn an_added_footprint_needs_no_pad_for_a_pin_kicad_left_unconnected() {
    let served = ServedSync::new().await;
    let netlist = format!(
        r#"(export
  (components
    (comp (ref "C1") (value "100n") (footprint "{STOCK_0603}")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "c1-uuid")))
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "C1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "C1") (pin "2") (pintype "passive")))
    (net (code "3") (name "unconnected-(C1-Pad3)") (class "Default")
      (node (ref "C1") (pin "3") (pintype "passive")))))
"#
    );

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["skipped_footprints"],
        serde_json::json!([]),
        "{plan:#}"
    );
    assert_eq!(plan["changes"][0]["kind"], "add", "{plan:#}");
    assert_eq!(
        plan["changes"][0]["pad_nets"],
        serde_json::json!({ "1": "VCC", "2": "GND" }),
        "{plan:#}"
    );
    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(served.sent_footprints().len(), 1);
}

/// The same for a replacement: its new footprint needs no pad for a pin
/// KiCad left unconnected.
#[tokio::test]
async fn a_replacement_needs_no_pad_for_a_pin_kicad_left_unconnected() {
    let served = ServedSync::holding(holding_r1()).await;
    let netlist = r1_netlist_with("10k", STOCK_0603, "", R1_PIN_3_NO_CONNECT);

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["skipped_footprints"],
        serde_json::json!([]),
        "{plan:#}"
    );
    assert_eq!(
        plan["changes"][0]["replace_footprint"]["to"], STOCK_0603,
        "{plan:#}"
    );
    assert_eq!(
        plan["changes"][0]["pad_nets"],
        serde_json::json!({ "1": "VCC", "2": "GND" }),
        "{plan:#}"
    );
    // Pads 1 and 2 keep their nets, and the new footprint has no pad 3.
    assert_eq!(
        plan["coverage"]["pads_reassigned"]["planned"], 0,
        "{plan:#}"
    );
    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
}

/// #789 through the served boundary. A stock 0603 capacitor the board
/// lacks is added, and what `CreateItems` receives carries the library's
/// `(attr smd)`, its description and tags, and its 3D model. Before, the
/// footprint went out with none of them, so KiCad treated the part as
/// unspecified and `export_3d`'s default left it out of the STEP.
#[tokio::test]
async fn an_added_footprint_carries_its_library_attributes_text_and_model() {
    use copperpour_ipc::gen::kiapi;

    let served = ServedSync::reading_back(Readback::AsSent).await;
    let applied = served
        .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 1);
    let footprint = &sent[0];
    assert_eq!(
        footprint.attributes.as_ref().map(|a| a.mounting_style()),
        Some(kiapi::board::types::FootprintMountingStyle::FmsSmd)
    );
    let definition = footprint.definition.as_ref().unwrap();
    let text = definition.attributes.as_ref().unwrap();
    assert!(
        text.description
            .starts_with("Capacitor SMD 0603 (1608 Metric)"),
        "{}",
        text.description
    );
    assert_eq!(text.keywords, "capacitor");
    let models: Vec<_> = definition
        .items
        .iter()
        .filter(|child| {
            copperpour_ipc::builders::any_is(child, "kiapi.board.types.Footprint3DModel")
        })
        .map(|child| kiapi::board::types::Footprint3DModel::decode(child.value.as_slice()).unwrap())
        .collect();
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].filename,
        "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"
    );
    assert!(models[0].visible);
}

/// The readback holds the board to the mounting style and model files
/// that were sent. A board that kept neither, which is what the sync used
/// to build, is reported. It is not refused: the commit has already been
/// applied, and the footprint is otherwise the one that was planned.
#[tokio::test]
async fn the_readback_names_library_data_the_board_did_not_keep() {
    let served = ServedSync::reading_back(Readback::WithoutLibraryData).await;
    let applied = served
        .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let messages: Vec<&str> = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "board_readback_differs")
        .map(|diagnostic| diagnostic["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages.len(), 2, "{applied:#}");
    assert!(
        messages[0].contains("C1: sent attributes [smd], board now has []"),
        "{}",
        messages[0]
    );
    assert!(
        messages[1].contains(
            "C1: sent 3D models [${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step], board now has []"
        ),
        "{}",
        messages[1]
    );
}

/// One attribute lost while the mounting style and the models survive,
/// which a check on those two alone passes. The flag is DNP as the
/// schematic set it, the final value sent, not the library's: the 0603's
/// library footprint is not DNP.
#[tokio::test]
async fn the_readback_names_a_single_attribute_the_board_did_not_keep() {
    let served = ServedSync::reading_back(Readback::WithoutDnp).await;
    let applied = served
        .apply(&with_dnp(
            &exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]),
            "C1",
        ))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(
        applied["diagnostics"].as_array().unwrap().len(),
        1,
        "{applied:#}"
    );
    assert_eq!(applied["diagnostics"][0]["code"], "board_readback_differs");
    let message = applied["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("C1: sent attributes [dnp, smd], board now has [smd]"),
        "{message}"
    );
    assert!(
        served.sent_footprints()[0]
            .attributes
            .as_ref()
            .unwrap()
            .do_not_populate
    );
}

/// The control for the test above: the same DNP part on a board that kept
/// everything is reported as nothing at all.
#[tokio::test]
async fn a_board_that_kept_every_attribute_reports_nothing() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let applied = served
        .apply(&with_dnp(
            &exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]),
            "C1",
        ))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
}

/// #237 through the served boundary: each added footprint carries its
/// sheet name, sheet file and footprint filters, the two channels one file
/// under two names, and the plan lists them with the addition.
#[tokio::test]
async fn an_added_footprint_carries_its_sheet() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let plan = served.dry_run(SHEET_FIELDS).await;
    let addition = &plan["changes"][1];
    assert_eq!(addition["reference"], "C2", "{plan:#}");
    assert_eq!(addition["sheet_name"], "/CH1/", "{plan:#}");
    assert_eq!(addition["sheet_file"], "channel.kicad_sch", "{plan:#}");
    assert_eq!(addition["footprint_filters"], "C_*", "{plan:#}");

    let applied = served.apply(SHEET_FIELDS).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let sheet = |reference: &str, name: &str, file: &str| {
        (
            reference.to_string(),
            name.to_string(),
            file.to_string(),
            "C_*".to_string(),
        )
    };
    assert_eq!(
        sent_sheets(&served),
        vec![
            sheet("C1", "/", "sheet_fields.kicad_sch"),
            sheet("C2", "/CH1/", "channel.kicad_sch"),
            sheet("C3", "/CH2/", "channel.kicad_sch"),
        ]
    );
}

/// A board that did not keep a footprint's sheet is reported for that
/// footprint, as any other part of it the board did not keep.
#[tokio::test]
async fn the_readback_names_a_sheet_the_board_did_not_keep() {
    let served = ServedSync::reading_back(Readback::WithoutSheet).await;
    let applied = served.apply(SHEET_FIELDS).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let messages = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic["code"], "board_readback_differs", "{applied:#}");
            diagnostic["message"].as_str().unwrap().to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 3, "{applied:#}");
    assert!(
        messages[1].contains(
            "C2: sent sheet name \"/CH1/\", sheet file \"channel.kicad_sch\", \
                 footprint filters \"C_*\", symbol units [\"A\" [\"1\", \"2\"]], board now has \
                 sheet name \"\", sheet file \"\", footprint filters \"\", symbol units \
                 [\"A\" [\"1\", \"2\"]]"
        ),
        "{}",
        messages[1]
    );
}

/// A board that did not keep a footprint's units is reported for that
/// footprint, as one that did not keep its sheet is (#682).
#[tokio::test]
async fn the_readback_names_units_the_board_did_not_keep() {
    let served = ServedSync::reading_back(Readback::WithoutSymbolUnits).await;
    let applied = served
        .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let diagnostics = applied["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{applied:#}");
    assert_eq!(diagnostics[0]["code"], "board_readback_differs");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(
        message.contains("symbol units [\"A\" [\"1\", \"2\"]], board now has")
            && message.ends_with(
                "symbol units []. No pad was invented, so this is reported rather than \
                     refused; check the footprint before relying on it."
            ),
        "{message}"
    );
}

/// The schematic names the stock 0603 for R1, which the board has as
/// KiCad's R_0402. As KiCad's Update PCB does, the sync replaces it in
/// place: same KIID, position, rotation and side, each pad's net and
/// schematic pin by number, the new footprint's custom properties instead
/// of the old library's, and the schematic's value. The next dry run has
/// nothing to do.
#[tokio::test]
async fn a_footprint_the_schematic_names_differently_is_replaced_in_place() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let before = r1_with_pins_and_an_old_property();
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![copperpour_ipc::builders::pack_any(
            &before,
            "kiapi.board.types.FootprintInstance",
        )],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    // The symbol's MPN is the board's already; the replacement drops the
    // old fields, so it has to be set again all the same.
    let netlist = r1_on(STOCK_0603).replace(
        "(tstamps \"symbol-uuid\")",
        "(tstamps \"symbol-uuid\")
      (fields (field (name \"MPN\") \"RC0402\"))",
    );

    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
    assert_eq!(
        plan["changes"][0]["replace_footprint"],
        serde_json::json!({ "from": "Resistor_SMD:R_0402", "to": STOCK_0603 }),
        "{plan:#}"
    );

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    assert_eq!(served.sent("BeginCommit"), 1);
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    let id = held.definition.as_ref().unwrap().id.clone().unwrap();
    assert_eq!(
        format!("{}:{}", id.library_nickname, id.entry_name),
        STOCK_0603
    );
    assert_eq!(held.id, before.id);
    assert_eq!(held.position, before.position);
    assert_eq!(held.orientation, before.orientation);
    assert_eq!(held.layer, before.layer);
    assert_eq!(
        held.symbol_path.as_ref().map(|path| &path.path),
        before.symbol_path.as_ref().map(|path| &path.path)
    );
    assert_eq!(field_text(&held.value_field), "10k");
    let pads = held
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
        .map(|item| {
            let pad = copperpour_ipc::gen::kiapi::board::types::Pad::decode(item.value.as_slice())
                .unwrap();
            (
                pad.number.clone(),
                pad.net.map(|net| net.name).unwrap_or_default(),
                pad.symbol_pin.map(|pin| pin.name).unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pads,
        vec![
            ("1".to_string(), "VCC".to_string(), "P1".to_string()),
            ("2".to_string(), "GND".to_string(), "P2".to_string()),
        ]
    );
    let properties = custom_property_names(&held);
    assert!(
        !properties.contains(&"Old_Library_Generator".to_string()),
        "{properties:?}"
    );
    assert!(
        properties.contains(&"KiLib_Generator".to_string()),
        "the stock 0603's property: {properties:?}"
    );
    assert!(
        properties.contains(&"MPN".to_string()),
        "the symbol's field, set again: {properties:?}"
    );
    assert_eq!(served.dry_run(&netlist).await["status"], "noop");
}

/// Found live: the library reader reads Reference and Value only in the
/// `(property …)` form, so a replacement by a footprint in the older form
/// planned `ready` and then failed its apply. The placed text now keeps
/// its position and style, as a refresh without a reset keeps it, and the
/// dry run builds each replacement, so `ready` means the apply builds it.
#[tokio::test]
async fn a_replacement_by_an_older_form_footprint_keeps_the_placed_text() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let before = FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    std::fs::write(
        served
            ._temp
            .path()
            .join("Capacitor_SMD.pretty")
            .join("R_0603_OlderForm.kicad_mod"),
        OLDER_FORM_0603,
    )
    .unwrap();
    let netlist = r1_on("Capacitor_SMD:R_0603_OlderForm");

    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    let id = held.definition.as_ref().unwrap().id.clone().unwrap();
    assert_eq!(id.entry_name, "R_0603_OlderForm");
    let position = |footprint: &FootprintInstance| {
        footprint
            .reference_field
            .as_ref()
            .and_then(|field| field.text.as_ref())
            .and_then(|text| text.text.as_ref())
            .and_then(|text| text.position)
    };
    assert_eq!(position(&held), position(&before));
}

/// A replacement whose footprint does not resolve, or that the refresh
/// cannot rebuild losslessly, is refused before anything is sent, with the
/// code the sync uses for an addition's footprint.
#[tokio::test]
async fn a_replacement_that_cannot_be_built_is_refused_before_the_commit() {
    for (footprint_id, code) in [
        ("Nope:Missing", "footprint_library_resolution_failed"),
        (TEXAS_VQFN, "unsupported_library_footprint"),
    ] {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let plan = served.dry_run(&r1_on(footprint_id)).await;
        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(plan["changes"], serde_json::json!([]), "{plan:#}");
        assert_eq!(plan["diagnostics"][0]["code"], code, "{plan:#}");
        assert_eq!(plan["diagnostics"][0]["reference"], "R1", "{plan:#}");
        assert_eq!(served.sent("BeginCommit"), 0);
    }
}

/// KiCad's Update PCB replaces with every reset on
/// (`board_netlist_updater.cpp` `replaceFootprint`), so the new
/// footprint's field text and fabrication attributes replace the placed
/// ones, where a library refresh keeps them by default (#554). R1 is given
/// a through-hole mounting style, a position-file exclusion and a
/// Datasheet of its own, none of which the schematic sets. The new
/// footprint is KiCad's stock 0603 with a Datasheet property added; it
/// replaces all three.
#[tokio::test]
async fn a_replacement_takes_the_new_footprints_text_and_attributes() {
    use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, FootprintMountingStyle};
    const LIBRARY_DATASHEET: &str = "https://example.com/library.pdf";
    let mut footprint =
        FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
    let attributes = footprint.attributes.get_or_insert_with(Default::default);
    attributes.mounting_style = FootprintMountingStyle::FmsThroughHole as i32;
    attributes.exclude_from_position_files = true;
    set_field_text(
        &mut footprint.datasheet_field,
        "Datasheet",
        "https://example.com/placed.pdf",
    );
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![copperpour_ipc::builders::pack_any(
            &footprint,
            "kiapi.board.types.FootprintInstance",
        )],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    let stock = std::str::from_utf8(include_bytes!(
        "../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
    ))
    .unwrap();
    let with_datasheet = stock
        .replacen(
            "(footprint \"C_0603_1608Metric\"",
            "(footprint \"C_0603_WithDatasheet\"",
            1,
        )
        .replacen(
            "\t(attr smd)",
            &format!(
                "\t(property \"Datasheet\" \"{LIBRARY_DATASHEET}\"\n\t\t(at 0 0 0)\n\t\t\
                     (layer \"F.Fab\")\n\t\t(hide yes)\n\t\t(effects\n\t\t\t(font\n\t\t\t\t\
                     (size 1 1)\n\t\t\t\t(thickness 0.15)\n\t\t\t)\n\t\t)\n\t)\n\t(attr smd)"
            ),
            1,
        );
    assert_ne!(with_datasheet, stock);
    std::fs::write(
        served
            ._temp
            .path()
            .join("Capacitor_SMD.pretty")
            .join("C_0603_WithDatasheet.kicad_mod"),
        with_datasheet,
    )
    .unwrap();
    let netlist = r1_on("Capacitor_SMD:C_0603_WithDatasheet");
    assert_eq!(served.dry_run(&netlist).await["status"], "ready");
    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");

    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    let attributes = held.attributes.clone().unwrap();
    assert_eq!(
        attributes.mounting_style,
        FootprintMountingStyle::FmsSmd as i32
    );
    assert!(!attributes.exclude_from_position_files);
    assert_eq!(field_text(&held.datasheet_field), LIBRARY_DATASHEET);
}

/// A replacement that only the dry run's build rejects: R1's two pads are
/// both numbered 1, one on VCC and one on GND. The stock 0603 resolves,
/// reads and keeps every connected pad number, so the preflight passes
/// it; rebuilding the footprint refuses a logical pad that carries two
/// nets. Without the build in the dry run this plan was `ready` and its
/// apply failed after the commit began.
#[tokio::test]
async fn a_replacement_only_the_build_rejects_is_refused_before_the_commit() {
    use copperpour_ipc::gen::kiapi;
    let mut footprint = kiapi::board::types::FootprintInstance::decode(
        schematic_backed_resistor().value.as_slice(),
    )
    .unwrap();
    let mut nets = [("VCC", 1), ("GND", 2)].into_iter();
    for item in &mut footprint.definition.as_mut().unwrap().items {
        if item.type_url.ends_with("kiapi.board.types.Pad") {
            let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
            let (name, code) = nets.next().expect("the capture has two pads");
            pad.number = "1".to_string();
            pad.net = Some(kiapi::board::types::Net {
                code: Some(kiapi::board::types::NetCode { value: code }),
                name: name.to_string(),
            });
            item.value = pad.encode_to_vec();
        }
    }
    assert_eq!(nets.next(), None, "both pads were renumbered");
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![copperpour_ipc::builders::pack_any(
            &footprint,
            "kiapi.board.types.FootprintInstance",
        )],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    let plan = served.dry_run(&r1_on(STOCK_0603)).await;
    assert_eq!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(plan["changes"], serde_json::json!([]), "{plan:#}");
    let diagnostic = &plan["diagnostics"][0];
    assert_eq!(
        diagnostic["code"], "unsupported_library_footprint",
        "{plan:#}"
    );
    assert_eq!(diagnostic["reference"], "R1", "{plan:#}");
    assert!(
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("carries multiple nets")),
        "the build's own refusal: {plan:#}"
    );
    assert_eq!(served.sent("BeginCommit"), 0);
}

/// A replacement that cannot be built skips its part, not the sync: C1 is
/// added beside it, and R1 stays as it was, footprint and value. (KiCad
/// keeps the old footprint there and updates the rest of it; the sync
/// leaves the whole part to a later run.)
#[tokio::test]
async fn a_replacement_that_cannot_be_built_skips_only_its_part() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    let netlist = r1_on_beside_c1(TEXAS_VQFN);

    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
    assert_eq!(plan["coverage"]["footprints_updated"]["planned"], 0);
    assert_eq!(
        plan["skipped_footprints"][0]["code"], "unsupported_library_footprint",
        "{plan:#}"
    );
    assert_eq!(plan["skipped_footprints"][0]["reference"], "R1");

    served.apply(&netlist).await;
    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 1);
    assert_eq!(field_text(&sent[0].reference_field), "C1");
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    let id = held.definition.as_ref().unwrap().id.clone().unwrap();
    assert_eq!(id.entry_name, "R_0402");
    assert_eq!(field_text(&held.value_field), "1k");
}

/// An acknowledged routed pad net change (#38) on a part that is then
/// skipped goes with the part: the apply adds C1, leaves R1's pad on its
/// net, and runs no post-apply check for a change it did not make. The
/// stand-in DRC has no report, so a check that ran would flag the apply.
#[tokio::test]
async fn an_acknowledged_change_on_a_skipped_part_is_neither_made_nor_checked() {
    let served = served_r1_under_the_gnd_pour().await;
    std::fs::remove_file(&served.drc_report).unwrap();
    let netlist = r1_on_beside_c1(TEXAS_VQFN).replace("(name \"GND\")", "(name \"NEW_NET\")");
    let refused = served.dry_run(&netlist).await;
    assert_eq!(refused["status"], "conflict", "{refused:#}");
    let acknowledged = acknowledging_each(&refused);
    assert_eq!(
        acknowledged.as_array().map(Vec::len),
        Some(1),
        "{refused:#}"
    );

    let (_, plan) = served
        .sync(
            &netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes_count"], 1, "{plan:#}");
    assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
    assert_eq!(plan["skipped_footprints"][0]["reference"], "R1", "{plan:#}");
    // R1 keeps its pad on GND, so no pad moves the pour off it; GND has
    // left the schematic, so the pour stays with KiCad's warning (#862).
    assert_eq!(
        plan["copper_net_changes"],
        serde_json::json!([]),
        "{plan:#}"
    );
    let codes = plan["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["zone_net_not_in_schematic"], "{plan:#}");

    let (is_error, applied) = served
        .sync(
            &netlist,
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
                "acknowledged_net_changes": acknowledged,
            }),
        )
        .await;

    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert!(applied.get("routed_copper").is_none(), "{applied:#}");
    assert!(applied.get("drc").is_none(), "{applied:#}");
    assert_eq!(applied["skipped_footprints"], plan["skipped_footprints"]);
    assert_eq!(held_r1_pad_2(&served).as_deref(), Some("GND"));
    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 1);
    assert_eq!(field_text(&sent[0].reference_field), "C1");
}

/// KiCad's Update PCB leaves a locked footprint's footprint as it is,
/// with a warning, and still updates the rest of it. So does the sync:
/// R1 keeps the R_0402, takes the schematic's value, and the plan notes
/// the replacement it left out without refusing anything.
#[tokio::test]
async fn a_locked_footprint_keeps_its_footprint_and_takes_the_rest() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let mut locked =
        FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
    locked.locked = copperpour_ipc::gen::kiapi::common::types::LockedState::LsLocked as i32;
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![copperpour_ipc::builders::pack_any(
            &locked,
            "kiapi.board.types.FootprintInstance",
        )],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    let netlist = r1_on(STOCK_0603);
    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["changes"][0].get("replace_footprint"),
        None,
        "{plan:#}"
    );
    assert_eq!(
        plan["diagnostics"][0]["code"], "footprint_locked_not_replaced",
        "{plan:#}"
    );
    assert_eq!(plan["coverage"]["conflicts"]["planned"], 0, "{plan:#}");

    served.apply(&netlist).await;
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    let id = held.definition.as_ref().unwrap().id.clone().unwrap();
    assert_eq!(id.entry_name, "R_0402");
    assert_eq!(field_text(&held.value_field), "10k");
}

/// The preflight checks a replacement's pads against the new footprint:
/// the stock 0603 has pads 1 and 2, so a schematic connecting pad 3 is
/// refused, and so is a board footprint whose pad 3 carries a net the
/// replacement would drop.
#[test]
fn a_replacement_keeps_every_connected_pad() {
    let (_temp, board_path) = project_with_stock_footprints();
    let kicad = copperpour_ipc::IpcKiCadVersion {
        major: 10,
        minor: 0,
        patch: 8,
        full_version: "10.0.8".to_string(),
    };
    let plan_for = |component: DesignComponent, footprint: BoardFootprint| {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![footprint]);
        let plan = plan_sync("netlist", &design, &board);
        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        prepare_replacements(&board_path, &plan, &board, &kicad)
    };
    let mut component = resistor("R1", "/sheet/existing");
    component.footprint_id = STOCK_0603.to_string();

    let (prepared, refused) = plan_for(
        component.clone(),
        board_resistor("R1", Some("/sheet/existing")),
    );
    assert!(refused.is_empty(), "{refused:?}");
    assert!(prepared.contains_key(STOCK_0603));

    let mut connects_pad_3 = component.clone();
    connects_pad_3
        .pad_nets
        .insert("3".to_string(), "EXTRA".to_string());
    let (_, refused) = plan_for(
        connects_pad_3,
        board_resistor("R1", Some("/sheet/existing")),
    );
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].code, "footprint_pad_missing");

    let mut has_pad_3 = board_resistor("R1", Some("/sheet/existing"));
    has_pad_3
        .pad_nets
        .insert("3".to_string(), "EXTRA".to_string());
    has_pad_3.pad_numbers.insert("3".to_string());
    let (_, refused) = plan_for(component, has_pad_3);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].code, "footprint_replacement_drops_connected_pad");
    assert!(
        refused[0].message.contains("pad(s) 3"),
        "{}",
        refused[0].message
    );
}

/// Update PCB's replacement reads the new footprint with the refresh's
/// reader, which takes custom pads now (#679): a part moved onto the
/// stock VQFN is prepared, and one moved onto a VQFN whose custom pads
/// have `(clearance convexhull)` is refused by name.
#[test]
fn a_replacement_with_custom_pads_is_prepared_and_convexhull_refused() {
    let (_temp, board_path) = project_with_stock_footprints();
    let kicad = copperpour_ipc::IpcKiCadVersion {
        major: 10,
        minor: 0,
        patch: 8,
        full_version: "10.0.8".to_string(),
    };
    let replace_onto = |footprint_id: &str| {
        let mut component = resistor("R1", "/sheet/existing");
        component.footprint_id = footprint_id.to_string();
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);
        let plan = plan_sync("netlist", &design, &board);
        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        prepare_replacements(&board_path, &plan, &board, &kicad)
    };

    let (prepared, refused) = replace_onto(PLACEABLE_VQFN);
    assert!(refused.is_empty(), "{refused:?}");
    assert!(prepared.contains_key(PLACEABLE_VQFN));

    let (_, refused) = replace_onto(GENERIC_VQFN);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].code, "unsupported_library_footprint");
    assert!(
        refused[0].message.contains("custom pad '1'") && refused[0].message.contains("convexhull"),
        "{}",
        refused[0].message
    );
}

/// A footprint an earlier sync placed is linked to its symbol but has no
/// sheet name, sheet file, filters or units. Its sheet alone differs from
/// the schematic's, and that is an update, as in KiCad's Update PCB: the
/// footprint keeps its place and takes the sheet, and the next dry run has
/// nothing to do.
#[tokio::test]
async fn a_footprint_without_its_sheet_is_updated_to_it() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
    let mut footprint =
        FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
    set_field_text(&mut footprint.value_field, "Value", "10k");
    assert_eq!(SymbolSheet::of(&footprint), SymbolSheet::default());
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![copperpour_ipc::builders::pack_any(
            &footprint,
            "kiapi.board.types.FootprintInstance",
        )],
        zones: Vec::new(),
        nets: vec!["VCC", "GND"],
    })
    .await;
    let netlist = r1_at_10k();

    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        plan["coverage"]["footprints_updated"]["planned"], 1,
        "{plan:#}"
    );
    assert_eq!(
        plan["coverage"]["pads_reassigned"]["planned"], 0,
        "{plan:#}"
    );
    assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
    assert_eq!(plan["changes"][0]["sheet_name"], "/Power/", "{plan:#}");

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let held = FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
    // The export names no `Sheetfile` or `ki_fp_filters` for it, so both
    // are empty, as Update PCB leaves them.
    assert_eq!(
        SymbolSheet::of(&held),
        SymbolSheet {
            sheet_name: "/Power/".to_string(),
            symbol_units: vec![unit("A", &["1", "2"])],
            ..SymbolSheet::default()
        }
    );
    assert_eq!(held.position, footprint.position);
    assert_eq!(served.dry_run(&netlist).await["status"], "noop");
}

/// #789's second increment through the served boundary. The added 0603
/// carries its library's fields: Reference and Value on the library's
/// layers with its visibility (Value on `F.Fab`, shown, not the hidden
/// silkscreen field the builder gave it), and the custom
/// `KiLib_Generator` property. Their text is the schematic's. None of the
/// library's properties carries `(unlocked yes)`, so each is kept upright,
/// and each is sent unlocked, as KiCad creates a field.
#[tokio::test]
async fn an_added_footprint_carries_its_library_fields() {
    use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
    use copperpour_ipc::gen::kiapi::common::types::LockedState;

    let served = ServedSync::reading_back(Readback::AsSent).await;
    let applied = served
        .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let sent = &served.sent_footprints()[0];
    let shown = |field: &Option<copperpour_ipc::gen::kiapi::board::types::Field>| {
        let field = field.as_ref().expect("a field");
        let text = field.text.as_ref().unwrap();
        let shown = text.text.as_ref().unwrap();
        assert!(
            shown.attributes.as_ref().unwrap().keep_upright,
            "{} is kept upright",
            field.name
        );
        (shown.text.clone(), text.layer, field.visible, text.locked)
    };
    assert_eq!(
        shown(&sent.reference_field),
        (
            "C1".to_string(),
            BoardLayer::BlFSilkS as i32,
            true,
            LockedState::LsUnlocked as i32
        )
    );
    assert_eq!(
        shown(&sent.value_field),
        (
            "part".to_string(),
            BoardLayer::BlFFab as i32,
            true,
            LockedState::LsUnlocked as i32
        )
    );
    let custom: Vec<_> = sent
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field"))
        .map(|child| {
            copperpour_ipc::gen::kiapi::board::types::Field::decode(child.value.as_slice()).unwrap()
        })
        .collect();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].name, "KiLib_Generator");
    assert_eq!(
        shown(&Some(custom[0].clone())),
        (
            "SMD_2terminal_chip_molded".to_string(),
            BoardLayer::BlFSilkS as i32,
            false,
            LockedState::LsUnlocked as i32
        )
    );
}

/// The readback holds the board to the fields that were sent. A board
/// that dropped the custom property, put Value back on the hidden
/// silkscreen field, which is what the sync used to build, and stopped
/// keeping Reference upright, which is what the sync used to send, is
/// reported field by field, after the commit.
#[tokio::test]
async fn the_readback_names_library_fields_the_board_did_not_keep() {
    let served = ServedSync::reading_back(Readback::WithoutLibraryFields).await;
    let applied = served
        .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let messages: Vec<&str> = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "board_readback_differs")
        .map(|diagnostic| diagnostic["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages.len(), 3, "{applied:#}");
    assert!(
        messages.iter().any(|message| message.contains(
            "C1: sent field KiLib_Generator as \"SMD_2terminal_chip_molded\" on F.SilkS, hidden, kept upright, board now has none"
        )),
        "{messages:#?}"
    );
    assert!(
        messages.iter().any(|message| message.contains(
            "C1: sent field Value as \"part\" on F.Fab, visible, kept upright, board now has \"part\" on F.SilkS, hidden, kept upright"
        )),
        "{messages:#?}"
    );
    assert!(
        messages.iter().any(|message| message.contains(
            "C1: sent field Reference as \"C1\" on F.SilkS, visible, kept upright, board now has \"C1\" on F.SilkS, visible, not kept upright"
        )),
        "{messages:#?}"
    );
}

/// #788 through the served boundary: the dry run lists the symbol's fields
/// and the added 0603 carries them. `KiLib_Generator`, which the library
/// has too, keeps the library's layer and visibility; `LCSC`, Datasheet and
/// Description arrive hidden on F.Fab; Footprint is not carried.
#[tokio::test]
async fn an_added_footprint_carries_its_symbols_fields_over_its_librarys() {
    use copperpour_ipc::gen::kiapi::board::types::{BoardLayer, Field};

    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]).replacen(
        "(value \"part\")",
        "(value \"part\")\n      (fields\n        (field (name \"LCSC\") \"C788001\")\n        (field (name \"KiLib_Generator\") \"from the symbol\")\n        (field (name \"Footprint\") \"Capacitor_SMD:C_0603_1608Metric\")\n        (field (name \"Datasheet\"))\n        (field (name \"Description\") \"Unpolarized capacitor\"))",
        1,
    );

    let plan = served.dry_run(&netlist).await;
    assert_eq!(
        plan["changes"][0]["fields"],
        serde_json::json!([
            {"name": "LCSC", "value": "C788001"},
            {"name": "KiLib_Generator", "value": "from the symbol"},
            {"name": "Datasheet", "value": ""},
            {"name": "Description", "value": "Unpolarized capacitor"}
        ]),
        "{plan:#}"
    );

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let sent = &served.sent_footprints()[0];
    let shown = |field: &Field| {
        let text = field.text.as_ref().unwrap();
        (
            text.text.as_ref().unwrap().text.clone(),
            text.layer,
            field.visible,
        )
    };
    let hidden_on_fab = |value: &str| (value.to_string(), BoardLayer::BlFFab as i32, false);
    assert_eq!(
        shown(sent.datasheet_field.as_ref().unwrap()),
        hidden_on_fab("")
    );
    assert_eq!(
        shown(sent.description_field.as_ref().unwrap()),
        hidden_on_fab("Unpolarized capacitor")
    );
    let custom = field_items(sent);
    assert_eq!(
        custom.keys().collect::<Vec<_>>(),
        ["KiLib_Generator", "LCSC"]
    );
    assert_eq!(shown(&custom["LCSC"]), hidden_on_fab("C788001"));
    // The library's own field, as #820's reader gives it, with the
    // symbol's text.
    let library = prepare_footprint_source(include_str!(
        "../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
    ))
    .unwrap()
    .library_fields
    .unwrap()
    .custom
    .into_iter()
    .find(|field| field.name == "KiLib_Generator")
    .expect("the library's generator field");
    let (text, layer, visible) = shown(&custom["KiLib_Generator"]);
    assert_eq!(text, "from the symbol");
    assert_eq!(
        (layer, visible),
        (library.text.as_ref().unwrap().layer, library.visible)
    );
}

/// A footprint whose properties the reader refuses is still placed, with
/// the fields every footprint gets, and the plan says which footprint and
/// part and why. Presentation metadata never refuses a part (#789).
#[tokio::test]
async fn a_footprint_whose_properties_cannot_be_read_is_placed_and_reported() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    // The stock 0603 with its Reference property written twice, the shape
    // of the 15 stock footprints the reader refuses.
    let library = served.board.parent().unwrap().join("Capacitor_SMD.pretty");
    let source = include_str!("../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
    let twice = source.replacen(
        "\t(property \"KiLib_Generator\"",
        "\t(property \"Reference\" \"REF**\"\n\t\t(at 0 0 0)\n\t\t(layer \"F.Fab\")\n\t\t(effects (font (size 1 1) (thickness 0.15)))\n\t)\n\t(property \"KiLib_Generator\"",
        1,
    );
    assert_ne!(twice, source);
    std::fs::write(library.join("C_0603_TwoReferences.kicad_mod"), twice).unwrap();
    let footprint_id = "Capacitor_SMD:C_0603_TwoReferences";

    let plan = served
        .dry_run(&exported_netlist(&[("C1", footprint_id, &["1", "2"])]))
        .await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes"].as_array().unwrap().len(), 1, "{plan:#}");
    let diagnostics = plan["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{plan:#}");
    assert_eq!(diagnostics[0]["code"], "library_fields_not_carried");
    assert_eq!(diagnostics[0]["footprint_id"], footprint_id);
    assert_eq!(diagnostics[0]["references"], serde_json::json!(["C1"]));
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(message.contains("more than once"), "{message}");
}

/// On KiCad 10.0.7+, an added footprint's pads go out with the library's
/// fabrication properties, as KiCad 10.0.8 itself reports the same stock
/// footprint placed from its library: eight `PFP_HEATSINK` pads numbered
/// 15 and `PFP_NONE` on the other 25.
#[tokio::test]
async fn an_added_footprint_carries_its_pad_fabrication_properties() {
    use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, PadFabricationProperty};

    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);

    let applied = served.apply(&netlist).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let kicad = FootprintInstance::decode(
        include_bytes!("../../../../tests/fixtures/issue_283_u1.ipc.bin").as_slice(),
    )
    .unwrap();
    let expected = pad_properties_of(&kicad);
    let heatsink = PadFabricationProperty::PfpHeatsink as i32;
    assert_eq!(
        expected
            .iter()
            .filter(|(_, property)| *property == heatsink)
            .count(),
        8
    );
    assert_eq!(pad_properties_of(&served.sent_footprints()[0]), expected);
    // Asked once per call: the dry run and the apply.
    assert_eq!(served.sent("GetVersion"), 2);
}

/// KiCad before 10.0.7 cannot take the property over its API, so the
/// footprint is placed without it, and the dry run and the apply both
/// name the footprint, the part, the pads and the KiCad version.
#[tokio::test]
async fn pad_fabrication_properties_an_older_kicad_cannot_take_are_reported() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    served.report_kicad_version(10, 0, 6);
    let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);

    let plan = served.dry_run(&netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    let applied = served.apply(&netlist).await;

    for response in [&plan, &applied] {
        let diagnostics = response["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 2, "{response:#}");
        assert_eq!(diagnostics[0]["code"], "not_carried");
        assert_eq!(diagnostics[0]["footprint_id"], THERMAL_DFN);
        assert_eq!(diagnostics[0]["references"], serde_json::json!(["U1"]));
        let message = diagnostics[0]["message"].as_str().unwrap();
        for part in ["pad 15: heatsink (8 pads)", "10.0.7", "10.0.6", "U1"] {
            assert!(message.contains(part), "{part} missing from: {message}");
        }
        // Nor can it take the symbol units (#682).
        assert_eq!(diagnostics[1]["code"], "symbol_units_not_carried");
    }
    assert_eq!(applied["status"], "applied", "{applied:#}");
    // Sent as before: KiCad 10.0.6 has no field to read.
    assert!(pad_properties_of(&served.sent_footprints()[0])
        .iter()
        .all(|(_, property)| *property == 0));
}

/// KiCad 10.0.7+ reports `PFP_NONE` for a pad sent without a property,
/// which is no difference: with and without pad properties.
#[tokio::test]
async fn a_pad_reported_without_a_property_is_no_readback_difference() {
    for (reference, footprint_id, pads) in [
        ("C1", STOCK_0603, ["1", "2"]),
        ("U1", THERMAL_DFN, ["1", "15"]),
    ] {
        let served = ServedSync::reading_back(Readback::UnknownPadPropertiesAsNone).await;
        let applied = served
            .apply(&exported_netlist(&[(reference, footprint_id, &pads)]))
            .await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    }
}

/// A plan made on one side of KiCad 10.0.7 is stale on the other: the
/// apply would send something the dry run didn't describe.
#[tokio::test]
async fn a_plan_is_stale_across_the_kicad_version_that_carries_pad_properties() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);
    for (planned_on, applied_on) in [(6, 8), (8, 6)] {
        served.report_kicad_version(10, 0, planned_on);
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        served.report_kicad_version(10, 0, applied_on);
        let (_, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                }),
            )
            .await;
        assert_eq!(applied["status"], "conflict", "{applied:#}");
        let codes = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"stale_plan_revision"), "{applied:#}");
    }
    assert!(served.sent_footprints().is_empty());
}

/// The readback names only the pads that differ, so a BGA's one lost
/// pad isn't hidden behind the same first entries of two long lists.
#[test]
fn the_readback_names_only_the_pads_whose_property_differs() {
    let bga = |number: &str| (number.to_string(), PadFabricationProperty::Bga);
    let mut sent = (1..=20).map(|n| bga(&format!("A{n}"))).collect::<Vec<_>>();
    sent.push(bga("A1"));
    sent.sort();
    let kept = sent
        .iter()
        .filter(|(number, _)| number != "A17")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        sorted_difference(&sent, &kept),
        [("A17", PadFabricationProperty::Bga)]
    );
    assert!(sorted_difference(&kept, &sent).is_empty());
    // A repeated number counts: one of the two A1 pads lost it.
    let one_a1 = sent[1..].to_vec();
    assert_eq!(sent[0].0, "A1");
    assert_eq!(
        sorted_difference(&sent, &one_a1),
        [("A1", PadFabricationProperty::Bga)]
    );
}

/// A sync whose footprints have no pad property never asks KiCad its
/// version, so it sends what it sent before #149. Nor does one whose
/// netlist has no units (#682); KiCad 10 writes them for every component,
/// so they are removed here.
#[tokio::test]
async fn a_footprint_without_pad_properties_never_asks_the_kicad_version() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);
    let without_units = netlist.replace(
        "\n      (units (unit (name \"A\") (pins (pin (num \"1\"))(pin (num \"2\")))))",
        "",
    );
    assert_ne!(without_units, netlist);

    let applied = served.apply(&without_units).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(served.sent("GetVersion"), 0);
}

/// A board that comes back without the pad properties that were sent is
/// reported, as a lost attribute or model is.
#[tokio::test]
async fn pad_fabrication_properties_the_board_lost_are_reported() {
    let served = ServedSync::reading_back(Readback::WithoutPadProperties).await;

    let applied = served
        .apply(&exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]))
        .await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let diagnostics = applied["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{applied:#}");
    assert_eq!(diagnostics[0]["code"], "board_readback_differs");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(
        message.contains(
            "U1: pad fabrication properties sent but not on the board \
                 [pad 15: heatsink (8 pads)], on the board but not sent []"
        ),
        "{message}"
    );
}

/// The apply reports the same `staging` evidence as the dry run, owed
/// from #799: both read it off the plan made against the snapshot.
#[tokio::test]
async fn the_apply_reports_staging_as_the_dry_run_does() {
    let served = ServedSync::refusing(None, kicad_10_refusals()).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);

    let planned = served.dry_run(&netlist).await;
    let applied = served.apply(&netlist).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let expected = serde_json::json!({
        "basis": "no_measured_geometry",
        "unavailable_item_classes": [refused_class("tables"), refused_class("generators")],
    });
    assert_eq!(planned["staging"], expected, "{planned:#}");
    assert_eq!(applied["staging"], expected, "{applied:#}");
}

/// The schematic decides DNP, over whatever the library footprint says,
/// as KiCad's own Update PCB from Schematic does. The library data is
/// applied first and the schematic's fields second, so the order is what
/// this pins: both directions, on the KiCad-written 0603.
#[test]
fn the_schematic_decides_dnp_over_the_library() {
    use copperpour_ipc::gen::kiapi;

    let mut part = prepare_footprint_source(include_str!(
        "../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
    ))
    .unwrap();
    let addition = |dnp: bool| PlannedChange::Add {
        reference: "C1".into(),
        value: "100n".into(),
        footprint_id: STOCK_0603.into(),
        symbol_path: "/c1-uuid".into(),
        dnp,
        exclude_from_bom: false,
        pad_nets: BTreeMap::new(),
        unconnected_pins: BTreeSet::new(),
        position: Point { x: 10.0, y: 10.0 },
        fields: Vec::new(),
        sheet: SymbolSheet::default(),
    };
    let built_dnp = |part: &PreparedFootprint, dnp: bool| {
        let item = build_added_footprint(
            &addition(dnp),
            part,
            &BTreeMap::new(),
            CarriedByKiCad::default(),
        )
        .unwrap();
        let footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let attributes = footprint.attributes.unwrap();
        assert_eq!(
            attributes.mounting_style(),
            kiapi::board::types::FootprintMountingStyle::FmsSmd,
            "the library's attributes are still applied"
        );
        attributes.do_not_populate
    };

    // The stock 0603 is not DNP in the library; the schematic says it is.
    assert!(!part.attributes.do_not_populate);
    assert!(built_dnp(&part, true));

    // A library footprint marked DNP; the schematic says it is fitted.
    part.attributes.do_not_populate = true;
    assert!(!built_dnp(&part, false));
}

/// A visible custom property travels as its typed field only. Drawing it as
/// plain text as well, which the sync did when it carried no fields, would
/// show it twice. The text drawing stays the fallback for a footprint whose
/// properties cannot be read.
#[test]
fn a_visible_custom_property_is_carried_once() {
    let drawn_text = |part: &PreparedFootprint| {
        part.graphics
            .iter()
            .filter(|graphic| {
                matches!(graphic, copperpour_ipc::IpcGraphicDefinition::Text { text, .. }
                    if text == "SMD_2terminal_chip_molded")
            })
            .count()
    };
    // The 0603's KiLib_Generator property, shown instead of hidden.
    let source = include_str!("../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
    let visible = source.replacen(
        "\t\t(layer \"F.SilkS\")\n\t\t(hide yes)",
        "\t\t(layer \"F.SilkS\")",
        1,
    );
    assert_ne!(visible, source);

    let part = prepare_footprint_source(&visible).unwrap();
    let fields = part.library_fields.as_ref().expect("readable properties");
    assert!(fields.custom[0].visible);
    assert_eq!(drawn_text(&part), 0, "carried as a field, not drawn too");

    // Unreadable properties: nothing is carried, so it is drawn as before.
    let twice = visible.replacen(
        "\t(property \"KiLib_Generator\"",
        "\t(property \"Value\" \"again\"\n\t\t(at 0 0 0)\n\t\t(layer \"F.Fab\")\n\t\t(effects (font (size 1 1) (thickness 0.15)))\n\t)\n\t(property \"KiLib_Generator\"",
        1,
    );
    let fallback = prepare_footprint_source(&twice).unwrap();
    assert!(fallback.library_fields.is_err());
    assert_eq!(drawn_text(&fallback), 1, "drawn when it cannot be carried");
}

/// A model the shared reader refuses refuses the footprint, under the code
/// the sync already uses for a footprint it cannot place, rather than
/// placing it without the model.
#[test]
fn a_library_model_the_reader_refuses_refuses_the_footprint() {
    let source = include_str!("../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod")
        .replacen("(offset", "(origin", 1);

    let refusal = prepare_footprint_source(&source).unwrap_err();

    assert_eq!(refusal.0, "unsupported_library_footprint");
    assert!(
        refusal.1.contains("3D model clause 'origin'"),
        "{}",
        refusal.1
    );
}

#[test]
fn what_a_footprint_owns_is_read_from_the_board_kicad_wrote() {
    let owned = not_carried_by_kiid(FOOTPRINT_CHILDREN).unwrap();

    assert_eq!(
        owned[OWNS_ALL_THREE],
        NotCarried {
            groups: 1,
            points: 1,
            variants: vec!["NoTimer".to_string()],
        }
    );
    assert_eq!(owned[OWNS_NONE], NotCarried::default());
}

/// Whichever footprint comes last, neither hides what the other owns.
#[test]
fn a_kiid_written_twice_owns_what_both_footprints_own() {
    let owned = not_carried_by_kiid(
        "(kicad_pcb\n\
             (footprint \"A\" (uuid \"twice\") (group \"\" (uuid \"g\") (members \"m\")))\n\
             (footprint \"B\" (uuid \"twice\") (point (at 0 0) (uuid \"p\"))))",
    )
    .unwrap();

    assert_eq!(
        owned["twice"],
        NotCarried {
            groups: 1,
            points: 1,
            variants: Vec::new(),
        }
    );
}

#[tokio::test]
async fn an_update_that_would_drop_what_a_footprint_owns_is_refused_before_the_commit() {
    let served = served_r1(OWNS_ALL_THREE, "1k").await;

    let (plan, applied) = dry_run_then_apply(&served).await;

    assert_refused(&plan, REFUSED_FOR);
    assert_refused(&applied, REFUSED_FOR);
    assert_eq!(served.sent("BeginCommit"), 0, "refused before the commit");
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// The control: the same update of a footprint that owns none of them,
/// read from the same board, is applied.
#[tokio::test]
async fn a_footprint_that_owns_none_of_them_is_still_updated() {
    use copperpour_ipc::gen::kiapi;

    let served = served_r1(OWNS_NONE, "1k").await;

    let (plan, applied) = dry_run_then_apply(&served).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_updated"]["applied"], 1);
    let held = served.held.lock().unwrap();
    let instance =
        kiapi::board::types::FootprintInstance::decode(held[0].value.as_slice()).unwrap();
    let r1 = board_footprint_from_instance(&instance).unwrap();
    assert_eq!((r1.kiid.as_str(), r1.value.as_str()), (OWNS_NONE, "10k"));
}

/// The apply reads the board again: a footprint that gains a group, a
/// point and a variant after a `ready` dry run is refused then.
#[tokio::test]
async fn a_footprint_that_gains_them_after_the_dry_run_is_refused_at_apply() {
    let served = served_r1(OWNS_NONE, "1k").await;
    let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    // The same board with the two footprints' KIIDs swapped.
    served.hold_board_text(
        &FOOTPRINT_CHILDREN
            .replace(OWNS_NONE, "\u{0}")
            .replace(OWNS_ALL_THREE, OWNS_NONE)
            .replace('\u{0}', OWNS_ALL_THREE),
    );
    let (_, applied) = served
        .call(serde_json::json!({
            "schematic": served.schematic.to_string_lossy(),
            "board": served.board.to_string_lossy(),
            "dry_run": false,
            "expected_plan_revision": plan["plan_revision"],
        }))
        .await;

    assert_refused(&applied, REFUSED_FOR);
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// The board text is read for updates only: R1 already holds 10k.
#[tokio::test]
async fn a_plan_without_updates_does_not_read_the_board() {
    let served = served_r1(OWNS_ALL_THREE, "10k").await;

    let (_, plan) = served.dry_run_result(&r1_at_10k()).await;

    assert_eq!(plan["status"], "noop", "{plan:#}");
    assert_eq!(served.sent("SaveDocumentToString"), 0);
}

/// A footprint the board text does not hold is not taken to own nothing.
#[tokio::test]
async fn a_footprint_missing_from_the_board_kicad_wrote_is_refused() {
    let served = served_r1("00000000-0000-0000-0000-000000000836", "1k").await;

    let (plan, _) = dry_run_then_apply(&served).await;

    assert_refused(
        &plan,
        "R1 (00000000-0000-0000-0000-000000000836) is not in the board",
    );
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// Nor is a footprint on a board text that cannot be read.
#[tokio::test]
async fn a_board_text_that_does_not_parse_refuses_each_update() {
    let served = served_r1(OWNS_NONE, "1k").await;
    served.hold_board_text("(kicad_pcb (footprint");

    let (plan, _) = dry_run_then_apply(&served).await;

    assert_refused(&plan, "what an update of R1 would drop is unknown");
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// A board text that cannot be read, or that lacks a footprint the plan
/// updates, says nothing about the part: the plan is refused whole, as
/// before parts were skipped, and C1's addition beside R1's update is not
/// planned alone.
#[tokio::test]
async fn an_unknown_board_text_refuses_the_whole_plan_not_each_update() {
    for (board_text, because) in [
        (
            "(kicad_pcb (footprint",
            "what an update of R1 would drop is unknown",
        ),
        ("(kicad_pcb (version 20241229))", "R1 ("),
    ] {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        served.hold_board_text(board_text);

        let plan = served
            .dry_run(&r1_on_beside_c1("Resistor_SMD:R_0402"))
            .await;

        assert_refused(&plan, because);
        assert_eq!(
            plan["skipped_footprints"],
            serde_json::json!([]),
            "{plan:#}"
        );
        assert_eq!(
            plan["coverage"]["footprints_added"]["planned"], 0,
            "{plan:#}"
        );
    }
}

/// #237's first sync of a board an earlier sync built: R1 owns a group,
/// a point and a variant and lacks only its sheet, so it leaves the plan
/// with a diagnostic that refuses nothing, and R2's value change applies.
#[tokio::test]
async fn a_footprint_that_owns_them_and_lacks_only_its_sheet_is_left_out() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;

    let r1 = captured_resistor(OWNS_ALL_THREE, "R1", "symbol-uuid", "10k", "");
    let r2 = captured_resistor(OWNS_NONE, "R2", "symbol-uuid-2", "1k", "/Power/");
    let served = served_children(vec![r1.clone(), r2]).await;

    let plan = served.dry_run(TWO_RESISTORS).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["coverage"]["footprints_updated"]["planned"], 1);
    let changes = plan["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{plan:#}");
    assert_eq!(changes[0]["reference"], "R2");
    let diagnostics = plan["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{plan:#}");
    assert_eq!(diagnostics[0]["code"], "footprint_sheet_not_updated");
    assert_eq!(diagnostics[0]["reference"], "R1");
    assert_eq!(diagnostics[0]["footprint_id"], "Resistor_SMD:R_0402");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "R1 differs from the schematic only in its sheet name, sheet file, \
                 footprint filters or symbol-unit mapping, and owns 1 group, 1 point and \
                 variant 'NoTimer'"
        ),
        "{message}"
    );

    let applied = served.apply(TWO_RESISTORS).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["footprints_updated"]["applied"], 1);
    assert_eq!(applied["diagnostics"], plan["diagnostics"], "{applied:#}");
    assert_eq!(served.sent("UpdateItems"), 1);
    let held = served.held.lock().unwrap().clone();
    let held_as = |kiid: &str| {
        held.iter()
            .find(|item| footprint_kiid(item) == kiid)
            .unwrap()
            .clone()
    };
    assert_eq!(held_as(OWNS_ALL_THREE), r1, "R1 is left as it was");
    let r2 = FootprintInstance::decode(held_as(OWNS_NONE).value.as_slice()).unwrap();
    assert_eq!(board_footprint_from_instance(&r2).unwrap().value, "10k");
}

/// With nothing else to change, the plan is a no-op that still names R1.
#[tokio::test]
async fn a_plan_whose_only_update_is_left_out_is_a_noop() {
    let served = served_children(vec![captured_resistor(
        OWNS_ALL_THREE,
        "R1",
        "symbol-uuid",
        "10k",
        "",
    )])
    .await;

    let (plan, applied) = dry_run_then_apply(&served).await;

    for body in [&plan, &applied] {
        assert_eq!(body["status"], "noop", "{body:#}");
        assert_eq!(body["changes"], serde_json::json!([]));
        assert_eq!(body["coverage"]["footprints_updated"]["planned"], 0);
        assert_eq!(
            body["diagnostics"][0]["code"],
            "footprint_sheet_not_updated"
        );
    }
    assert_eq!(served.sent("BeginCommit"), 0);
}

/// #682: a footprint an earlier sync placed has its sheet but not its
/// symbol's units, or has them in another order. Either alone is an
/// update, as KiCad's Update PCB makes it (`updateComponentUnits`
/// compares them in order): the footprint keeps its place and takes the
/// units, and the next dry run has nothing to do.
#[tokio::test]
async fn a_footprint_without_its_units_is_updated_to_them() {
    use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, FootprintUnitInfo};

    let reordered = vec![FootprintUnitInfo {
        name: "A".to_string(),
        pin_numbers: vec!["2".to_string(), "1".to_string()],
    }];
    for held_units in [Vec::new(), reordered] {
        let mut footprint = FootprintInstance::decode(
            captured_resistor(OWNS_NONE, "R1", "symbol-uuid", "10k", "/Power/")
                .value
                .as_slice(),
        )
        .unwrap();
        footprint.symbol_units = held_units;
        let served = served_children(vec![copperpour_ipc::builders::pack_any(
            &footprint,
            "kiapi.board.types.FootprintInstance",
        )])
        .await;
        let netlist = r1_at_10k();

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["coverage"]["footprints_updated"]["planned"], 1);
        assert_eq!(plan["coverage"]["pads_reassigned"]["planned"], 0);
        assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
        assert_eq!(
            plan["changes"][0]["symbol_units"],
            serde_json::json!([{ "name": "A", "pin_numbers": ["1", "2"] }]),
            "{plan:#}"
        );

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        assert_eq!(
            SymbolSheet::of(&held).symbol_units,
            vec![unit("A", &["1", "2"])]
        );
        assert_eq!(held.position, footprint.position);
        assert_eq!(served.dry_run(&netlist).await["status"], "noop");
    }
}

/// A footprint that owns a group, a point and a variant and lacks only
/// its units is left out, as one that lacks only its sheet is (#237).
#[tokio::test]
async fn a_footprint_that_owns_them_and_lacks_only_its_units_is_left_out() {
    let served = served_children(vec![captured_resistor(
        OWNS_ALL_THREE,
        "R1",
        "symbol-uuid",
        "10k",
        "/Power/",
    )])
    .await;

    let plan = served.dry_run(&r1_at_10k()).await;

    assert_eq!(plan["status"], "noop", "{plan:#}");
    assert_eq!(
        plan["diagnostics"][0]["code"],
        "footprint_sheet_not_updated"
    );
    assert_eq!(plan["diagnostics"][0]["reference"], "R1");
}

/// Each added footprint carries its symbol's units, and the plan lists
/// them with the addition (#682).
#[tokio::test]
async fn an_added_footprint_carries_its_symbol_units() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);

    let plan = served.dry_run(&netlist).await;
    assert_eq!(
        plan["changes"][0]["symbol_units"],
        serde_json::json!([{ "name": "A", "pin_numbers": ["1", "2"] }]),
        "{plan:#}"
    );
    let applied = served.apply(&netlist).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    assert_eq!(
        SymbolSheet::of(&served.sent_footprints()[0]).symbol_units,
        vec![unit("A", &["1", "2"])]
    );
}

/// KiCad before 10.0.8 neither serves a footprint's units nor takes them
/// (#682). The sync is as before: a footprint that lacks only its units
/// plans nothing, and a footprint added or updated is sent without them,
/// each with a note that refuses nothing.
#[tokio::test]
async fn symbol_units_an_older_kicad_cannot_take_are_noted() {
    use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;

    let unchanged = served_children(vec![captured_resistor(
        OWNS_NONE,
        "R1",
        "symbol-uuid",
        "10k",
        "/Power/",
    )])
    .await;
    unchanged.report_kicad_version(10, 0, 7);
    let plan = unchanged.dry_run(&r1_at_10k()).await;
    assert_eq!(plan["status"], "noop", "{plan:#}");
    assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");

    let updated = served_children(vec![captured_resistor(
        OWNS_NONE,
        "R1",
        "symbol-uuid",
        "1k",
        "/Power/",
    )])
    .await;
    updated.report_kicad_version(10, 0, 7);
    let added = ServedSync::reading_back(Readback::AsSent).await;
    added.report_kicad_version(10, 0, 7);
    for (served, netlist, done) in [
        (&updated, r1_at_10k(), "updated"),
        (
            &added,
            exported_netlist(&[("R1", STOCK_0603, &["1", "2"])]),
            "added",
        ),
    ] {
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes"][0]["symbol_units"], serde_json::json!([]));
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        for body in [&plan, &applied] {
            let diagnostics = body["diagnostics"].as_array().unwrap();
            assert_eq!(diagnostics.len(), 1, "{body:#}");
            assert_eq!(diagnostics[0]["code"], "symbol_units_not_carried");
            assert_eq!(diagnostics[0]["reference"], "R1");
            let message = diagnostics[0]["message"].as_str().unwrap();
            for part in [
                &format!("R1 is {done} without its symbol-unit mapping"),
                "10.0.8",
                "10.0.7",
                "TROUBLESHOOTING.md#footprint-unit-metadata-disappears",
            ] {
                assert!(message.contains(part), "{part} missing from: {message}");
            }
        }
    }
    let held = FootprintInstance::decode(updated.held.lock().unwrap()[0].value.as_slice()).unwrap();
    assert!(held.symbol_units.is_empty());
    assert!(added.sent_footprints()[0].symbol_units.is_empty());
}

/// A plan made on one side of KiCad 10.0.8 is stale on the other: the
/// apply would send units the dry run didn't describe, or leave out ones
/// it did.
#[tokio::test]
async fn a_plan_is_stale_across_the_kicad_version_that_carries_symbol_units() {
    let served = ServedSync::reading_back(Readback::AsSent).await;
    let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);
    for (planned_on, applied_on) in [(7, 8), (8, 7)] {
        served.report_kicad_version(10, 0, planned_on);
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        served.report_kicad_version(10, 0, applied_on);
        let (_, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                }),
            )
            .await;
        assert_eq!(applied["status"], "conflict", "{applied:#}");
        let codes = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"stale_plan_revision"), "{applied:#}");
    }
    assert!(served.sent_footprints().is_empty());
}

/// The dry run planned R1's sheet; it owns a group, a point and a variant
/// by the apply, so the apply would leave it out, and its plan is stale.
#[tokio::test]
async fn a_footprint_that_gains_them_after_a_sheet_only_dry_run_makes_the_plan_stale() {
    let served = served_children(vec![captured_resistor(
        OWNS_NONE,
        "R1",
        "symbol-uuid",
        "10k",
        "",
    )])
    .await;
    let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    // The same board with the two footprints' KIIDs swapped.
    served.hold_board_text(
        &FOOTPRINT_CHILDREN
            .replace(OWNS_NONE, "\u{0}")
            .replace(OWNS_ALL_THREE, OWNS_NONE)
            .replace('\u{0}', OWNS_ALL_THREE),
    );
    let (_, applied) = served
        .call(serde_json::json!({
            "schematic": served.schematic.to_string_lossy(),
            "board": served.board.to_string_lossy(),
            "dry_run": false,
            "expected_plan_revision": plan["plan_revision"],
        }))
        .await;

    assert_eq!(applied["status"], "conflict", "{applied:#}");
    let codes = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"stale_plan_revision"), "{applied:#}");
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// A plan another update refuses lists only its conflicts: R2 is missing
/// from the board KiCad wrote, and R1, left out, is not named.
#[tokio::test]
async fn a_refused_plan_lists_only_its_conflicts() {
    let missing = "00000000-0000-0000-0000-000000000836";
    let served = served_children(vec![
        captured_resistor(OWNS_ALL_THREE, "R1", "symbol-uuid", "10k", ""),
        captured_resistor(missing, "R2", "symbol-uuid-2", "1k", "/Power/"),
    ])
    .await;

    let plan = served.dry_run(TWO_RESISTORS).await;

    assert_eq!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(plan["coverage"]["conflicts"]["planned"], 1, "{plan:#}");
    let diagnostics = plan["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{plan:#}");
    assert_eq!(diagnostics[0]["code"], "footprint_children_not_carried");
    assert_eq!(diagnostics[0]["reference"], "R2");
}

/// A footprint that lacks its sheet and differs in anything else as well
/// is refused as before.
#[tokio::test]
async fn a_footprint_that_owns_them_and_lacks_its_sheet_and_value_is_refused() {
    let served = served_children(vec![captured_resistor(
        OWNS_ALL_THREE,
        "R1",
        "symbol-uuid",
        "1k",
        "",
    )])
    .await;

    let (plan, applied) = dry_run_then_apply(&served).await;

    assert_refused(&plan, REFUSED_FOR);
    assert_refused(&applied, REFUSED_FOR);
    assert_eq!(served.sent("UpdateItems"), 0);
}

/// Added footprints in `update_pcb_from_schematic` carry pad zone
/// settings and reference points on KiCad 10.0.7+, and report
/// `not_carried` on KiCad < 10.0.7 or when the footprint has clauses
/// typed placement leaves out (#318).
#[tokio::test]
async fn added_footprint_carries_zone_settings_and_reference_point_and_reports_not_carried() {
    use copperpour_ipc::builders::any_is;
    use copperpour_ipc::gen::kiapi::board::types::{Pad, ReferencePoint, ZoneConnectionStyle};

    let served = ServedSync::reading_back(Readback::AsSent).await;
    let project_dir = served.board.parent().unwrap();
    let pretty = project_dir.join("Test_318.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    std::fs::write(
        pretty.join("WSON6.kicad_mod"),
        include_str!("../../../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
    )
    .unwrap();
    std::fs::write(
        pretty.join("DIP8.kicad_mod"),
        include_str!("../../../../tests/fixtures/issue_283_dip8_kicad10.kicad_mod"),
    )
    .unwrap();
    std::fs::write(
        pretty.join("WithZone.kicad_mod"),
        r#"(footprint "WithZone"
  (version 20241229)
  (generator "pcbnew")
  (layer "F.Cu")
  (property "Reference" "REF**" (at 0 -2 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (property "Value" "WithZone" (at 0 2 0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (zone_connect 2))
  (zone (net 0) (net_name "") (layer "F.Cu"))
)"#,
    )
    .unwrap();
    let fp_table = project_dir.join("fp-lib-table");
    let existing_table = std::fs::read_to_string(&fp_table)
        .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
    let updated_table = existing_table.replacen(
        "\n)",
        "\n  (lib (name \"Test_318\")(type \"KiCad\")(uri \"${KIPRJMOD}/Test_318.pretty\")(options \"\")(descr \"\"))\n)",
        1,
    );
    std::fs::write(&fp_table, updated_table).unwrap();

    let netlist = exported_netlist(&[
        ("U1", "Test_318:WSON6", &["1", "7"]),
        ("U2", "Test_318:DIP8", &["1", "2"]),
        ("U3", "Test_318:WithZone", &["1"]),
    ]);

    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let diagnostics = applied["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{applied:#}");
    assert_eq!(diagnostics[0]["code"], "not_carried");
    assert_eq!(diagnostics[0]["footprint_id"], "Test_318:WithZone");
    assert!(diagnostics[0]["message"].as_str().unwrap().contains("zone"));

    let sent = served.sent_footprints();
    assert_eq!(sent.len(), 3);
    let wson = sent
        .iter()
        .find(|fp| {
            fp.reference_field
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                == "U1"
        })
        .unwrap();
    let wson_full = wson
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| any_is(item, "kiapi.board.types.Pad"))
        .map(|item| Pad::decode(item.value.as_slice()).unwrap())
        .filter(|p| {
            p.pad_stack
                .as_ref()
                .and_then(|s| s.zone_settings.as_ref())
                .is_some_and(|z| z.zone_connection == ZoneConnectionStyle::ZcsFull as i32)
        })
        .count();
    assert_eq!(wson_full, 2);

    let dip = sent
        .iter()
        .find(|fp| {
            fp.reference_field
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                == "U2"
        })
        .unwrap();
    let rps: Vec<ReferencePoint> = dip
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| any_is(item, "kiapi.board.types.ReferencePoint"))
        .map(|item| ReferencePoint::decode(item.value.as_slice()).unwrap())
        .collect();
    assert_eq!(rps.len(), 1);

    // On KiCad < 10.0.7, DIP8's reference point is reported in `not_carried`.
    let older = ServedSync::reading_back(Readback::AsSent).await;
    older.report_kicad_version(10, 0, 6);
    let older_dir = older.board.parent().unwrap();
    let older_pretty = older_dir.join("Test_318.pretty");
    std::fs::create_dir_all(&older_pretty).unwrap();
    std::fs::write(
        older_pretty.join("DIP8.kicad_mod"),
        include_str!("../../../../tests/fixtures/issue_283_dip8_kicad10.kicad_mod"),
    )
    .unwrap();
    let older_fp_table = older_dir.join("fp-lib-table");
    let older_table = std::fs::read_to_string(&older_fp_table)
        .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string())
        .replacen(
            "\n)",
            "\n  (lib (name \"Test_318\")(type \"KiCad\")(uri \"${KIPRJMOD}/Test_318.pretty\")(options \"\")(descr \"\"))\n)",
            1,
        );
    std::fs::write(&older_fp_table, older_table).unwrap();
    let dip_only = exported_netlist(&[("U2", "Test_318:DIP8", &["1", "2"])]);
    let older_applied = older.apply(&dip_only).await;
    assert_eq!(older_applied["status"], "applied", "{older_applied:#}");
    let older_diags = older_applied["diagnostics"].as_array().unwrap();
    assert_eq!(older_diags.len(), 2, "{older_applied:#}");
    assert_eq!(older_diags[0]["code"], "not_carried");
    assert!(older_diags[0]["message"]
        .as_str()
        .unwrap()
        .contains("reference point"));
    // Nor can it take the symbol units (#682).
    assert_eq!(older_diags[1]["code"], "symbol_units_not_carried");
}

/// An added net tie is read with its group, and built with it when the
/// KiCad takes it and without it when not.
#[test]
fn an_added_net_tie_carries_its_group_when_kicad_takes_it() {
    use copperpour_ipc::gen::kiapi;

    let part = prepare_footprint_source(NET_TIE_3_THT_SOURCE).unwrap();
    assert!(part.unsupported.is_empty(), "{:?}", part.unsupported);
    let groups = |carried: CarriedByKiCad| {
        let item = build_added_footprint(
            &planned_add("NT1", NET_TIE_3_THT, &[]),
            &part,
            &BTreeMap::new(),
            carried,
        )
        .unwrap();
        kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
            .unwrap()
            .definition
            .unwrap()
            .net_ties
            .into_iter()
            .map(|group| group.pad_number)
            .collect::<Vec<_>>()
    };
    assert_eq!(groups(CarriedByKiCad::ALL), [vec!["1", "2", "3"]]);
    assert!(groups(CarriedByKiCad::default()).is_empty());

    // A group the API would not give back refuses the part, as the
    // refresh refuses it.
    let malformed = NET_TIE_3_THT_SOURCE.replace(
        "(net_tie_pad_groups \"1, 2, 3\")",
        "(net_tie_pad_groups \"1, 4\")",
    );
    assert_ne!(malformed, NET_TIE_3_THT_SOURCE, "the edit must land");
    let Err((code, reason)) = prepare_footprint_source(&malformed) else {
        panic!("a group naming a missing pad is refused");
    };
    assert_eq!(code, "unsupported_library_footprint");
    assert!(reason.contains("names pad '4'"), "{reason}");
}

/// Update PCB names the net ties a KiCad before 10.0.1 cannot keep, with
/// every part that uses them, and none from 10.0.1.
#[test]
fn net_ties_an_older_kicad_cannot_keep_are_named() {
    let prepared = BTreeMap::from([(
        NET_TIE_3_THT.to_string(),
        prepare_footprint_source(NET_TIE_3_THT_SOURCE).unwrap(),
    )]);
    let plan = plan_adding(vec![
        planned_add("NT1", NET_TIE_3_THT, &[]),
        planned_add("NT2", NET_TIE_3_THT, &[]),
    ]);
    assert!(net_ties_not_carried(&plan, &prepared, &kicad_10_0(1)).is_empty());
    let diagnostics = net_ties_not_carried(&plan, &prepared, &kicad_10_0(0));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "not_carried");
    assert_eq!(diagnostics[0].references, ["NT1", "NT2"]);
    assert!(
        diagnostics[0].message.contains(
            "net-tie pad groups (\"1, 2, 3\") require KiCad 10.0.1 or newer (connected \
                 KiCad is 10.0.0)"
        ),
        "{}",
        diagnostics[0].message
    );
}

/// Update PCB's replacement onto a net tie is refused on KiCad 10.0.0, as
/// the refresh refuses it, and read from 10.0.1.
#[test]
fn a_replacement_onto_a_net_tie_needs_kicad_10_0_1() {
    let (temp, board_path) = project_with_stock_footprints();
    std::fs::write(
        temp.path()
            .join("Custom_Stock.pretty/NetTie-3_THT_Pad0.3mm.kicad_mod"),
        NET_TIE_3_THT_SOURCE,
    )
    .unwrap();
    let footprint_id = "Custom_Stock:NetTie-3_THT_Pad0.3mm";
    let (code, reason) = read_replacement(footprint_id, &board_path, &kicad_10_0(0)).unwrap_err();
    assert_eq!(code, "unsupported_library_footprint");
    assert_eq!(
        reason,
        "it has net-tie pad groups, which KiCad's API carries only from KiCad 10.0.1; this \
             KiCad is 10.0.0"
    );
    let footprint = read_replacement(footprint_id, &board_path, &kicad_10_0(1)).unwrap();
    assert!(footprint.has_net_ties());
}

/// Update PCB adds a net tie with its library group on KiCad 10.0.8. On
/// 10.0.0 it adds it without, asking KiCad's version once per call, and
/// says so in a `not_carried` diagnostic.
#[tokio::test]
async fn an_added_net_tie_carries_its_group_or_reports_it_not_carried() {
    let netlist = exported_netlist(&[("NT1", NET_TIE_3_THT, &["1", "2", "3"])]);

    let served = ServedSync::reading_back(Readback::AsSent).await;
    net_tie_library(&served);
    let applied = served.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    let sent = served.sent_footprints();
    let net_ties = &sent[0].definition.as_ref().unwrap().net_ties;
    assert_eq!(net_ties.len(), 1, "{net_ties:?}");
    assert_eq!(net_ties[0].pad_number, ["1", "2", "3"]);
    assert_eq!(served.sent("GetVersion"), 2);

    let older = ServedSync::reading_back(Readback::AsSent).await;
    older.report_kicad_version(10, 0, 0);
    net_tie_library(&older);
    let applied = older.apply(&netlist).await;
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let diagnostics = applied["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2, "{applied:#}");
    assert_eq!(diagnostics[0]["code"], "not_carried");
    assert_eq!(diagnostics[0]["footprint_id"], NET_TIE_3_THT);
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("require KiCad 10.0.1 or newer (connected KiCad is 10.0.0)"),
        "{applied:#}"
    );
    // Nor can it take the symbol units (#682).
    assert_eq!(diagnostics[1]["code"], "symbol_units_not_carried");
    assert!(older.sent_footprints()[0]
        .definition
        .as_ref()
        .unwrap()
        .net_ties
        .is_empty());
}

/// An added footprint gets the library's own solder mask and paste
/// margins, and each pad its own on both outer layers, a margin of 0 as an
/// override, with none of them in `not_carried`; on KiCad 10.0.6 too,
/// which reads them as 10.0.8 does.
#[tokio::test]
async fn added_footprint_carries_pad_and_footprint_solder_margins() {
    use crate::tools::library_footprint::test_margins::{
        expected_margins, placed_margins, SOLDER_MARGIN_FOOTPRINTS,
    };

    let references = ["U1", "U2", "U3", "U4"];
    let ids = SOLDER_MARGIN_FOOTPRINTS.map(|(name, ..)| format!("Margins:{name}"));
    let parts: Vec<(&str, &str, &[&str])> = references
        .iter()
        .zip(&ids)
        .map(|(reference, id)| (*reference, id.as_str(), &["1", "9"][..]))
        .collect();
    for version in [(10, 0, 6), (10, 0, 8)] {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        served.report_kicad_version(version.0, version.1, version.2);
        margins_library(
            &served,
            &SOLDER_MARGIN_FOOTPRINTS.map(|(name, source, ..)| (name, source)),
        );

        let applied = served.apply(&exported_netlist(&parts)).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert!(
            !applied["diagnostics"].to_string().contains("solder"),
            "{applied:#}"
        );
        // On 10.0.6, pad 9's heatsink property, which needs 10.0.7, is
        // listed.
        if version == (10, 0, 8) {
            assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        }

        let sent = served.sent_footprints();
        assert_eq!(sent.len(), references.len());
        for (reference, (name, _, own, pad_9)) in references.iter().zip(SOLDER_MARGIN_FOOTPRINTS) {
            let placed = sent
                .iter()
                .find(|fp| {
                    fp.reference_field
                        .as_ref()
                        .and_then(|field| field.text.as_ref())
                        .and_then(|text| text.text.as_ref())
                        .is_some_and(|text| text.text == *reference)
                })
                .unwrap();
            assert_eq!(
                placed_margins(placed),
                expected_margins(own, pad_9),
                "{name} on {version:?}"
            );
        }
    }
}

/// Inline because KiCad writes no such clause: a library footprint whose
/// pad or own margin is not one number cannot be added; the part is
/// skipped and named, as KiCad's Update PCB skips a part it cannot add,
/// and the rest is still planned.
#[tokio::test]
async fn a_footprint_with_a_malformed_solder_margin_is_skipped_and_named() {
    use crate::tools::library_footprint::test_margins::SOLDER_MARGIN_FOOTPRINTS;

    for (owner, good, clause) in [
        (
            "pad",
            SOLDER_MARGIN_FOOTPRINTS[0].1,
            "(solder_mask_margin -0.05)",
        ),
        (
            "footprint",
            SOLDER_MARGIN_FOOTPRINTS[2].1,
            "(solder_mask_margin 0.05)",
        ),
    ] {
        assert_eq!(good.matches(clause).count(), 1, "{owner}");
        let bad = good.replace(clause, "(solder_mask_margin x)");
        let served = ServedSync::new().await;
        margins_library(&served, &[("good", good), ("bad", &bad)]);

        let plan = served
            .dry_run(&exported_netlist(&[
                ("U1", "Margins:bad", &["1", "9"]),
                ("U2", "Margins:good", &["1", "9"]),
            ]))
            .await;
        assert_eq!(plan["status"], "ready", "{owner}: {plan:#}");
        assert_eq!(plan["changes_count"], 1, "{owner}: {plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "U2", "{owner}: {plan:#}");
        let skipped = &plan["skipped_footprints"];
        assert_eq!(skipped.as_array().unwrap().len(), 1, "{owner}: {plan:#}");
        assert_eq!(skipped[0]["code"], "unsupported_library_footprint");
        assert_eq!(skipped[0]["footprint_id"], "Margins:bad");
        assert_eq!(skipped[0]["references"], serde_json::json!(["U1"]));
        assert!(
            skipped[0]["message"].as_str().unwrap().contains(&format!(
                "{owner} clause 'solder_mask_margin' has an invalid value 'x'"
            )),
            "{owner}: {plan:#}"
        );
    }
}

/// A replacement by a footprint with solder margins takes them, as KiCad's
/// Update PCB does (`ExchangeFootprint` with its defaults): each pad gets
/// the library's, and the footprint's own clearance, zone connection,
/// margins and bridge setting become the library's, whatever the board
/// had. The next dry run has nothing to do.
#[tokio::test]
async fn a_replacement_takes_the_new_footprints_solder_margins() {
    use crate::tools::library_footprint::test_margins::{
        expected_margins, placed_margins, SOLDER_MARGIN_FOOTPRINTS,
    };
    use copperpour_ipc::gen::kiapi::board::types::{
        FootprintInstance, SolderMaskOverrides, ZoneConnectionStyle,
    };

    for (name, source, own, pad_9) in SOLDER_MARGIN_FOOTPRINTS {
        let mut before =
            FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        let overrides = before.overrides.get_or_insert_with(Default::default);
        overrides.copper_clearance = Some(copperpour_ipc::builders::distance(0.2));
        overrides.solder_mask = Some(SolderMaskOverrides {
            solder_mask_margin: Some(copperpour_ipc::builders::distance(0.07)),
            ..Default::default()
        });
        overrides.zone_connection = ZoneConnectionStyle::ZcsFull as i32;
        before.attributes.as_mut().unwrap().allow_soldermask_bridges = true;
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &before,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        margins_library(&served, &[(name, source)]);
        let netlist = r1_on(&format!("Margins:{name}"));

        let plan = served.dry_run(&netlist).await;
        assert_eq!(
            plan["changes"][0]["replace_footprint"]["to"],
            format!("Margins:{name}"),
            "{name}: {plan:#}"
        );
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{name}: {applied:#}");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        assert_eq!(
            placed_margins(&held),
            expected_margins(own, pad_9),
            "{name}"
        );
        let overrides = held.overrides.unwrap();
        assert_eq!(overrides.copper_clearance, None, "{name}");
        assert_eq!(
            overrides.zone_connection,
            ZoneConnectionStyle::ZcsInherited as i32,
            "{name}"
        );
        assert!(!held.attributes.unwrap().allow_soldermask_bridges, "{name}");
        assert_eq!(served.dry_run(&netlist).await["status"], "noop", "{name}");
    }
}
