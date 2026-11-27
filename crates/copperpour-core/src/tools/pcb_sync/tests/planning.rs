use super::*;

/// Nothing asserted the *response* keys — every other test here reads the
/// `SyncPlan` struct — so a renamed or dropped JSON field was invisible to
/// the suite. This pins the two names a caller reads.
#[test]
fn the_response_names_the_collection_in_the_plural_and_the_count_in_the_singular() {
    let design = unassigned_design();
    let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));
    let result = sync_response(&plan, "ready", 1, false, &ResponseOptions::default());
    let text = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    let unassigned = value["unassigned_footprints"]
        .as_array()
        .expect("the collection is a plural-named array");
    assert_eq!(unassigned.len(), 1);
    assert_eq!(unassigned[0]["reference"], "R1");
    assert_eq!(unassigned[0]["board_state"], "absent");
    // The count category keeps its singular name.
    assert_eq!(value["coverage"]["unassigned_footprint"]["planned"], 1);
    assert!(value.get("unassigned_footprint").is_none());
}

/// Empty board: the two assigned components are planned as additions, the
/// unassigned one is listed as absent, and the plan is ready — not a
/// conflict, and R1 is not counted under `added`.
#[test]
fn the_sync_goes_on_for_every_assigned_component() {
    let design = unassigned_design();
    let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));

    assert_eq!(plan.status, PlanStatus::Ready);
    assert!(plan.diagnostics.is_empty());
    assert_eq!(plan.counts.added.planned, 2);
    assert_eq!(plan.counts.unassigned_footprint.planned, 1);
    assert_eq!(plan.unassigned.len(), 1);
    assert_eq!(plan.unassigned[0].reference, "R1");
    assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Absent);
    assert!(plan.changes.iter().all(|change| {
        !matches!(change, PlannedChange::Add { reference, .. } if reference == "R1")
    }));
}

/// A footprint already on the board for the unassigned symbol is kept as
/// it is and counted as matched — not board-only, not a conflict.
#[test]
fn an_existing_board_footprint_for_an_unassigned_symbol_is_kept() {
    let design = unassigned_design();
    let r1_path = design.unassigned[0].symbol_path.clone();
    let plan = plan_sync(
        UNASSIGNED,
        &design,
        &board_with(vec![board_resistor("R1", Some(&r1_path))]),
    );

    assert_eq!(plan.status, PlanStatus::Ready);
    assert!(plan.diagnostics.is_empty());
    assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Kept);
    assert_eq!(plan.counts.board_only_preserved.planned, 0);
    assert!(!plan.changes.iter().any(
        |change| matches!(change, PlannedChange::Update { reference, .. } if reference == "R1")
    ));
}

/// Nothing but unassigned parts is a no-op that still names them.
#[test]
fn an_all_unassigned_schematic_is_a_noop_with_the_list() {
    let mut design = unassigned_design();
    design.components.clear();
    let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));

    assert_eq!(plan.status, PlanStatus::Noop);
    assert_eq!(plan.counts.unassigned_footprint.planned, 1);
    assert_eq!(plan.unassigned.len(), 1);
}

/// A genuine conflict still clears the changes, but the report about the
/// schematic survives it.
#[test]
fn the_unassigned_list_survives_a_conflict() {
    let design = unassigned_design();
    let mut stranger = board_resistor("C1", Some("/other/identity"));
    stranger.footprint_id = "Capacitor_SMD:C_0603_1608Metric".to_string();
    let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![stranger]));

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.unassigned.len(), 1);
}

/// A part whose footprint cannot be built, and an update that would lose
/// what its footprint owns, are skipped; every other part stays planned,
/// the counts follow (R1's net change goes with R1), the skipped parts are
/// reported with their own diagnostics, and a note about a skipped part
/// goes while one about a planned part stays.
#[test]
fn a_part_that_cannot_be_built_is_skipped_and_the_rest_stays_planned() {
    let (mut plan, board) = two_updates_and_two_additions();
    plan.diagnostics.push(conflict(
        "footprint_locked_not_replaced",
        "R1".to_string(),
        Some("R1"),
    ));
    plan.diagnostics.push(conflict(
        "footprint_locked_not_replaced",
        "R2".to_string(),
        Some("R2"),
    ));
    let preflight = vec![
        cannot_build("unsupported_library_footprint", &["R3"]),
        cannot_build("footprint_children_not_carried", &["R1"]),
    ];

    assert!(skip_parts(
        &mut plan,
        preflight.clone(),
        &board,
        &BTreeSet::new()
    ));

    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(planned_references(&plan), ["R2", "R4"]);
    assert_eq!(plan.counts.added.planned, 1);
    assert_eq!(plan.counts.updated.planned, 1);
    assert_eq!(plan.counts.pads_reassigned.planned, 0);
    assert_eq!(plan.counts.skipped_footprint.planned, 2);
    assert_eq!(plan.counts.conflicts.planned, 0);
    assert_eq!(plan.skipped, preflight);
    let notes: Vec<_> = plan
        .diagnostics
        .iter()
        .map(|note| note.message.as_str())
        .collect();
    assert_eq!(notes, ["R2"]);
}

/// Skipping only R2 keeps R1's net change counted.
#[test]
fn skipping_a_part_keeps_the_net_changes_of_the_rest() {
    let (mut plan, board) = two_updates_and_two_additions();
    assert!(skip_parts(
        &mut plan,
        vec![cannot_build("footprint_children_not_carried", &["R2"])],
        &board,
        &BTreeSet::new(),
    ));
    assert_eq!(planned_references(&plan), ["R1", "R3", "R4"]);
    assert_eq!(plan.counts.pads_reassigned.planned, 1);
}

/// An acknowledged routed pad net change (#38) goes with its part when
/// the part is skipped: its note is dropped, so the apply neither makes
/// nor reports it, and the rest of the plan stays.
#[test]
fn skipping_a_part_drops_its_acknowledged_net_changes() {
    let (mut design, mut board) = routed_r1_moving_both_pads();
    let mut r2 = resistor("R2", "/r2");
    r2.value = "1k".to_string();
    design.components.push(r2);
    board.footprints.push(board_resistor("R2", Some("/r2")));
    let acknowledged = BTreeSet::from([
        acknowledging("R1", "1", "VCC", "VCC_A"),
        acknowledging("R1", "2", "GND", "GND_A"),
    ]);
    let mut plan = plan_sync_acknowledging("netlist", &design, &board, &acknowledged, false);
    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    assert_eq!(plan.counts.pads_reassigned.planned, 2);
    let skipped = vec![cannot_build("unsupported_library_footprint", &["R1"])];

    assert!(skip_parts(
        &mut plan,
        skipped.clone(),
        &board,
        &BTreeSet::new()
    ));

    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(planned_references(&plan), ["R2"]);
    assert_eq!(plan.counts.pads_reassigned.planned, 0);
    assert_eq!(plan.skipped, skipped);
    assert!(
        plan.diagnostics
            .iter()
            .all(|note| note.code != "routed_pad_net_change_acknowledged"),
        "{:?}",
        plan.diagnostics
    );
}

/// When every planned part would be skipped there is nothing to apply,
/// and the plan is refused whole with the diagnostics as conflicts, as it
/// was before parts were skipped.
#[test]
fn skipping_every_part_refuses_the_plan() {
    let (mut plan, board) = two_updates_and_two_additions();
    let all = cannot_build("footprint_pad_missing", &["R1", "R2", "R3", "R4"]);

    assert!(!skip_parts(
        &mut plan,
        vec![all.clone()],
        &board,
        &BTreeSet::new()
    ));

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.added.planned, 0);
    assert_eq!(plan.counts.updated.planned, 0);
    assert_eq!(plan.counts.conflicts.planned, 1);
    assert_eq!(plan.counts.skipped_footprint.planned, 0);
    assert!(plan.skipped.is_empty());
    assert_eq!(plan.diagnostics, [all]);
}

/// An update that `leave_out_updates` takes off next counts as nothing to
/// apply: a plan left with only that would read `noop` and hide the parts
/// the schematic asked for. It is refused instead.
#[test]
fn a_plan_left_with_only_left_out_updates_is_refused_not_a_noop() {
    let (mut plan, board) = two_updates_and_two_additions();
    let skipped = cannot_build("unsupported_library_footprint", &["R1", "R3", "R4"]);

    assert!(!skip_parts(
        &mut plan,
        vec![skipped],
        &board,
        &BTreeSet::from(["R2-kiid"]),
    ));

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty());
}

/// A preflight diagnostic that names no part cannot be skipped by part,
/// so it refuses the whole plan, the named ones with it.
#[test]
fn a_diagnostic_without_parts_refuses_the_whole_plan() {
    let (mut plan, board) = two_updates_and_two_additions();
    let preflight = vec![
        cannot_build("unsupported_library_footprint", &["R3"]),
        conflict("library_unreadable", "no parts".to_string(), None),
    ];

    assert!(!skip_parts(&mut plan, preflight, &board, &BTreeSet::new()));

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.conflicts.planned, 2);
    assert!(plan.skipped.is_empty());
}

/// `leave_out_updates` counts and reports only an update still planned,
/// so one `skip_parts` already took off is not taken off twice.
#[test]
fn leaving_out_a_skipped_update_changes_nothing() {
    let (mut plan, board) = two_updates_and_two_additions();
    assert!(skip_parts(
        &mut plan,
        vec![cannot_build("unsupported_library_footprint", &["R2"])],
        &board,
        &BTreeSet::new(),
    ));
    let diagnostic = cannot_build("footprint_sheet_not_updated", &["R2"]);

    leave_out_updates(&mut plan, vec![("R2-kiid".to_string(), diagnostic)]);

    assert_eq!(plan.counts.updated.planned, 1);
    assert!(plan.diagnostics.is_empty());
    assert_eq!(plan.status, PlanStatus::Ready);
}

/// What was skipped, and why, is part of the reviewed plan: the same
/// changes with another part skipped, or the same part skipped for
/// another reason, are another plan revision.
#[test]
fn the_skipped_parts_are_part_of_the_plan_revision() {
    let revision = |skipped: Vec<SyncDiagnostic>| {
        let (mut plan, board) = two_updates_and_two_additions();
        assert!(skip_parts(&mut plan, skipped, &board, &BTreeSet::new()));
        refresh_revision_with_staging(&mut plan);
        plan.plan_revision
    };
    let unsupported = revision(vec![cannot_build("unsupported_library_footprint", &["R3"])]);
    let unresolved = revision(vec![cannot_build(
        "footprint_library_resolution_failed",
        &["R3"],
    )]);
    assert_ne!(unsupported, unresolved);
    assert_eq!(
        unsupported,
        revision(vec![cannot_build("unsupported_library_footprint", &["R3"])])
    );
}

#[test]
fn planner_matches_identity_preserves_pose_and_stages_new_parts_deterministically() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![
            resistor("R2", "/sheet/existing"),
            resistor("R3", "/sheet/new"),
        ],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let board = BoardState {
        footprints: vec![
            BoardFootprint {
                kiid: "existing-kiid".to_string(),
                reference: "R1".to_string(),
                value: "1k".to_string(),
                footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
                symbol_path: Some("/sheet/existing".to_string()),
                pad_nets: BTreeMap::from([
                    ("1".to_string(), "VCC".to_string()),
                    ("2".to_string(), "GND".to_string()),
                ]),
                pad_numbers: BTreeSet::from(["1".to_string(), "2".to_string()]),
                position: Point { x: 25.0, y: 30.0 },
                rotation: 90.0,
                layer: "B.Cu".to_string(),
                locked: true,
                dnp: false,
                exclude_from_bom: false,
                not_in_schematic: false,
                fields: BTreeMap::new(),
                sheet: SymbolSheet::default(),
            },
            BoardFootprint {
                kiid: "board-only".to_string(),
                reference: "MH1".to_string(),
                value: "MountingHole".to_string(),
                footprint_id: "MountingHole:MountingHole_3.2mm_M3".to_string(),
                symbol_path: None,
                pad_nets: BTreeMap::new(),
                pad_numbers: BTreeSet::new(),
                position: Point { x: 2.0, y: 2.0 },
                rotation: 0.0,
                layer: "F.Cu".to_string(),
                locked: true,
                dnp: false,
                exclude_from_bom: false,
                not_in_schematic: true,
                fields: BTreeMap::new(),
                sheet: SymbolSheet::default(),
            },
        ],
        routed_nets: BTreeMap::new(),
        bounds: Bounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 50.0,
            max_y: 40.0,
        },
    };

    let first = plan_sync("netlist bytes", &design, &board);
    let second = plan_sync("netlist bytes", &design, &board);

    assert_eq!(first.status, PlanStatus::Ready);
    assert_eq!(first.plan_revision, second.plan_revision);
    assert_eq!(first.counts.added.planned, 1);
    assert_eq!(first.counts.updated.planned, 1);
    assert_eq!(first.counts.board_only_preserved.planned, 1);
    assert_eq!(first.changes, second.changes);
    assert!(first.changes.iter().any(|change| matches!(
        change,
        PlannedChange::Update { kiid, reference, preserve, .. }
            if kiid == "existing-kiid"
                && reference == "R2"
                && preserve.position == Point { x: 25.0, y: 30.0 }
                && preserve.rotation == 90.0
                && preserve.layer == "B.Cu"
                && preserve.locked
    )));
    assert!(first.changes.iter().any(|change| matches!(
        change,
        PlannedChange::Add { reference, position, .. }
            if reference == "R3" && position.x > board.bounds.max_x
    )));
}

/// A footprint whose fields alone differ is updated, carrying only the
/// changed fields; a field the symbol lacks is no reason to update (#788).
#[test]
fn a_field_only_difference_plans_an_update_carrying_the_changed_fields() {
    let mut component = resistor("R1", "/sheet/r1");
    component.fields = vec![
        symbol_field("LCSC", "C788002"),
        symbol_field("MPN", ""),
        symbol_field("Datasheet", ""),
        symbol_field("Description", "Resistor"),
    ];
    let mut footprint = board_resistor("R1", Some("/sheet/r1"));
    footprint.fields = BTreeMap::from([
        ("Datasheet".to_string(), String::new()),
        ("Description".to_string(), "Resistor".to_string()),
        ("LCSC".to_string(), "C788001".to_string()),
        ("Board only".to_string(), "kept".to_string()),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![component],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("fields", &design, &board_with(vec![footprint.clone()]));

    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.counts.updated.planned, 1);
    assert_eq!(plan.counts.pads_reassigned.planned, 0);
    assert!(
        matches!(plan.changes.as_slice(), [PlannedChange::Update { .. }]),
        "{:?}",
        plan.changes
    );
    let reported = serde_json::to_value(&plan.changes[0]).unwrap();
    assert_eq!(
        reported["fields"],
        serde_json::json!([
            {"name": "LCSC", "value": "C788002"},
            {"name": "MPN", "value": ""}
        ])
    );

    // Once the footprint carries the symbol's values, nothing is planned,
    // whatever else the footprint carries.
    footprint
        .fields
        .insert("LCSC".to_string(), "C788002".to_string());
    footprint.fields.insert("MPN".to_string(), String::new());
    let plan = plan_sync("fields", &design, &board_with(vec![footprint]));
    assert_eq!(plan.status, PlanStatus::Noop, "{:?}", plan.changes);
}

#[test]
fn removing_a_pad_from_a_routed_net_conflicts_the_whole_plan() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![DesignComponent {
            pad_nets: BTreeMap::new(),
            ..resistor("R1", "/sheet/existing")
        }],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let board = BoardState {
        footprints: vec![BoardFootprint {
            kiid: "existing-kiid".to_string(),
            reference: "R1".to_string(),
            value: "10k".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            symbol_path: Some("/sheet/existing".to_string()),
            pad_nets: BTreeMap::from([("1".to_string(), "VCC".to_string())]),
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
        }],
        routed_nets: BTreeMap::from([("VCC".to_string(), 1)]),
        bounds: Bounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 10.0,
            max_y: 10.0,
        },
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty());
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "routed_pad_net_change"));
}

/// #38: each routed pad net change named exactly, as its diagnostic
/// reports it, is planned as an ordinary pad change and noted.
#[test]
fn acknowledged_routed_pad_net_changes_are_planned_and_noted() {
    let (design, board) = routed_r1_moving_both_pads();
    let refused = plan_sync("netlist", &design, &board);
    assert_eq!(refused.status, PlanStatus::Conflict);
    let reported = refused
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "routed_pad_net_change")
        .map(|diagnostic| {
            (
                diagnostic.reference.clone().unwrap(),
                diagnostic.net_change.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let change = |pad: &str, from: &str, to: &str| NetChange {
        pad: pad.to_string(),
        from: from.to_string(),
        to: to.to_string(),
    };
    assert_eq!(
        reported,
        vec![
            ("R1".to_string(), change("1", "VCC", "VCC_A")),
            ("R1".to_string(), change("2", "GND", "GND_A")),
        ]
    );
    let acknowledged = reported
        .iter()
        .map(|(reference, change)| acknowledging(reference, &change.pad, &change.from, &change.to))
        .collect();

    let plan = plan_sync_acknowledging("netlist", &design, &board, &acknowledged, false);

    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    assert_eq!(refusing_codes(&plan), Vec::<&str>::new());
    assert_eq!(plan.counts.updated.planned, 1);
    assert_eq!(plan.counts.pads_reassigned.planned, 2);
    let PlannedChange::Update { pad_nets, .. } = &plan.changes[0] else {
        panic!("the acknowledged change is an update");
    };
    assert_eq!(pad_nets, &design.components[0].pad_nets);
    let noted = plan
        .diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.code.as_str(),
                diagnostic.net_change.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        noted,
        vec![
            (
                "routed_pad_net_change_acknowledged",
                change("1", "VCC", "VCC_A")
            ),
            (
                "routed_pad_net_change_acknowledged",
                change("2", "GND", "GND_A")
            ),
        ]
    );
}

/// #38: an acknowledgement lets through only the change it names. The
/// other routed change and any other conflict still refuse the plan, and
/// an acknowledgement of a change the plan does not make refuses it too.
#[test]
fn an_acknowledgement_lets_through_only_the_change_it_names() {
    let (design, board) = routed_r1_moving_both_pads();
    let pad_1 = acknowledging("R1", "1", "VCC", "VCC_A");
    let pad_2 = acknowledging("R1", "2", "GND", "GND_A");
    let unmatched = "net_change_acknowledgement_unmatched";
    for (acknowledged, codes) in [
        (vec![pad_1.clone()], vec!["routed_pad_net_change"]),
        // Another target net, another pad, another part.
        (
            vec![pad_1.clone(), acknowledging("R1", "2", "GND", "GND_B")],
            vec!["routed_pad_net_change", unmatched],
        ),
        (
            vec![
                pad_1.clone(),
                pad_2.clone(),
                acknowledging("R1", "3", "GND", "GND_A"),
            ],
            vec![unmatched],
        ),
        (
            vec![
                pad_1.clone(),
                pad_2.clone(),
                acknowledging("R2", "1", "VCC", "VCC_A"),
            ],
            vec![unmatched],
        ),
    ] {
        let plan = plan_sync_acknowledging(
            "netlist",
            &design,
            &board,
            &acknowledged.iter().cloned().collect(),
            false,
        );
        assert_eq!(plan.status, PlanStatus::Conflict, "{acknowledged:?}");
        assert!(plan.changes.is_empty(), "{acknowledged:?}");
        assert_eq!(refusing_codes(&plan), codes, "{acknowledged:?}");
    }

    // With no routed copper the changes need no acknowledgement, so one
    // given for them matches nothing.
    let unrouted = board_with(board.footprints.clone());
    let plan = plan_sync_acknowledging(
        "netlist",
        &design,
        &unrouted,
        &BTreeSet::from([pad_1.clone(), pad_2.clone()]),
        false,
    );
    assert_eq!(plan.status, PlanStatus::Conflict);
    assert_eq!(refusing_codes(&plan), vec![unmatched, unmatched]);

    // A conflict of another kind is not waived.
    let mut duplicated = board.clone();
    duplicated.footprints.push(board_resistor("R1", None));
    let plan = plan_sync_acknowledging(
        "netlist",
        &design,
        &duplicated,
        &BTreeSet::from([pad_1, pad_2]),
        false,
    );
    assert_eq!(plan.status, PlanStatus::Conflict);
    assert_eq!(refusing_codes(&plan), vec!["duplicate_board_reference"]);
}

#[test]
fn already_synchronized_design_is_noop() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let plan = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
    );

    assert_eq!(plan.status, PlanStatus::Noop);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.conflicts.planned, 0);
}

/// A footprint the schematic names differently is replaced, as KiCad's
/// Update PCB replaces it; a locked one keeps its footprint, with a note
/// that refuses nothing, as KiCad warns. The new footprint's pads are
/// checked in the handler's preflight, not here.
#[test]
fn a_changed_footprint_is_replaced_unless_locked_and_an_unrouted_net_change_is_planned() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let mut footprint = board_resistor("R1", Some("/sheet/existing"));
    footprint.footprint_id = "Resistor_SMD:R_0805_2012Metric".to_string();
    let to = design.components[0].footprint_id.clone();
    let swap = plan_sync("netlist", &design, &board_with(vec![footprint.clone()]));
    assert_eq!(swap.status, PlanStatus::Ready, "{:?}", swap.diagnostics);
    assert_eq!(swap.counts.updated.planned, 1);
    let PlannedChange::Update {
        replace_footprint,
        fields,
        ..
    } = &swap.changes[0]
    else {
        panic!("a replacement is an update");
    };
    assert_eq!(
        replace_footprint,
        &Some(ReplacedFootprint {
            from: "Resistor_SMD:R_0805_2012Metric".to_string(),
            to: to.clone(),
        })
    );
    // The new footprint's fields replace the old ones, so the symbol's
    // are all set again.
    assert_eq!(fields, &design.components[0].fields);
    assert_eq!(
        serde_json::to_value(&swap.changes[0]).unwrap()["replace_footprint"]["to"],
        to.as_str()
    );

    footprint.locked = true;
    let locked = plan_sync("netlist", &design, &board_with(vec![footprint]));
    assert_eq!(locked.status, PlanStatus::Noop, "{:?}", locked.diagnostics);
    assert_eq!(locked.counts.conflicts.planned, 0);
    assert_eq!(locked.diagnostics.len(), 1);
    assert_eq!(locked.diagnostics[0].code, "footprint_locked_not_replaced");

    let mut footprint = board_resistor("R1", Some("/sheet/existing"));
    footprint
        .pad_nets
        .insert("1".to_string(), "OLD_VCC".to_string());
    let net_change = plan_sync("netlist", &design, &board_with(vec![footprint]));
    assert_eq!(net_change.status, PlanStatus::Ready);
    assert_eq!(net_change.counts.pads_reassigned.planned, 1);
}

/// KiCad's Update PCB skips a symbol marked Exclude from board and treats
/// its footprint as one with no symbol (#870): the footprint is kept and
/// named, and nothing else is refused.
#[test]
fn on_board_no_skips_absent_and_names_its_footprint_when_present() {
    let design = r1_excluded();
    let absent = plan_sync("netlist", &design, &board_with(Vec::new()));
    assert_eq!(absent.status, PlanStatus::Noop);
    assert_eq!(absent.counts.skipped_by_flag.planned, 1);
    assert!(absent.without_symbols.is_empty());

    let present = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
    );
    assert_eq!(
        present.status,
        PlanStatus::Noop,
        "{:?}",
        present.diagnostics
    );
    assert!(present.diagnostics.is_empty(), "{:?}", present.diagnostics);
    assert_eq!(present.counts.skipped_by_flag.planned, 1);
    assert_eq!(present.counts.board_only_preserved.planned, 1);
    assert_eq!(
        present.without_symbols,
        vec![r1_without_a_symbol(
            WithoutSymbolReason::ExcludedFromBoard,
            false
        )]
    );
}

/// The rest of the schematic syncs beside it: R2 is added.
#[test]
fn an_excluded_part_on_the_board_leaves_the_rest_to_sync() {
    let mut design = r1_excluded();
    design.components.push(resistor("R2", "/sheet/new"));

    let plan = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
    );

    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    assert_eq!(plan.counts.added.planned, 1);
    assert_eq!(plan.without_symbols.len(), 1, "{:?}", plan.without_symbols);
    assert_eq!(
        plan.without_symbols[0].reason,
        WithoutSymbolReason::ExcludedFromBoard
    );
}

/// With `delete_footprints_without_symbols`, it is deleted, as KiCad
/// deletes it with "Delete footprints with no symbols"; the change says
/// why.
#[test]
fn an_excluded_part_s_footprint_is_deleted_on_request() {
    let plan = plan_sync_acknowledging(
        "netlist",
        &r1_excluded(),
        &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
        &BTreeSet::new(),
        true,
    );

    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    assert_eq!(
        plan.changes,
        vec![PlannedChange::Delete {
            kiid: "R1-kiid".to_string(),
            reference: "R1".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            reason: WithoutSymbolReason::ExcludedFromBoard,
        }]
    );
    assert_eq!(plan.counts.deleted.planned, 1);
    assert_eq!(plan.counts.board_only_preserved.planned, 0);
    assert!(plan.without_symbols.is_empty());
}

/// A planned deletion keeps its reason when the plan is refused, and in
/// the list the apply reads back against.
#[test]
fn an_excluded_part_s_deletion_keeps_its_reason_when_refused_or_read_back() {
    let mut plan = plan_sync_acknowledging(
        "netlist",
        &r1_excluded(),
        &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
        &BTreeSet::new(),
        true,
    );
    let excluded = vec![r1_without_a_symbol(
        WithoutSymbolReason::ExcludedFromBoard,
        false,
    )];
    assert_eq!(planned_deletions(&plan), excluded);

    refuse_plan(
        &mut plan,
        vec![conflict("preflight_conflict", "refused".to_string(), None)],
    );

    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert_eq!(plan.without_symbols, excluded);
}

/// Locked, it is kept with a note that names the reason; with the
/// board-only attribute, it is neither deleted nor named.
#[test]
fn a_locked_or_board_only_excluded_footprint_is_kept() {
    let mut footprint = board_resistor("R1", Some("/sheet/existing"));
    footprint.locked = true;
    let locked = plan_sync_acknowledging(
        "netlist",
        &r1_excluded(),
        &board_with(vec![footprint]),
        &BTreeSet::new(),
        true,
    );
    assert_eq!(locked.status, PlanStatus::Noop);
    assert_eq!(
        locked.without_symbols,
        vec![r1_without_a_symbol(
            WithoutSymbolReason::ExcludedFromBoard,
            true
        )]
    );
    assert_eq!(locked.diagnostics.len(), 1, "{:?}", locked.diagnostics);
    assert_eq!(locked.diagnostics[0].code, "footprint_locked_not_deleted");
    assert!(
        locked.diagnostics[0]
            .message
            .contains("has its symbol excluded from the board"),
        "{}",
        locked.diagnostics[0].message
    );

    let mut footprint = board_resistor("R1", Some("/sheet/existing"));
    footprint.not_in_schematic = true;
    let board_only = plan_sync_acknowledging(
        "netlist",
        &r1_excluded(),
        &board_with(vec![footprint]),
        &BTreeSet::new(),
        true,
    );
    assert_eq!(board_only.status, PlanStatus::Noop);
    assert!(board_only.changes.is_empty(), "{:?}", board_only.changes);
    assert!(board_only.without_symbols.is_empty());
    assert_eq!(board_only.counts.board_only_preserved.planned, 1);
}

/// The excluded symbol claims a footprint as a component does: by
/// identity, or by reference only when the footprint has no identity. A
/// footprint with its reference but another symbol's identity lost that
/// symbol, and is named for that reason.
#[test]
fn an_excluded_symbol_claims_a_footprint_by_reference_only_without_an_identity() {
    let unlinked = plan_sync(
        "netlist",
        &r1_excluded(),
        &board_with(vec![board_resistor("R1", None)]),
    );
    assert_eq!(
        unlinked.without_symbols,
        vec![r1_without_a_symbol(
            WithoutSymbolReason::ExcludedFromBoard,
            false
        )]
    );

    let another = plan_sync(
        "netlist",
        &r1_excluded(),
        &board_with(vec![board_resistor("R1", Some("/sheet/gone"))]),
    );
    assert_eq!(
        another.status,
        PlanStatus::Noop,
        "{:?}",
        another.diagnostics
    );
    assert_eq!(
        another.without_symbols,
        vec![r1_without_a_symbol(
            WithoutSymbolReason::SymbolRemoved,
            false
        )]
    );
}

/// The footprint an excluded symbol claims is no rename candidate for a
/// new part, as it was not while the sync refused it.
#[test]
fn an_excluded_part_s_footprint_is_no_rename_candidate() {
    let mut design = r1_excluded();
    design.components.push(resistor("R2", "/sheet/new"));

    let plan = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_resistor("R1", None)]),
    );

    assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
    assert_eq!(plan.counts.added.planned, 1);
    assert_eq!(
        plan.without_symbols,
        vec![r1_without_a_symbol(
            WithoutSymbolReason::ExcludedFromBoard,
            false
        )]
    );
}

#[test]
fn reference_only_possible_rename_is_a_conflict() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R2", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let plan = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_resistor("R1", None)]),
    );

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "reference_only_rename_ambiguous"));
}

#[test]
fn plan_revision_changes_when_reviewed_board_bounds_change() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/new")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };
    let first = plan_sync("netlist", &design, &board_with(Vec::new()));
    let mut changed_board = board_with(Vec::new());
    changed_board.bounds.max_x = 11.0;
    let second = plan_sync("netlist", &design, &changed_board);

    assert_ne!(first.plan_revision, second.plan_revision);
}

/// A plan revision must survive the clock. `kicad-cli` stamps the export
/// time and its own version into every netlist, so hashing the raw source
/// changed the revision every second — and apply, which requires the
/// revision a dry run returned, could then only succeed if both calls
/// landed inside the same wall-clock second.
#[test]
fn plan_revision_ignores_the_export_timestamp_and_tool_version() {
    let netlist = |date: &str, tool: &str| {
        format!(
            "(export (version \"E\")
  (design
    (source \"/tmp/x.kicad_sch\")
    (date \"{date}\")
    (tool \"{tool}\")
  )
  (components
    (comp (ref \"R1\")
      (value \"10k\")
      (footprint \"Resistor_SMD:R_0805\")
      (tstamps \"/aaa\")))
  (nets
    (net (code \"1\") (name \"GND\")
      (node (ref \"R1\") (pin \"1\")))))
"
        )
    };
    let board = BoardState {
        footprints: Vec::new(),
        routed_nets: BTreeMap::new(),
        bounds: Bounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 0.0,
            max_y: 0.0,
        },
    };
    let a = plan_revision(
        &netlist("2026-08-15T14:48:16", "kicad-cli (10.0.5)"),
        &board,
    );
    let b = plan_revision(
        &netlist("2026-08-15T14:48:18", "kicad-cli (10.0.5)"),
        &board,
    );
    assert_eq!(a, b, "two seconds apart is not a design change");

    let c = plan_revision(
        &netlist("2026-08-15T14:48:16", "kicad-cli (10.1.0)"),
        &board,
    );
    assert_eq!(a, c, "a KiCad upgrade is not a design change");

    // A real change still moves it, or the guard is worthless.
    let changed = netlist("2026-08-15T14:48:16", "kicad-cli (10.0.5)")
        .replace("Resistor_SMD:R_0805", "Resistor_SMD:R_0603");
    assert_ne!(
        a,
        plan_revision(&changed, &board),
        "a footprint swap must move the revision"
    );
}

#[test]
fn plan_revision_keeps_nested_and_quoted_design_content() {
    let netlist = |nested_date: &str, value: &str| {
        format!(
            r#"(export
  (design (date "2026-08-15T14:48:16") (tool "kicad-cli (10.0.5)"))
  (components
    (comp (ref "R1")
      (value "{value}")
      (footprint "Resistor_SMD:R_0805")
      (date "{nested_date}")
      (tstamps "/aaa")))
  (nets
    (net (code "1") (name "GND")
      (node (ref "R1") (pin "1")))))"#
        )
    };
    let board = board_with(Vec::new());
    let baseline = plan_revision(&netlist("2025-01-01", "literal (tool alpha)"), &board);

    assert_ne!(
        baseline,
        plan_revision(&netlist("2025-01-02", "literal (tool alpha)"), &board),
        "a nested date node is component content, not export metadata"
    );
    assert_ne!(
        baseline,
        plan_revision(&netlist("2025-01-01", "literal (tool beta)"), &board),
        "tool-like text inside a quoted value is design content"
    );
}

#[test]
fn issue_452_board_only_footprint_reads_as_no_identity_not_a_shared_one() {
    let logo = board_footprint_from_instance(&board_only_instance("logo-kiid", "LOGO1"))
        .expect("a board-only footprint is a valid footprint");

    assert_eq!(
        logo.symbol_path, None,
        "an empty SheetPath means no schematic symbol, not the identity `/`"
    );
    assert_eq!(logo.reference, "LOGO1");
    assert_eq!(logo.kiid, "logo-kiid");
    // KiCad does *not* set `not_in_schematic` on a board-only footprint —
    // it marks it excluded from position files and the BOM instead. The
    // first version of this test asserted the opposite and passed, because
    // the hand-built fixture it ran against said so. That flag is the
    // precondition for the rename branch two tests below, so getting it
    // wrong made that branch unreachable from the tests written to cover
    // this change.
    assert!(
        !logo.not_in_schematic,
        "real KiCad leaves not_in_schematic false on a board-only footprint"
    );
}

#[test]
fn issue_452_two_board_only_footprints_do_not_collide_on_identity() {
    // The whole board as KiCad reports it: one schematic-backed resistor
    // and two pathless graphics. Before the fix both graphics arrived
    // carrying `/`, and the planner refused to sync the resistor.
    let board = board_with(vec![
        board_resistor("R1", Some("/sheet/existing")),
        board_footprint_from_instance(&board_only_instance("logo-kiid", "LOGO1")).unwrap(),
        board_footprint_from_instance(&board_only_instance("fiducial-kiid", "FID1")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(
        plan.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "duplicate_board_identity")
            .count(),
        0,
        "footprints with no schematic identity are not duplicates of each other"
    );
    assert_eq!(plan.status, PlanStatus::Noop);
    assert_eq!(plan.counts.conflicts.planned, 0);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
}

#[test]
fn a_schematic_backed_footprint_still_reads_its_identity() {
    use copperpour_ipc::gen::kiapi;

    let mut instance = board_only_instance("r1-kiid", "R1");
    instance.symbol_path = Some(kiapi::common::types::SheetPath {
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

    let footprint = board_footprint_from_instance(&instance).unwrap();

    assert_eq!(
        footprint.symbol_path.as_deref(),
        Some("/sheet-uuid/symbol-uuid"),
        "the guard must not swallow a real schematic identity"
    );
}

// The adoption and possible-rename tests below exercise planner branches that
// require `symbol_path.is_none()`. Before #452, empty sheet paths decoded as
// `/`, so neither branch was reachable for these footprints. The captured
// footprint has `not_in_schematic` set to false, which lets it match a symbol.
#[test]
fn issue_452_an_unlinked_footprint_the_schematic_names_is_adopted_not_refused() {
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let mut footprint = board_footprint_from_instance(&unlinked_instance("R1-kiid", "R1")).unwrap();
    footprint.pad_nets = BTreeMap::from([
        ("1".to_string(), "VCC".to_string()),
        ("2".to_string(), "GND".to_string()),
    ]);
    // The capture is a mounting hole dressed as the schematic's resistor,
    // so it is given the resistor's pads along with their nets.
    footprint.pad_numbers = BTreeSet::from(["1".to_string(), "2".to_string()]);
    let plan = plan_sync("netlist", &design, &board_with(vec![footprint]));

    // Was `reference_identity_conflict`: the board footprint appeared to
    // hold the identity `/`, so the schematic's R1 looked like a different
    // symbol wearing the same reference.
    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.counts.updated.planned, 1);
    assert_eq!(plan.counts.board_only_preserved.planned, 0);
    let PlannedChange::Update {
        kiid, symbol_path, ..
    } = &plan.changes[0]
    else {
        panic!("adoption is an update, not an add: {:?}", plan.changes[0]);
    };
    assert_eq!(kiid, "R1-kiid");
    assert_eq!(
        symbol_path, "/sheet/existing",
        "adoption writes the schematic identity onto a footprint the \
             planner previously refused to touch"
    );
}

#[test]
fn issue_452_an_unlinked_lookalike_blocks_a_new_component_instead_of_duplicating_it() {
    // The board carries a footprint with no schematic identity whose
    // library id and value match a component the schematic has just gained.
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R5", "/sheet/new")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync(
        "netlist",
        &design,
        &board_with(vec![board_footprint_from_instance(&unlinked_instance(
            "R1-kiid", "R1",
        ))
        .unwrap()]),
    );

    // This plan was `Ready` before #452, and it added a second identical
    // footprint beside the unlinked one. It is now the conflict the
    // `possible_renames` scan was written to raise. Better, but it is a
    // fix that can newly block a sync, and that belongs in the notes.
    assert_eq!(plan.status, PlanStatus::Conflict);
    assert_eq!(plan.counts.added.planned, 0);
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "reference_only_rename_ambiguous"));
}

#[test]
fn issue_452_repeated_ref_star_graphics_are_not_a_duplicate_the_schematic_can_see() {
    // The reported board: a synchronized resistor plus two unannotated
    // graphics, both called `REF**` because that is what the library calls
    // them. Nothing in the schematic is named `REF**`, so no adoption ever
    // looks that reference up and there is nothing to disambiguate.
    let board = board_with(vec![
        board_resistor("R1", Some("/sheet/existing")),
        board_footprint_from_instance(&ref_star_instance("first-kiid")).unwrap(),
        board_footprint_from_instance(&ref_star_instance("second-kiid")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(
        plan.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "duplicate_board_reference")
            .count(),
        0,
        "a reference the schematic never names has no adoption to make ambiguous"
    );
    assert_eq!(plan.status, PlanStatus::Noop);
    assert_eq!(plan.counts.conflicts.planned, 0);
    assert_eq!(
        plan.counts.board_only_preserved.planned, 2,
        "both graphics are preserved, not merely un-diagnosed"
    );
    assert!(plan.changes.is_empty());
}

#[test]
fn issue_452_repeated_ref_star_graphics_no_longer_block_an_unrelated_update() {
    // The same board, with the resistor out of date. Before this change the
    // duplicate `REF**` diagnostic conflicted the whole plan, so the update
    // the user asked for could not be applied while those graphics existed
    // — which is the symptom #452 was filed about.
    let mut stale = board_resistor("R1", Some("/sheet/existing"));
    stale.value = "4k7".to_string();
    let board = board_with(vec![
        stale,
        board_footprint_from_instance(&ref_star_instance("first-kiid")).unwrap(),
        board_footprint_from_instance(&ref_star_instance("second-kiid")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.counts.updated.planned, 1);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
    let Some(PlannedChange::Update { kiid, value, .. }) = plan.changes.first() else {
        panic!("expected one update, got {:?}", plan.changes);
    };
    assert_eq!(kiid, "R1-kiid");
    assert_eq!(value, "10k");
}

/// A **scope test**: it passes with the narrowing removed, and that is the
/// point of it.
///
/// The guard tests above fail when the `exported_references` clause is
/// deleted. This one cannot — it asserts the diagnostic that the
/// unconditional version also raises. It exists to prove the narrowing did
/// not go too far, so its silence under neutering is the finding, and
/// counting it as coverage of the change would be a lie.
#[test]
fn issue_452_a_duplicate_reference_the_schematic_does_use_is_still_a_conflict() {
    // Two pathless footprints called `R1`, and a schematic component called
    // `R1` with no board identity to match on. The reference-only adoption
    // path keys on exactly that name, so with two candidates it would pick
    // whichever one `board_by_reference` happened to keep and write the
    // schematic identity onto it. That is the ambiguity the diagnostic
    // exists for, and scoping the rule to the schematic's own reference set
    // — rather than exempting board-only footprints as a kind — is what
    // keeps it.
    let board = board_with(vec![
        board_footprint_from_instance(&unlinked_instance("first-kiid", "R1")).unwrap(),
        board_footprint_from_instance(&unlinked_instance("second-kiid", "R1")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "duplicate_board_reference"));
    assert!(
        plan.changes.is_empty(),
        "an ambiguous adoption target must not be written to: {:?}",
        plan.changes
    );
    assert_eq!(plan.counts.updated.planned, 0);
    assert_eq!(plan.counts.added.planned, 0);
}

/// The `on_board=no` side of the export, which is a consult site too.
///
/// One of the four `board_by_reference` lookups reads `skipped.reference`,
/// so the set has to span both halves of the export or an excluded
/// instance loses its ambiguity check. This test guards the `.chain(...)`
/// specifically: it survives deleting the whole narrowing — the
/// unconditional diagnostic raises it too — and fails only when the
/// skipped half is dropped from the set. Do not read it as coverage of the
/// narrowing itself; the two `ref_star` tests above are that.
#[test]
fn issue_452_a_duplicate_reference_only_an_on_board_no_instance_uses_still_conflicts() {
    let board = board_with(vec![
        board_footprint_from_instance(&unlinked_instance("first-kiid", "R9")).unwrap(),
        board_footprint_from_instance(&unlinked_instance("second-kiid", "R9")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: vec![SkippedComponent {
            reference: "R9".to_string(),
            symbol_path: "/sheet/excluded".to_string(),
        }],
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "duplicate_board_reference"));
}

/// One of a duplicate pair *can* still be written to — when the schematic
/// matched it by path, which the duplicate reference plays no part in.
///
/// A footprint can carry a schematic identity and still wear `REF**` if it
/// was never back-annotated, and then it collides with the board's loose
/// graphics. The old diagnostic blocked the whole sync, so the identity
/// match could not be applied while such a graphic existed; now the
/// path-matched footprint is renamed and the other is preserved.
///
/// This is the honest limit of "un-diagnosed footprints are left alone":
/// they are never *selected by reference*, because the reference is not in
/// the export. Selection by path is a different and unambiguous route.
#[test]
fn issue_452_a_path_matched_footprint_is_still_adopted_despite_a_duplicate_reference() {
    let mut pathed = board_footprint_from_instance(&ref_star_instance("pathed-kiid")).unwrap();
    pathed.symbol_path = Some("/sheet/existing".to_string());
    pathed.footprint_id = "Resistor_SMD:R_0603_1608Metric".to_string();
    pathed.value = "10k".to_string();
    // A captured graphic dressed as the schematic's resistor, so it is
    // given the resistor's pads as well as its identity.
    pathed.pad_numbers = BTreeSet::from(["1".to_string(), "2".to_string()]);
    let board = board_with(vec![
        pathed,
        board_footprint_from_instance(&ref_star_instance("loose-kiid")).unwrap(),
    ]);
    let design = ExportedDesign {
        root_sheet_uuid: None,
        components: vec![resistor("R1", "/sheet/existing")],
        skipped: Vec::new(),
        unassigned: Vec::new(),
        net_names: BTreeSet::new(),
    };

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Ready);
    let Some(PlannedChange::Update {
        kiid, reference, ..
    }) = plan.changes.first()
    else {
        panic!("expected one update, got {:?}", plan.changes);
    };
    assert_eq!(
        kiid, "pathed-kiid",
        "the path match selected it, not the reference"
    );
    assert_eq!(reference, "R1");
    assert_eq!(
        plan.counts.board_only_preserved.planned, 1,
        "the footprint the schematic never named is preserved untouched"
    );
}

/// Without `delete_footprints_without_symbols`, R3 is kept and named, and
/// MH1 is kept and not named; both are counted as preserved.
#[test]
fn a_footprint_without_a_symbol_is_named_and_kept() {
    let (design, board) = footprints_without_symbols();

    let plan = plan_sync("netlist", &design, &board);

    assert_eq!(plan.status, PlanStatus::Noop);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
    assert_eq!(plan.counts.deleted.planned, 0);
    assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
}

/// With it, R3 is deleted, and that alone makes the plan ready. MH1 has
/// the board-only attribute, so it stays, as KiCad keeps it.
#[test]
fn a_footprint_without_a_symbol_is_deleted_on_request() {
    let (design, board) = footprints_without_symbols();

    let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.changes, vec![r3_deleted()]);
    assert_eq!(plan.counts.deleted.planned, 1);
    assert_eq!(plan.counts.board_only_preserved.planned, 1);
    assert!(
        plan.without_symbols.is_empty(),
        "{:?}",
        plan.without_symbols
    );
    assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
}

/// A locked R3 is kept with a note that refuses nothing, as KiCad keeps it
/// with a warning when locks are not overridden.
#[test]
fn a_locked_footprint_without_a_symbol_is_kept_with_a_note() {
    let (design, mut board) = footprints_without_symbols();
    board.footprints[1].locked = true;

    let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

    assert_eq!(plan.status, PlanStatus::Noop);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(true)]);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
    assert_eq!(plan.counts.conflicts.planned, 0);
    assert_eq!(plan.diagnostics.len(), 1, "{:?}", plan.diagnostics);
    assert_eq!(plan.diagnostics[0].code, "footprint_locked_not_deleted");
    assert_eq!(plan.diagnostics[0].reference.as_deref(), Some("R3"));
}

/// A plan with a conflict deletes nothing: R3 is named as kept.
#[test]
fn a_plan_with_a_conflict_deletes_no_footprint() {
    let (mut design, board) = footprints_without_symbols();
    design.components.push(resistor("R1", "/sheet/other"));

    let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

    assert_eq!(plan.status, PlanStatus::Conflict);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert_eq!(plan.counts.deleted.planned, 0);
    assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
}

/// A plan the handler refuses later, as a stale revision or a preflight
/// refusal, keeps the footprints it would have deleted, and says so.
#[test]
fn a_refused_plan_lists_the_footprints_it_would_have_deleted() {
    let (design, board) = footprints_without_symbols();
    let mut plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

    refuse_plan(
        &mut plan,
        vec![conflict("preflight_conflict", "refused".to_string(), None)],
    );

    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert_eq!(plan.counts.deleted.planned, 0);
    assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
    assert_eq!(plan.counts.board_only_preserved.planned, 2);
}

/// A part the preflight skips takes no deletion with it, and a plan left
/// with only the deletion still applies it.
#[test]
fn a_skipped_part_leaves_the_deletions_planned() {
    let (mut design, board) = footprints_without_symbols();
    design.components.push(resistor("R2", "/sheet/new"));
    let mut plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);
    assert_eq!(plan.counts.added.planned, 1, "{:?}", plan.changes);

    let applies = skip_parts(
        &mut plan,
        vec![cannot_build("unsupported_library_footprint", &["R2"])],
        &board,
        &BTreeSet::new(),
    );

    assert!(applies);
    assert_eq!(plan.status, PlanStatus::Ready);
    assert_eq!(plan.changes, vec![r3_deleted()]);
    assert_eq!(plan.counts.added.planned, 0);
    assert_eq!(plan.counts.deleted.planned, 1);
}
