use super::*;

/// The via and the zone on GND, which leaves the schematic, follow the
/// pads off it; copper on VCC, still in the schematic, and on no net
/// stays without a note.
#[test]
fn the_vias_and_zones_of_a_net_that_left_follow_its_pads() {
    let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
    let copper = [
        copper(CopperKind::Via, "via-gnd", "GND"),
        copper(CopperKind::Via, "via-vcc", "VCC"),
        copper(CopperKind::Via, "via-none", ""),
        copper(CopperKind::Zone, "zone-gnd", "GND"),
    ];

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

    assert_eq!(
        plan.copper_net_changes,
        vec![
            moved(CopperKind::Via, "via-gnd", "GND", "NEW"),
            moved(CopperKind::Zone, "zone-gnd", "GND", "NEW"),
        ]
    );
    assert_eq!(plan.counts.copper_net_changes.planned, 2);
    assert_eq!(note_codes(&plan), []);
}

/// GND splits: R1's pad goes to A, R2's to B. As in KiCad, the last pad
/// sets the net map, so the via and the zone with no connected pad go to
/// B; the zone R1's pad touches goes to A.
#[test]
fn a_zone_takes_the_net_of_a_pad_connected_to_it_first() {
    let (mut plan, board, net_names) = plan_moving_gnd("A", "B");
    let copper = [
        copper(CopperKind::Via, "via", "GND"),
        copper(CopperKind::Zone, "zone-r1", "GND"),
        copper(CopperKind::Zone, "zone-alone", "GND"),
    ];
    let zone_pads = BTreeMap::from([(
        "zone-r1".to_string(),
        vec![("R1-kiid".to_string(), "2".to_string())],
    )]);

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &zone_pads);

    assert_eq!(
        plan.copper_net_changes,
        vec![
            moved(CopperKind::Via, "via", "GND", "B"),
            moved(CopperKind::Zone, "zone-r1", "GND", "A"),
            moved(CopperKind::Zone, "zone-alone", "GND", "B"),
        ]
    );
}

/// A pad connected to the zone that stays on the zone's own net is passed
/// over, as KiCad's `if( getNetname( pad ) != zone->GetNetname() )` passes
/// it (`board_netlist_updater.cpp:1786`, 10.0.8): H1, a footprint the
/// schematic does not touch (a mounting hole or fiducial), keeps its pad 2
/// on GND under the GND zone. Listed before R1's pad, the zone still takes
/// R1's new net; listed alone, the net map gives the net (#866 review).
#[test]
fn a_connected_pad_that_stays_on_the_zones_net_is_passed_over() {
    let (mut plan, mut board, net_names) = plan_moving_gnd("A", "B");
    board.footprints.push(board_resistor("H1", None));
    let copper = [
        copper(CopperKind::Zone, "zone-h1-first", "GND"),
        copper(CopperKind::Zone, "zone-h1-only", "GND"),
    ];
    let h1 = ("H1-kiid".to_string(), "2".to_string());
    let r1 = ("R1-kiid".to_string(), "2".to_string());
    let zone_pads = BTreeMap::from([
        ("zone-h1-first".to_string(), vec![h1.clone(), r1]),
        ("zone-h1-only".to_string(), vec![h1]),
    ]);

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &zone_pads);

    assert_eq!(
        plan.copper_net_changes,
        vec![
            moved(CopperKind::Zone, "zone-h1-first", "GND", "A"),
            moved(CopperKind::Zone, "zone-h1-only", "GND", "B"),
        ]
    );
}

/// KiCad records a pad's move in the net map only when its new net is
/// not empty: R2's pad 2 losing its net does not take the via off to no
/// net, and R1's pad 2 moving to A still leads it there.
#[test]
fn a_pad_that_loses_its_net_leaves_the_net_map_alone() {
    let (mut plan, board, net_names) = plan_moving_gnd("A", "");
    let copper = [copper(CopperKind::Via, "via", "GND")];

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

    assert_eq!(
        plan.copper_net_changes,
        vec![moved(CopperKind::Via, "via", "GND", "A")]
    );
}

/// Copper on a net that left the schematic, with no pad moving off it,
/// stays, each item with KiCad's warning as a note naming it.
#[test]
fn copper_on_a_net_no_pad_moves_off_stays_with_a_note() {
    let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
    let copper = [
        copper(CopperKind::Via, "via-old", "OLD"),
        copper(CopperKind::Zone, "zone-old", "OLD"),
    ];

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

    assert_eq!(plan.copper_net_changes, []);
    assert_eq!(
        note_codes(&plan),
        [
            ("via_net_not_in_schematic", Some("via-old")),
            ("zone_net_not_in_schematic", Some("zone-old")),
        ]
    );
    assert_eq!(plan.status, PlanStatus::Ready);
}

/// KiCad reads a zone net that starts with a schematic net's name as
/// that net (`isInNetlist`); a via gets no such reading.
#[test]
fn a_zone_on_a_pseudo_name_of_a_schematic_net_stays() {
    let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
    let copper = [
        copper(CopperKind::Zone, "zone", "NEW_1"),
        copper(CopperKind::Via, "via", "NEW_1"),
    ];

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

    assert_eq!(plan.copper_net_changes, []);
    assert_eq!(
        note_codes(&plan),
        [("via_net_not_in_schematic", Some("via"))]
    );
}

/// The net map comes from the pad changes still planned: with the
/// updates taken off, as the preflight takes off a skipped part's, no
/// copper moves.
#[test]
fn copper_follows_only_the_pad_changes_still_planned() {
    let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
    plan.changes.clear();
    let copper = [copper(CopperKind::Via, "via", "GND")];

    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

    assert_eq!(plan.copper_net_changes, []);
    assert_eq!(
        note_codes(&plan),
        [("via_net_not_in_schematic", Some("via"))]
    );
}

/// A plan with a conflict moves no copper and notes none; a plan refused
/// later drops the moves it planned.
#[test]
fn a_refused_plan_moves_no_copper() {
    let copper = [copper(CopperKind::Via, "via", "GND")];
    let (mut conflicted, board, net_names) = plan_moving_gnd("NEW", "NEW");
    conflicted.status = PlanStatus::Conflict;
    plan_copper_nets(
        &mut conflicted,
        &board,
        &copper,
        &net_names,
        &BTreeMap::new(),
    );
    assert_eq!(conflicted.copper_net_changes, []);
    assert_eq!(note_codes(&conflicted), []);

    let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
    plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());
    assert_eq!(plan.counts.copper_net_changes.planned, 1);
    refuse_plan(
        &mut plan,
        vec![conflict("preflight_conflict", "refused".to_string(), None)],
    );
    assert_eq!(plan.copper_net_changes, []);
    assert_eq!(plan.counts.copper_net_changes.planned, 0);
}

/// #779: a copper zone counts as copper on its own net only. KiCad's
/// captured messages carry the pour's net and the rule area's lack of one.
#[test]
fn a_zone_pours_only_its_own_net() {
    let ZoneNet::Copper(net) = zone_net(GND_POUR) else {
        panic!("the captured pour is a copper zone");
    };
    assert_eq!(net.name, "GND");
    assert_eq!(zone_net(RULE_AREA), ZoneNet::RuleArea);
    assert_eq!(zone_net(&pour_without_a_net().value), ZoneNet::Unreadable);
    assert_eq!(zone_net(&[0xff, 0xff]), ZoneNet::Unreadable);
}

/// #779 through `tools/call`: a `GND` pour and a rule area on the board
/// leave a pad on `VCC`, which has no copper, free to change net.
#[tokio::test]
async fn a_pour_does_not_block_a_net_it_does_not_carry() {
    let plan = plan_moving_off("VCC", vec![zone_item(GND_POUR), zone_item(RULE_AREA)]).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(routed_pad_conflicts(&plan), Vec::<String>::new());
    let update = &plan["changes"][0];
    assert_eq!(update["kind"], "update", "{plan:#}");
    assert_eq!(update["reference"], "R1", "{plan:#}");
}

/// The pour's own net still conflicts: moving `R1`'s `GND` pad would
/// leave it under copper KiCad pours as `GND`.
#[tokio::test]
async fn a_pour_still_blocks_its_own_net() {
    let plan = plan_moving_off("GND", vec![zone_item(GND_POUR)]).await;

    assert_eq!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(
        routed_pad_conflicts(&plan),
        vec!["R1 pad 2 would change from 'GND' to 'NEW_NET' while routed copper uses that net"],
    );
}

/// Moving a pad onto the pour's net conflicts too: the check covers the
/// new net as well as the old one.
#[tokio::test]
async fn a_pour_blocks_a_pad_moving_onto_its_net() {
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: vec![zone_item(GND_POUR)],
        nets: vec!["VCC", "GND"],
    })
    .await;
    let netlist = ONE_RESISTOR
        .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
        .replace("(name \"/Power/VCC\")", "(name \"GND\")");

    let plan = served.dry_run(&netlist).await;

    assert_eq!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(
        routed_pad_conflicts(&plan),
        vec!["R1 pad 1 would change from 'VCC' to 'GND' while routed copper uses that net"],
    );
}

/// The plan #779 unblocks applies: KiCad's board ends with `R1` pad 1 on
/// `NEW_NET` under the `GND` pour it was refused for before.
#[tokio::test]
async fn a_pad_under_a_pour_on_another_net_is_reassigned() {
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: vec![zone_item(GND_POUR), zone_item(RULE_AREA)],
        nets: vec!["VCC", "GND"],
    })
    .await;

    let applied = served.apply(&netlist_moving_off("VCC")).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let held = served.held.lock().unwrap();
    let footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
        held[0].value.as_slice(),
    )
    .expect("the board's R1");
    let r1 = board_footprint_from_instance(&footprint).expect("R1 identity");
    assert_eq!(r1.pad_nets.get("1").map(String::as_str), Some("NEW_NET"));
    assert_eq!(r1.pad_nets.get("2").map(String::as_str), Some("GND"));
}

/// A copper zone whose net cannot be read could pour any net, so the
/// board still fails closed for every one of them.
#[tokio::test]
async fn a_pour_without_a_net_blocks_every_net() {
    let plan = plan_moving_off("VCC", vec![zone_item(GND_POUR), pour_without_a_net()]).await;

    assert_eq!(plan["status"], "conflict", "{plan:#}");
    assert_eq!(
        routed_pad_conflicts(&plan),
        vec!["R1 pad 1 would change from 'VCC' to 'NEW_NET' while routed copper uses that net"],
        "{plan:#}"
    );
}

/// #38 through `tools/call`: the conflict names the change; a dry run
/// that acknowledges it plans the update; the apply of that plan moves
/// R1's pad 2 off the pour's net and reports the copper on both nets and
/// DRC of the board KiCad then holds. The mock lists the pour on the new
/// net once the commit ends, so only a readback after it reports that.
#[tokio::test]
async fn an_acknowledged_routed_pad_net_change_is_applied_and_checked() {
    let served = served_r1_under_the_gnd_pour().await;
    served.list_zones_after_commit(vec![pour_on("NEW_NET")]);
    let netlist = netlist_moving_off("GND");

    let refused = served.dry_run(&netlist).await;
    assert_eq!(refused["status"], "conflict", "{refused:#}");
    let diagnostic = &refused["diagnostics"][0];
    assert_eq!(diagnostic["code"], "routed_pad_net_change", "{refused:#}");
    assert_eq!(diagnostic["reference"], "R1", "{refused:#}");
    assert_eq!(
        diagnostic["net_change"],
        serde_json::json!({ "pad": "2", "from": "GND", "to": "NEW_NET" }),
        "{refused:#}"
    );
    let acknowledged = acknowledging_each(&refused);

    let (_, plan) = served
        .sync(
            &netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
    assert_eq!(plan["changes"][0]["pad_nets"]["2"], "NEW_NET", "{plan:#}");
    let noted = &plan["diagnostics"][0];
    assert_eq!(noted["code"], "routed_pad_net_change_acknowledged");
    assert_eq!(noted["net_change"], diagnostic["net_change"]);
    assert!(plan.get("routed_copper").is_none(), "{plan:#}");
    assert!(plan.get("drc").is_none(), "{plan:#}");

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
    assert_eq!(held_r1_pad_2(&served).as_deref(), Some("NEW_NET"));
    assert_eq!(
        applied["routed_copper"],
        serde_json::json!([
            { "net": "GND", "before_count": 1, "after_count": 0 },
            { "net": "NEW_NET", "before_count": 0, "after_count": 1 },
        ]),
        "{applied:#}"
    );
    assert_eq!(
        applied["drc"],
        serde_json::json!({
            "clean": false,
            "violations": 1,
            "unconnected_items": 0,
            "schematic_parity_violations": 0,
            "project_rules_applied": cfg!(unix),
            "creepage_skipped": false,
        }),
        "{applied:#}"
    );
}

/// #38: the acknowledged plan applies only with its own revision and its
/// acknowledgement. The conflict's revision is stale for it, and its
/// revision without the acknowledgement is the conflict again; neither
/// opens a commit.
#[tokio::test]
async fn an_acknowledged_plan_applies_only_with_its_revision_and_acknowledgement() {
    let served = served_r1_under_the_gnd_pour().await;
    let netlist = netlist_moving_off("GND");
    let refused = served.dry_run(&netlist).await;
    let acknowledged = acknowledging_each(&refused);
    let (_, plan) = served
        .sync(
            &netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;
    assert_ne!(plan["plan_revision"], refused["plan_revision"]);

    for (arguments, code) in [
        (
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": refused["plan_revision"],
                "acknowledged_net_changes": acknowledged,
            }),
            "stale_plan_revision",
        ),
        (
            serde_json::json!({
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }),
            "routed_pad_net_change",
        ),
    ] {
        let (_, body) = served.sync(&netlist, arguments).await;
        assert_eq!(body["status"], "conflict", "{body:#}");
        assert!(
            body["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == code),
            "{body:#}"
        );
    }
    assert_eq!(served.sent("BeginCommit"), 0);
    assert_eq!(held_r1_pad_2(&served).as_deref(), Some("GND"));
}

/// #38: the commit stands when DRC cannot run after it. The response is
/// `applied`, flagged as an error, and says not to apply the plan again.
#[tokio::test]
async fn a_check_that_fails_after_the_commit_is_reported_on_the_applied_response() {
    let served = served_r1_under_the_gnd_pour().await;
    std::fs::remove_file(&served.drc_report).unwrap();
    let netlist = netlist_moving_off("GND");
    let acknowledged = acknowledging_each(&served.dry_run(&netlist).await);
    let (_, plan) = served
        .sync(
            &netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;

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

    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(served.sent("EndCommit"), 1);
    assert_eq!(held_r1_pad_2(&served).as_deref(), Some("NEW_NET"));
    let last = applied["diagnostics"].as_array().unwrap().last().unwrap();
    assert_eq!(last["code"], "acknowledged_apply_unchecked", "{applied:#}");
    assert!(
        last["message"]
            .as_str()
            .unwrap()
            .contains("Do not apply this plan again"),
        "{applied:#}"
    );
    assert!(applied.get("drc").is_none(), "{applied:#}");
}

/// #38: a copper zone whose net KiCad cannot serve could carry either
/// net, so the readback after the commit cannot count them. The apply
/// stands and is reported unchecked, never with counts that leave it out.
#[tokio::test]
async fn a_zone_read_back_without_a_net_leaves_the_apply_unchecked() {
    let served = ServedSync::holding(HeldBoard {
        footprints: vec![schematic_backed_resistor()],
        zones: vec![pour_without_a_net()],
        nets: vec!["VCC", "GND"],
    })
    .await;
    served.hold_drc_report(r#"{"violations":[],"unconnected_items":[],"schematic_parity":[]}"#);
    let netlist = netlist_moving_off("VCC");
    let acknowledged = acknowledging_each(&served.dry_run(&netlist).await);
    assert_eq!(acknowledged.as_array().map(Vec::len), Some(1));
    let (_, plan) = served
        .sync(
            &netlist,
            serde_json::json!({ "acknowledged_net_changes": acknowledged }),
        )
        .await;

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

    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    let last = applied["diagnostics"].as_array().unwrap().last().unwrap();
    assert_eq!(last["code"], "acknowledged_apply_unchecked", "{applied:#}");
    assert!(applied.get("routed_copper").is_none(), "{applied:#}");
}

/// #38: an acknowledgement missing a field, or carrying one it does not
/// have, is refused before anything is read.
#[tokio::test]
async fn a_malformed_acknowledgement_is_an_invalid_argument() {
    let served = served_r1_under_the_gnd_pour().await;
    for acknowledged in [
        serde_json::json!([{ "reference": "R1", "pad": "2", "from": "GND" }]),
        serde_json::json!([{
            "reference": "R1", "pad": "2", "from": "GND", "to": "NEW_NET", "code": "x"
        }]),
        serde_json::json!({ "reference": "R1" }),
    ] {
        let (is_error, refused) = served
            .sync(
                &netlist_moving_off("GND"),
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;
        assert!(is_error, "{refused:#}");
        assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
        assert!(
            refused["error"]["field"]
                .as_str()
                .unwrap()
                .starts_with("acknowledged_net_changes"),
            "{refused:#}"
        );
    }
    assert_eq!(served.sent("GetItems"), 0);
}

/// The handler refuses a malformed acknowledgement itself, for a caller
/// that reaches it without the served schema.
#[tokio::test]
async fn the_handler_refuses_a_malformed_acknowledgement_itself() {
    let context = ToolContext::new(
        crate::tools::ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        std::sync::Arc::new(crate::tools::ToolRouter::new()),
    );
    for acknowledged in [
        serde_json::json!([{ "reference": "R1", "pad": "2" }]),
        serde_json::json!([{
            "reference": "R1", "pad": "2", "from": "GND", "to": "NEW_NET", "code": "x"
        }]),
    ] {
        let result = handle_update_pcb_from_schematic(
            &serde_json::json!({
                "schematic": "/absent/carrier.kicad_sch",
                "board": "/absent/carrier.kicad_pcb",
                "acknowledged_net_changes": acknowledged,
            }),
            &context,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("the refusal is JSON text");
        };
        let refused: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
        assert_eq!(
            refused["error"]["field"], "acknowledged_net_changes",
            "{refused:#}"
        );
    }
}

/// #862 through `tools/call`: GND leaves the schematic as R1's pad 2
/// moves to NEW_NET. The plan moves the via on GND and the pour with it,
/// the via on VCC stays, and the apply sends them in the sync's commit.
#[tokio::test]
async fn the_vias_and_pour_of_a_renamed_net_move_in_the_same_commit() {
    let served = served_with_gnd_copper().await;
    let netlist = netlist_moving_off("GND");

    let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        copper_moves(&plan),
        [
            move_of("via", "via-gnd", "GND", "NEW_NET"),
            move_of("zone", &pour_kiid(), "GND", "NEW_NET"),
        ],
        "{plan:#}"
    );
    assert_eq!(plan["coverage"]["copper_net_changes"]["planned"], 2);

    let (is_error, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;
    assert!(!is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["coverage"]["copper_net_changes"]["applied"], 2);
    assert!(
        !applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["code"] == "board_readback_differs"),
        "{applied:#}"
    );
    assert_eq!(served.sent("BeginCommit"), 1);
    assert_eq!(
        held_copper_nets(&served),
        BTreeMap::from([
            ("via-gnd".to_string(), "NEW_NET".to_string()),
            ("via-vcc".to_string(), "VCC".to_string()),
            (pour_kiid(), "NEW_NET".to_string()),
        ])
    );
}

/// A zone first follows a pad KiCad's connectivity joins to it: the
/// mock says the pour touches R1's pad 1, which stays on VCC, so the
/// pour goes to VCC rather than to NEW_NET, where GND's pads went.
/// (A pad on a GND pour is on GND in a real board; the mock only shows
/// which source the plan takes the net from.)
#[tokio::test]
async fn a_zone_follows_the_pad_kicad_joins_to_it() {
    let served = served_with_gnd_copper().await;
    served
        .connected
        .lock()
        .unwrap()
        .insert(pour_kiid(), vec![r1_pad("1")]);
    let netlist = netlist_moving_off("GND");

    let (plan, _) = acknowledged_dry_run(&served, &netlist).await;

    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(
        copper_moves(&plan)[1],
        move_of("zone", &pour_kiid(), "GND", "VCC"),
        "{plan:#}"
    );
    assert!(served.sent("GetConnectedItems") >= 1);
}

/// KiCad before 10.0.1 cannot list a zone's pads: the zone follows the
/// net map, and a note says why.
#[tokio::test]
async fn an_older_kicad_moves_a_zone_by_the_net_map_with_a_note() {
    let served = served_with_gnd_copper().await;
    served.report_kicad_version(10, 0, 0);
    served
        .connected
        .lock()
        .unwrap()
        .insert(pour_kiid(), vec![r1_pad("1")]);
    let netlist = netlist_moving_off("GND");

    let (plan, _) = acknowledged_dry_run(&served, &netlist).await;

    assert_eq!(
        copper_moves(&plan)[1],
        move_of("zone", &pour_kiid(), "GND", "NEW_NET"),
        "{plan:#}"
    );
    assert!(
        plan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["code"] == "zone_pads_not_read"),
        "{plan:#}"
    );
    assert_eq!(served.sent("GetConnectedItems"), 0);
}

/// What the zones' pads say is part of the plan: a dry run that moved
/// the pour to VCC does not apply once it would go to NEW_NET.
#[tokio::test]
async fn the_plan_revision_covers_the_copper_moves() {
    let served = served_with_gnd_copper().await;
    served
        .connected
        .lock()
        .unwrap()
        .insert(pour_kiid(), vec![r1_pad("1")]);
    let netlist = netlist_moving_off("GND");
    let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");
    served.connected.lock().unwrap().clear();

    let (_, refused) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

    assert_eq!(refused["status"], "conflict", "{refused:#}");
    assert!(
        refused["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["code"] == "stale_plan_revision"),
        "{refused:#}"
    );
    assert_eq!(refused["copper_net_changes"], serde_json::json!([]));
    assert_eq!(held_copper_nets(&served)[&pour_kiid()], "GND");
}

/// The board is read back after the commit: a via or zone KiCad left on
/// its old net is reported, naming it.
#[tokio::test]
async fn copper_kicad_did_not_move_is_reported() {
    let served = served_with_gnd_copper().await;
    served
        .keeps_copper
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let netlist = netlist_moving_off("GND");
    let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;

    let (_, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

    assert_eq!(applied["status"], "applied", "{applied:#}");
    let mut differs = applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["code"] == "board_readback_differs")
        .map(|note| note["kiid"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    differs.sort();
    let mut expected = vec!["via-gnd".to_string(), pour_kiid()];
    expected.sort();
    assert_eq!(differs, expected, "{applied:#}");
    // The message reads as one sentence: no run of a source line's
    // indentation inside it.
    for note in applied["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["code"] == "board_readback_differs")
    {
        let message = note["message"].as_str().unwrap();
        assert!(!message.contains("  "), "{message:?}");
        assert!(
            message.contains("reads back on 'GND' after the sync moved it to '"),
            "{message:?}"
        );
    }
    assert_eq!(applied["coverage"]["copper_net_changes"]["planned"], 2);
    assert_eq!(applied["coverage"]["copper_net_changes"]["applied"], 0);
}

/// A readback of the moved copper that fails after the commit refuses
/// nothing: the apply answers `applied`, with a note, as an error.
#[tokio::test]
async fn a_copper_readback_that_fails_after_the_commit_is_reported() {
    let served = served_with_gnd_copper().await;
    served
        .fails_copper_readback
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let netlist = netlist_moving_off("GND");
    let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
    assert_eq!(plan["status"], "ready", "{plan:#}");

    let (is_error, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

    assert!(is_error, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert!(
        applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| {
                note["code"] == "apply_readback_unchecked"
                    && note["message"]
                        .as_str()
                        .unwrap()
                        .contains("the vias and zones it moved")
            }),
        "{applied:#}"
    );
    assert_eq!(
        held_copper_nets(&served)[&pour_kiid()],
        "NEW_NET",
        "the commit stands"
    );
}
