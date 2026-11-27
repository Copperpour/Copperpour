use super::*;

#[test]
fn exported_netlist_is_one_flattened_source_of_component_and_pad_truth() {
    let design = parse_exported_netlist(ONE_RESISTOR).expect("valid KiCad netlist");

    assert_eq!(design.components.len(), 1);
    let component = &design.components[0];
    assert_eq!(component.reference, "R1");
    assert_eq!(component.value, "10k");
    assert_eq!(component.footprint_id, "Resistor_SMD:R_0603_1608Metric");
    assert_eq!(component.symbol_path, "/sheet-uuid/symbol-uuid");
    assert_eq!(
        component.pad_nets.get("1").map(String::as_str),
        Some("/Power/VCC")
    );
    assert_eq!(component.pad_nets.get("2").map(String::as_str), Some("GND"));
    assert!(!component.dnp);
}

/// Every unit's UUID is read, in the order KiCad lists them, the first
/// unit's path being the one an added footprint is linked to. Only the
/// first was read before, so a footprint linked to another unit matched
/// nothing.
#[test]
fn every_unit_of_a_multi_unit_symbol_is_read_from_the_netlist() {
    let design = parse_exported_netlist(MULTI_UNIT_KEYED).expect("valid KiCad netlist");

    let u1 = exported(&design, "U1");
    assert_eq!(u1.symbol_path, "/d6286f98-cc91-4832-97c0-cec9c712e8d3");
    assert_eq!(
        u1.other_unit_paths,
        [
            "/ea566a33-6106-497d-ab3a-228e365858d7",
            "/1951944c-c6bb-40ae-8bca-507fb207334e",
        ]
    );
    assert_eq!(
        exported(&design, "R1").other_unit_paths,
        Vec::<String>::new()
    );
}

/// The pins KiCad left unconnected: the header's pin 2, which has a
/// no-connect flag, its pin 4, wired to nothing, and every unwired pin of
/// the comparator. Each is alone on an `unconnected-…` net.
#[test]
fn the_pins_kicad_left_unconnected_are_read_from_the_netlist() {
    let design = parse_exported_netlist(MULTI_UNIT_KEYED).expect("valid KiCad netlist");

    let pins = |reference| {
        exported(&design, reference)
            .unconnected_pins
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    };
    assert_eq!(pins("P1"), ["2", "4"]);
    assert_eq!(pins("R1"), Vec::<&str>::new());
    assert_eq!(pins("U1"), ["1", "12", "3", "5", "6", "7"]);
    // They still carry their nets, for the footprints that have them.
    assert_eq!(
        exported(&design, "P1")
            .pad_nets
            .get("2")
            .map(String::as_str),
        Some("unconnected-(P1-Pin_2-Pad2)")
    );
}

/// A pin with a no-connect flag that is wired to another pin all the same
/// is connected, and so is a pin alone on a net a label names: only a pin
/// alone on KiCad's own `unconnected-…` net, or with a no-connect pin
/// type, connects nothing.
#[test]
fn a_no_connect_pin_wired_to_another_pin_is_still_connected() {
    let design = parse_exported_netlist(
        r#"(export
  (components
    (comp (ref "P1") (value "Conn") (footprint "kk:KK254_1x04_Keyed")
      (sheetpath (names "/") (tstamps "/")) (tstamps "p1-uuid"))
    (comp (ref "R1") (value "1k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (names "/") (tstamps "/")) (tstamps "r1-uuid")))
  (nets
    (net (code "1") (name "Net-(P1-Pin_2)") (class "Default")
      (node (ref "P1") (pin "2") (pintype "passive+no_connect"))
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "TEST_POINT") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))
    (net (code "3") (name "Net-(P1-Pin_3)") (class "Default")
      (node (ref "P1") (pin "3") (pintype "no_connect")))))
"#,
    )
    .expect("valid netlist");

    assert_eq!(
        exported(&design, "P1").unconnected_pins,
        BTreeSet::from(["3".to_string()])
    );
    assert_eq!(exported(&design, "R1").unconnected_pins, BTreeSet::new());
}

/// #237: each footprint gets its sheet path's names, the `Sheetfile`
/// property and the `ki_fp_filters` property, as KiCad's Update PCB gives
/// them. The names win over the `Sheetname` property: the root's is
/// `sheet_fields`, and its footprint's sheet name is `/`.
#[test]
fn each_component_carries_its_sheet_as_update_pcb_reads_it() {
    let design = parse_exported_netlist(SHEET_FIELDS).unwrap();

    let sheets = design
        .components
        .iter()
        .map(|component| {
            (
                component.reference.as_str(),
                component.sheet.sheet_name.as_str(),
                component.sheet.sheet_file.as_str(),
                component.sheet.footprint_filters.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sheets,
        vec![
            ("C1", "/", "sheet_fields.kicad_sch", "C_*"),
            ("C2", "/CH1/", "channel.kicad_sch", "C_*"),
            ("C3", "/CH2/", "channel.kicad_sch", "C_*"),
        ]
    );
}

/// KiCad 10 writes every sheet path's names, so the fixture is edited to
/// reach the case KiCad's reader also covers: no names, and the sheet name
/// is the `Sheetname` property.
#[test]
fn a_sheet_path_without_names_takes_the_sheetname_property() {
    let design =
        parse_exported_netlist(&SHEET_FIELDS.replace("(names \"/CH1/\")", "(names \"\")")).unwrap();

    assert_eq!(design.components[1].sheet.sheet_name, "CH1");
    assert_eq!(design.components[2].sheet.sheet_name, "/CH2/");
}

/// A property named twice keeps its last value, as in KiCad's reader.
/// KiCad writes each once, so the fixture is edited to repeat C1's.
#[test]
fn a_property_named_twice_keeps_its_last_value() {
    let property = "(name \"ki_fp_filters\")\n\t\t\t\t(value \"C_*\")\n\t\t\t)";
    let repeated = SHEET_FIELDS.replacen(
        property,
        &format!(
            "{property}\n\t\t\t(property\n\t\t\t\t(name \"ki_fp_filters\")\n\t\t\t\t\
                 (value \"CP_*\")\n\t\t\t)"
        ),
        1,
    );
    let design = parse_exported_netlist(&repeated).unwrap();

    assert_eq!(design.components[0].sheet.footprint_filters, "CP_*");
    assert_eq!(design.components[1].sheet.footprint_filters, "C_*");
}

/// #682: each component carries its units, with their pins, in the order
/// KiCad writes them and its Update PCB compares them.
#[test]
fn each_component_carries_its_units_as_update_pcb_reads_them() {
    let design = parse_exported_netlist(SYMBOL_UNITS).unwrap();

    let units = design
        .components
        .iter()
        .map(|component| {
            (
                component.reference.as_str(),
                component.sheet.symbol_units.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        units,
        vec![
            ("R1", vec![unit("A", &["1", "2"])]),
            (
                "U1",
                vec![
                    unit("A", &["3", "2", "1"]),
                    unit("B", &["5", "6", "7"]),
                    unit("C", &["8", "4"]),
                ]
            ),
        ]
    );
}

/// KiCad 10 writes a `units` node for every component, so R1's is removed
/// to reach the case KiCad's reader also covers: a component without the
/// node has no units.
#[test]
fn a_component_without_units_has_none() {
    let without = ONE_RESISTOR.replace(
        "(units (unit (name \"A\") (pins (pin (num \"1\")) (pin (num \"2\")))))",
        "",
    );
    assert_ne!(without, ONE_RESISTOR);

    let design = parse_exported_netlist(&without).unwrap();

    assert_eq!(design.components[0].sheet.symbol_units, Vec::new());
}

/// #507: one footprint-less symbol failed the whole sync with "KiCad
/// netlist node is missing footprint" — nothing about which component,
/// every other component blocked with it.
#[test]
fn a_component_without_a_footprint_is_reported_not_fatal() {
    let design = parse_exported_netlist(UNASSIGNED).expect("a real export parses");

    let mut placed: Vec<&str> = design
        .components
        .iter()
        .map(|component| component.reference.as_str())
        .collect();
    placed.sort_unstable();
    assert_eq!(placed, ["C1", "R2"]);
    assert_eq!(design.unassigned.len(), 1);
    let unassigned = &design.unassigned[0];
    assert_eq!(unassigned.reference, "R1");
    assert_eq!(unassigned.value, "R");
    assert_eq!(unassigned.lib_id.as_deref(), Some("Device:R"));
    assert!(unassigned.symbol_path.starts_with('/'));
    // Its wired pin names a component with no pads; the net node is
    // dropped, and C1's side of the same net is kept.
    let c1 = design
        .components
        .iter()
        .find(|component| component.reference == "C1")
        .unwrap();
    assert_eq!(
        c1.pad_nets.get("1").map(String::as_str),
        Some("Net-(C1-Pad1)")
    );
}

/// The KiCad-written export carries the fields KiCad's Update PCB copies,
/// in the order KiCad lists them: the custom ones, Datasheet and
/// Description, an empty one included, and never Footprint (#788).
#[test]
fn a_kicad_export_carries_the_symbol_fields_update_pcb_copies() {
    let design = parse_exported_netlist(include_str!(
        "../../../../tests/fixtures/symbol_fields_kicad10.net"
    ))
    .expect("a real export parses");

    let fields = |reference: &str| {
        design
            .components
            .iter()
            .find(|component| component.reference == reference)
            .unwrap()
            .fields
            .clone()
    };
    assert_eq!(
        fields("C1"),
        [
            symbol_field("LCSC", "C788001"),
            symbol_field("MPN", "MPN-788-C1"),
            symbol_field("Datasheet", ""),
            symbol_field("Description", "Unpolarized capacitor"),
        ]
    );
    assert_eq!(
        fields("R1"),
        [
            symbol_field("LCSC", "C788002"),
            symbol_field("MPN", ""),
            symbol_field("Datasheet", ""),
            symbol_field("Description", "Resistor"),
        ]
    );
}

/// A name the export repeats keeps its first position and its last value,
/// as KiCad's netlist reader keeps it. Two values for one name would
/// otherwise make every sync plan the same update again.
#[test]
fn a_repeated_field_keeps_its_first_place_and_its_last_value() {
    let source = include_str!("../../../../tests/fixtures/symbol_fields_kicad10.net");
    let repeated = source.replacen(
        "(name \"Description\") \"Unpolarized capacitor\")",
        "(name \"Description\") \"Unpolarized capacitor\")\n\t\t\t\t(field\n\t\t\t\t\t(name \"LCSC\") \"C788009\")",
        1,
    );
    assert_ne!(repeated, source);

    let design = parse_exported_netlist(&repeated).unwrap();

    let c1 = design
        .components
        .iter()
        .find(|component| component.reference == "C1")
        .unwrap();
    assert_eq!(
        c1.fields,
        [
            symbol_field("LCSC", "C788009"),
            symbol_field("MPN", "MPN-788-C1"),
            symbol_field("Datasheet", ""),
            symbol_field("Description", "Unpolarized capacitor"),
        ]
    );
}

/// Order-sensitive: the second `R1` is unassigned like the first.
#[test]
fn a_repeated_unassigned_reference_is_refused() {
    refuses_duplicate(&with_duplicate_of("R1", "R1", "R1"));
}

/// Unassigned `R1` first, then an assigned component renamed to `R1`.
#[test]
fn an_assigned_repeat_of_an_unassigned_reference_is_refused() {
    refuses_duplicate(&with_duplicate_of("R2", "R1", "R1"));
}

/// Assigned `C1` first, then an unassigned component renamed to `C1`.
#[test]
fn an_unassigned_repeat_of_an_assigned_reference_is_refused() {
    refuses_duplicate(&with_duplicate_of("R1", "C1", "C1"));
}

/// Assigned then assigned, the case the parser already refused.
#[test]
fn a_repeated_assigned_reference_is_still_refused() {
    refuses_duplicate(&with_duplicate_of("R2", "C1", "R2"));
}

/// The helper builds what it claims: the duplicated block parses as a
/// second component when it is given a fresh reference instead.
#[test]
fn the_duplicate_helper_produces_a_parseable_export() {
    let design = parse_exported_netlist(&with_duplicate_of("R2", "R3", "R2")).unwrap();
    assert_eq!(design.components.len(), 3);
    assert_eq!(design.unassigned.len(), 1);
}

#[test]
fn empty_and_duplicate_component_exports_are_rejected() {
    let empty = parse_exported_netlist("(export (components) (nets))")
        .unwrap_err()
        .to_string();
    assert!(empty.contains("zero components"), "{empty}");

    let duplicate = r#"
(export
  (components
    (comp (ref "R1") (value "1k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (tstamps "/one/")) (tstamps "one"))
    (comp (ref "R1") (value "2k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (tstamps "/two/")) (tstamps "two")))
  (nets))
"#;
    let duplicate = parse_exported_netlist(duplicate).unwrap_err().to_string();
    assert!(
        duplicate.contains("duplicate component reference R1"),
        "{duplicate}"
    );
}

/// The parser keeps every net a pin is on, a footprint-less part's
/// included, as KiCad's netlist holds it.
#[test]
fn the_netlist_keeps_the_net_of_a_part_with_no_footprint() {
    let design = parse_exported_netlist(UNASSIGNED).unwrap();
    assert!(!design.net_names.is_empty());
    let footprint_nets = design
        .components
        .iter()
        .flat_map(|component| component.pad_nets.values().cloned())
        .collect::<BTreeSet<_>>();
    assert!(
        design.net_names.is_superset(&footprint_nets),
        "{:?} lacks some of {footprint_nets:?}",
        design.net_names
    );
    // R1 has no footprint; its pad 2's net is on no other pin.
    assert!(
        design.net_names.contains("unconnected-(R1-Pad2)"),
        "{:?}",
        design.net_names
    );
    assert!(!footprint_nets.contains("unconnected-(R1-Pad2)"));
}

/// Issue #82: a KiCad-resaved hierarchy with root-page symbols, two
/// instances of a reused sheet, reused nested sheets, and an unassigned
/// footprint synchronizes existing footprints whose stored PCB paths carry
/// the requested root sheet UUID prefix, while preserving their stored
/// paths and poses and keeping native root-relative paths for additions.
#[tokio::test]
async fn root_prefixed_hierarchy_syncs_and_preserves_stored_paths_and_poses() {
    use copperpour_ipc::gen::kiapi;
    const ROOT_UUID: &str = "8631f762-28ca-4b46-9a77-580c49a715b8";
    const R1_REL: &str = "/cb73633d-0588-4c05-8386-c6f9a836d1b8";
    const R2_UNASSIGNED_REL: &str = "/564f8de9-7101-4413-aa04-9c4146998ec0";
    const R3_REL: &str =
        "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/bc8b0001-33da-4719-a210-a76cec91947d";
    const R4_REL: &str =
        "/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d";
    const C1_REL: &str = "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3";
    const C2_REL: &str = "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3";
    const C3_REL: &str = "/c311205d-f32d-47da-97d6-904ff928174e/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3";
    const C4_REL: &str = "/c311205d-f32d-47da-97d6-904ff928174e/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3";

    let rooted = |rel: &str| format!("/{ROOT_UUID}{rel}");
    let make_fp = |kiid: &str,
                   reference: &str,
                   value: &str,
                   fp_id: &str,
                   path: &str,
                   x: f64,
                   y: f64,
                   angle: f64| {
        let source = include_str!("../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let pads = crate::tools::pcb_components::extract_pad_definitions(source).unwrap();
        let graphics = crate::tools::pcb_components::extract_graphic_definitions(source).unwrap();
        let fields = crate::tools::pcb_components::extract_field_placement(source);
        let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
            fp_id, reference, value, &pads, &graphics, &fields, x, y, angle, "F.Cu",
        )
        .unwrap();
        let mut fp = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            item.value.as_slice(),
        )
        .unwrap();
        fp.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        fp.symbol_path = Some(kiapi::common::types::SheetPath {
            path: path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| kiapi::common::types::Kiid {
                    value: s.to_string(),
                })
                .collect(),
            path_human_readable: String::new(),
        });
        copperpour_ipc::builders::pack_any(&fp, "kiapi.board.types.FootprintInstance")
    };

    // Mix rooted and root-relative paths on the board, plus a reference
    // rename on rooted R3 (board holds R30 -> schematic has R3) and on
    // root-relative C2 (board holds C20 -> schematic has C2), leave C4
    // absent so it is added with a native root-relative path, and keep
    // rooted R2 (unassigned) on the board.
    let held = vec![
        make_fp(
            "fp-r1",
            "R1",
            "4k7",
            "Resistor_SMD:R_0603_1608Metric",
            &rooted(R1_REL),
            12.5,
            22.5,
            90.0,
        ),
        make_fp(
            "fp-r2",
            "R2",
            "UNASSIGNED",
            "Resistor_SMD:R_0603_1608Metric",
            &rooted(R2_UNASSIGNED_REL),
            20.0,
            20.0,
            0.0,
        ),
        make_fp(
            "fp-r3",
            "R30",
            "1k",
            "Resistor_SMD:R_0603_1608Metric",
            &rooted(R3_REL),
            31.0,
            41.0,
            180.0,
        ),
        make_fp(
            "fp-r4",
            "R4",
            "1k",
            "Resistor_SMD:R_0603_1608Metric",
            R4_REL,
            42.0,
            52.0,
            0.0,
        ),
        make_fp(
            "fp-c1",
            "C1",
            "47n",
            "Capacitor_SMD:C_0603_1608Metric",
            &rooted(C1_REL),
            53.0,
            63.0,
            270.0,
        ),
        make_fp(
            "fp-c2",
            "C20",
            "100n",
            "Capacitor_SMD:C_0603_1608Metric",
            C2_REL,
            64.0,
            74.0,
            0.0,
        ),
        make_fp(
            "fp-c3",
            "C3",
            "100n",
            "Capacitor_SMD:C_0603_1608Metric",
            &rooted(C3_REL),
            75.0,
            85.0,
            0.0,
        ),
    ];

    let served = ServedSync::holding(HeldBoard {
        footprints: held,
        ..Default::default()
    })
    .await;
    served.hold_board_text(
        "(kicad_pcb\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r1\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r2\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r3\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r4\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c1\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c2\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c3\"))\n\
             )",
    );
    let dir = served.board.parent().unwrap();
    let root_sch = dir.join("root.kicad_sch");
    std::fs::write(
        dir.join("root.kicad_pro"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"),
    )
    .unwrap();
    std::fs::write(
        &root_sch,
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        dir.join("stage.kicad_sch"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        dir.join("leaf.kicad_sch"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        &served.exported,
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
    )
    .unwrap();
    let (is_error, plan) = served
        .call(serde_json::json!({
            "schematic": root_sch.to_string_lossy(),
            "board": served.board.to_string_lossy(),
            "dry_run": true,
        }))
        .await;
    assert!(!is_error, "{plan:#}");
    assert_eq!(plan["status"], "ready", "{plan:#}");
    assert_eq!(plan["coverage"]["conflicts"]["planned"], 0, "{plan:#}");
    assert_eq!(
        plan["coverage"]["footprints_added"]["planned"], 1,
        "{plan:#}"
    );
    assert_eq!(
        plan["coverage"]["board_only_preserved"]["planned"], 0,
        "{plan:#}"
    );
    assert_eq!(
        plan["coverage"]["unassigned_footprint"]["planned"], 1,
        "{plan:#}"
    );
    assert_eq!(plan["unassigned_footprints"][0]["reference"], "R2");
    assert_eq!(plan["unassigned_footprints"][0]["board_state"], "kept");

    let (apply_err, applied) = served
        .call(serde_json::json!({
            "schematic": root_sch.to_string_lossy(),
            "board": served.board.to_string_lossy(),
            "dry_run": false,
            "expected_plan_revision": plan["plan_revision"],
        }))
        .await;
    assert!(!apply_err, "{applied:#}");
    assert_eq!(applied["status"], "applied", "{applied:#}");

    // Verify rooted paths and poses were preserved on updated footprints.
    {
        let held_after = served.held.lock().unwrap();
        let find_held = |kiid: &str| {
            held_after
                .iter()
                .map(|any| {
                    kiapi::board::types::FootprintInstance::decode(any.value.as_slice()).unwrap()
                })
                .find(|fp| fp.id.as_ref().map(|id| id.value.as_str()) == Some(kiid))
                .unwrap()
        };
        let r1 = find_held("fp-r1");
        assert_eq!(
            board_symbol_path(r1.symbol_path.as_ref()).as_deref(),
            Some(rooted(R1_REL).as_str())
        );
        assert_eq!(r1.position.as_ref().unwrap().x_nm, 12_500_000);
        assert_eq!(r1.position.as_ref().unwrap().y_nm, 22_500_000);
        assert_eq!(r1.orientation.as_ref().unwrap().value_degrees, 90.0);

        let r3 = find_held("fp-r3");
        assert_eq!(field_text(&r3.reference_field), "R3");
        assert_eq!(
            board_symbol_path(r3.symbol_path.as_ref()).as_deref(),
            Some(rooted(R3_REL).as_str())
        );
        assert_eq!(r3.position.as_ref().unwrap().x_nm, 31_000_000);
        assert_eq!(r3.orientation.as_ref().unwrap().value_degrees, 180.0);

        let c1 = find_held("fp-c1");
        assert_eq!(
            board_symbol_path(c1.symbol_path.as_ref()).as_deref(),
            Some(rooted(C1_REL).as_str())
        );
        assert_eq!(c1.position.as_ref().unwrap().x_nm, 53_000_000);
        assert_eq!(c1.orientation.as_ref().unwrap().value_degrees, 270.0);

        let c2 = find_held("fp-c2");
        assert_eq!(field_text(&c2.reference_field), "C2");
        assert_eq!(
            board_symbol_path(c2.symbol_path.as_ref()).as_deref(),
            Some(C2_REL)
        );
    }

    // Verify newly added C4 carries native root-relative path.
    let added = served.sent_footprints();
    assert_eq!(added.len(), 1);
    assert_eq!(field_text(&added[0].reference_field), "C4");
    assert_eq!(
        board_symbol_path(added[0].symbol_path.as_ref()).as_deref(),
        Some(C4_REL)
    );

    // Verify a follow-up dry run against the resulting board (carrying the
    // preserved rooted paths + newly added root-relative C4) is a clean
    // noop — rooted paths do not trigger spurious updates.
    let mut after_apply = served.held.lock().unwrap().clone();
    let mut c4 = added[0].clone();
    c4.id = Some(kiapi::common::types::Kiid {
        value: "fp-c4".to_string(),
    });
    after_apply.push(copperpour_ipc::builders::pack_any(
        &c4,
        "kiapi.board.types.FootprintInstance",
    ));
    let follow_up_served = ServedSync::holding(HeldBoard {
        footprints: after_apply,
        ..Default::default()
    })
    .await;
    let follow_dir = follow_up_served.board.parent().unwrap();
    let follow_root = follow_dir.join("root.kicad_sch");
    std::fs::write(
        follow_dir.join("root.kicad_pro"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"),
    )
    .unwrap();
    std::fs::write(
        &follow_root,
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        follow_dir.join("stage.kicad_sch"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        follow_dir.join("leaf.kicad_sch"),
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        &follow_up_served.exported,
        include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
    )
    .unwrap();
    let (noop_err, follow_up) = follow_up_served
        .call(serde_json::json!({
            "schematic": follow_root.to_string_lossy(),
            "board": follow_up_served.board.to_string_lossy(),
            "dry_run": true,
        }))
        .await;
    assert!(!noop_err, "{follow_up:#}");
    assert_eq!(follow_up["status"], "noop", "{follow_up:#}");
}

/// Issue #82: foreign roots, other sheets, other symbols, and simultaneous
/// rooted + root-relative duplicate aliases are refused before any PCB
/// mutation over served MCP dispatch.
#[tokio::test]
async fn foreign_root_other_sheet_other_symbol_and_duplicate_aliases_refuse_before_mutation() {
    use copperpour_ipc::gen::kiapi;
    const ROOT_UUID: &str = "8631f762-28ca-4b46-9a77-580c49a715b8";
    const FOREIGN_ROOT: &str = "99999999-9999-4999-8999-999999999999";
    const R1_REL: &str = "/cb73633d-0588-4c05-8386-c6f9a836d1b8";
    const R4_REL: &str =
        "/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d";

    let make_fp = |kiid: &str, reference: &str, path: &str| {
        let source = include_str!("../../../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let pads = crate::tools::pcb_components::extract_pad_definitions(source).unwrap();
        let graphics = crate::tools::pcb_components::extract_graphic_definitions(source).unwrap();
        let fields = crate::tools::pcb_components::extract_field_placement(source);
        let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
            "Resistor_SMD:R_0603_1608Metric",
            reference,
            "10k",
            &pads,
            &graphics,
            &fields,
            10.0,
            10.0,
            0.0,
            "F.Cu",
        )
        .unwrap();
        let mut fp = kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        fp.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        fp.symbol_path = Some(kiapi::common::types::SheetPath {
            path: path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| kiapi::common::types::Kiid {
                    value: s.to_string(),
                })
                .collect(),
            path_human_readable: String::new(),
        });
        copperpour_ipc::builders::pack_any(&fp, "kiapi.board.types.FootprintInstance")
    };

    let cases: &[(&str, Vec<prost_types::Any>, &str)] = &[
        (
            "foreign_root",
            vec![make_fp("fp-r1", "R1", &format!("/{FOREIGN_ROOT}{R1_REL}"))],
            "reference_identity_conflict",
        ),
        (
            "other_sheet",
            vec![make_fp("fp-r3", "R3", &format!("/{ROOT_UUID}{R4_REL}"))],
            "reference_identity_conflict",
        ),
        (
            "other_symbol",
            vec![make_fp(
                "fp-r1",
                "R1",
                &format!("/{ROOT_UUID}/00000000-0000-4000-8000-000000000000"),
            )],
            "reference_identity_conflict",
        ),
        (
            "duplicate_alias_rooted_and_relative",
            vec![
                make_fp("fp-r1-rooted", "R1", &format!("/{ROOT_UUID}{R1_REL}")),
                make_fp("fp-r1-rel", "R1_DUP", R1_REL),
            ],
            "duplicate_board_identity",
        ),
    ];

    for (label, held, expected_code) in cases {
        let served = ServedSync::holding(HeldBoard {
            footprints: held.clone(),
            ..Default::default()
        })
        .await;
        let dir = served.board.parent().unwrap();
        let root_sch = dir.join("root.kicad_sch");
        std::fs::write(
            dir.join("root.kicad_pro"),
            include_bytes!(
                "../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"
            ),
        )
        .unwrap();
        std::fs::write(
            &root_sch,
            include_bytes!(
                "../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("stage.kicad_sch"),
            include_bytes!(
                "../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("leaf.kicad_sch"),
            include_bytes!(
                "../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"
            ),
        )
        .unwrap();
        std::fs::write(
            &served.exported,
            include_bytes!("../../../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
        )
        .unwrap();

        let (_dry_err, dry) = served
            .call(serde_json::json!({
                "schematic": root_sch.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": true,
            }))
            .await;
        assert_eq!(dry["status"], "conflict", "{label}: {dry:#}");
        let codes: Vec<&str> = dry["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["code"].as_str())
            .collect();
        assert!(
            codes.contains(expected_code),
            "{label}: expected {expected_code} in {codes:?}"
        );

        // Attempting apply with the conflict's plan revision must also
        // refuse before any IPC commit or item mutation.
        let (_apply_err, applied) = served
            .call(serde_json::json!({
                "schematic": root_sch.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": dry["plan_revision"],
            }))
            .await;
        assert_eq!(applied["status"], "conflict", "{label}: {applied:#}");
        assert_eq!(
            served.sent("BeginCommit"),
            0,
            "{label}: must not begin commit"
        );
        assert_eq!(
            served.sent("CreateItems"),
            0,
            "{label}: must not create items"
        );
        assert_eq!(
            served.sent("UpdateItems"),
            0,
            "{label}: must not update items"
        );
    }
}

#[test]
fn normalize_symbol_path_strips_only_exact_root_prefix() {
    let root = Some("root-uuid");
    assert_eq!(normalize_symbol_path("/root-uuid/sym", root), "/sym");
    assert_eq!(
        normalize_symbol_path("/root-uuid/sheet/sym", root),
        "/sheet/sym"
    );
    assert_eq!(
        normalize_symbol_path("/root-uuid/s1/s2/sym", root),
        "/s1/s2/sym"
    );
    assert_eq!(normalize_symbol_path("/sym", root), "/sym");
    assert_eq!(normalize_symbol_path("/sheet/sym", root), "/sheet/sym");
    assert_eq!(
        normalize_symbol_path("/other-root/sym", root),
        "/other-root/sym"
    );
    assert_eq!(
        normalize_symbol_path("/root-uuid-extra/sym", root),
        "/root-uuid-extra/sym"
    );
    assert_eq!(normalize_symbol_path("/root-uuid", root), "/root-uuid");
    assert_eq!(normalize_symbol_path("/root-uuid/", root), "/root-uuid/");
    assert_eq!(
        normalize_symbol_path("/root-uuid//sym", root),
        "/root-uuid//sym"
    );
    assert_eq!(
        normalize_symbol_path("/root-uuid/sym", None),
        "/root-uuid/sym"
    );
    assert_eq!(
        normalize_symbol_path("/root-uuid/sym", Some("")),
        "/root-uuid/sym"
    );
}

#[test]
fn rooted_skipped_and_renamed_unassigned_footprints_match_by_normalized_path() {
    let design = ExportedDesign {
        root_sheet_uuid: Some("root-uuid".to_string()),
        components: Vec::new(),
        skipped: vec![SkippedComponent {
            reference: "H1".to_string(),
            symbol_path: "/h1-sym".to_string(),
        }],
        unassigned: vec![UnassignedComponent {
            reference: "R2_NEW".to_string(),
            value: "UNASSIGNED".to_string(),
            lib_id: Some("Device:R".to_string()),
            symbol_path: "/r2-sym".to_string(),
        }],
        net_names: BTreeSet::new(),
    };
    let board = board_with(vec![
        board_resistor("H1_OLD", Some("/root-uuid/h1-sym")),
        board_resistor("R2_OLD", Some("/root-uuid/r2-sym")),
    ]);
    let plan = plan_sync("netlist", &design, &board);
    assert_eq!(plan.status, PlanStatus::Noop, "{:?}", plan.diagnostics);
    // H1_OLD is claimed by the excluded H1 only through the normalized
    // path: its reference differs (#870).
    assert_eq!(plan.without_symbols.len(), 1, "{:?}", plan.without_symbols);
    assert_eq!(plan.without_symbols[0].reference, "H1_OLD");
    assert_eq!(
        plan.without_symbols[0].reason,
        WithoutSymbolReason::ExcludedFromBoard
    );
    assert_eq!(plan.unassigned.len(), 1);
    assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Kept);
    assert_eq!(plan.counts.board_only_preserved.planned, 1);
}
