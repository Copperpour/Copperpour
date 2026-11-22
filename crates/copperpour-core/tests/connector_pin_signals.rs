use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};

const TOOL: &str = "check_connector_pin_signals";
const CONNECTORS: &str = include_str!("fixtures/connector_pin_signals_kicad10.kicad_sch");
const SHEET_PINS: &str = include_str!("fixtures/pin_net_names_kicad10.kicad_sch");
const WIRE_STUB: &str = include_str!("fixtures/rigid_block_move_kicad10.kicad_sch");

async fn call_tool(tool: &str, sheet: &str, mut args: Value, is_error: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.kicad_sch");
    std::fs::write(&path, sheet).unwrap();
    args["schematic"] = json!(path);
    let h = McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 769, "method": "tools/call",
        "params": {"name": tool, "arguments": args}});
    let run = async || {
        let result = h
            .handle_message(request.clone())
            .await
            .unwrap()
            .result
            .unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(result["isError"] == true, is_error, "{text}");
        assert_eq!(std::fs::read(&path).unwrap(), sheet.as_bytes());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        text.to_owned()
    };
    let first = run().await;
    if !is_error {
        assert_eq!(run().await, first);
    }
    first
}

async fn call(sheet: &str, args: Value, is_error: bool) -> String {
    call_tool(TOOL, sheet, args, is_error).await
}

async fn check(sheet: &str, args: Value) -> Value {
    serde_json::from_str(&call(sheet, args, false).await).unwrap()
}

fn context(answer: &Value, connector: usize, pin: usize) -> &Value {
    &answer["connectors"][connector]["pins"][pin - 1]["context"]
}

fn assert_mark(mark: &Value, name: &str, distance_mm: f64) {
    assert_eq!(mark["name"], name);
    assert!(
        (mark["distance_mm"].as_f64().unwrap() - distance_mm).abs() < 1e-6,
        "{mark}"
    );
}

#[tokio::test]
async fn reports_the_fixture_pin_statuses_and_drawing_context() {
    let answer = check(CONNECTORS, json!({})).await;
    assert_eq!(answer["source"], "saved_schematic");
    let connectors = answer["connectors"].as_array().unwrap();
    assert_eq!(connectors.len(), 3);
    type ExpectedPin<'a> = (&'a str, &'a [&'a str]);
    let expected: &[(&str, &[ExpectedPin<'_>])] = &[
        (
            "J1",
            &[
                ("identified", &["label"]),
                ("unidentified", &[]),
                ("no_connect", &[]),
                ("identified", &["power_symbol"]),
                ("identified", &["label"]),
                ("unconnected", &[]),
            ],
        ),
        (
            "J2",
            &[
                ("identified", &["power_symbol", "pin_name"]),
                ("identified", &["label", "pin_name"]),
                ("no_connect", &[]),
                ("identified", &["pin_name"]),
                ("identified", &["power_symbol", "pin_name"]),
                ("no_connect", &[]),
            ],
        ),
        ("J3", &[("unidentified", &[]), ("identified", &["label"])]),
    ];
    for (connector, (reference, expected_pins)) in connectors.iter().zip(expected) {
        assert_eq!(connector["reference"], *reference);
        assert_eq!(connector["selected_by"], "reference_prefix");
        let pins = connector["pins"].as_array().unwrap();
        assert_eq!(pins.len(), expected_pins.len());
        for (i, (pin, (status, evidence))) in pins.iter().zip(*expected_pins).enumerate() {
            assert_eq!(pin["pin"], (i + 1).to_string());
            assert_eq!(pin["status"], *status, "{reference}: {pin}");
            assert_eq!(pin["evidence"], json!(evidence), "{reference}: {pin}");
        }
    }
    assert_mark(&context(&answer, 0, 1)["nearest_label"], "SIG_A", 5.08);
    assert_eq!(context(&answer, 0, 3)["no_connect"], "flag");
    assert_mark(
        &context(&answer, 0, 4)["nearest_power_symbol"],
        "GND",
        10.16,
    );
    assert_mark(&context(&answer, 0, 5)["nearest_label"], "SIG_B", 66.04);
    assert_eq!(context(&answer, 1, 1)["pin_name"], "VCC");
    assert_eq!(
        context(&answer, 1, 1)["nearest_power_symbol"]["name"],
        "+3V3"
    );
    assert_eq!(context(&answer, 1, 2)["pin_name"], "SWDIO");
    assert_eq!(context(&answer, 1, 2)["nearest_label"]["name"], "SWDIO");
    assert_eq!(context(&answer, 1, 4)["pin_name"], "SWCLK");
    assert_eq!(
        context(&answer, 1, 5)["nearest_power_symbol"]["name"],
        "GND"
    );
    assert_mark(&context(&answer, 2, 2)["nearest_label"], "SWDIO", 10.16);

    assert_eq!(answer["unidentified_count"], 2);
    let findings = answer["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["reference"], "J1");
    assert_eq!(findings[0]["pin"], "2");
    assert_eq!(findings[0]["net"], "Net-(J1-Pin_2)");
    assert_eq!(findings[0]["other_pin_names"], json!([]));
    assert_eq!(findings[1]["reference"], "J3");
    assert_eq!(findings[1]["pin"], "1");
    assert_eq!(findings[1]["net"], "Net-(J2-SWCLK)");
    let other_names = findings[1]["other_pin_names"].as_array().unwrap();
    assert_eq!(other_names.len(), 1);
    assert_eq!(other_names[0]["reference"], "J2");
    assert_eq!(other_names[0]["pin"], "4");
    assert_eq!(other_names[0]["name"], "SWCLK");
}

#[tokio::test]
async fn references_replace_the_default_and_do_not_select_power_symbols() {
    let answer = check(CONNECTORS, json!({"references": ["R1", "R1", "#PWR06"]})).await;
    assert_eq!(answer["unmatched_references"], json!(["#PWR06"]));
    let connectors = answer["connectors"].as_array().unwrap();
    assert_eq!(connectors.len(), 1);
    assert_eq!(connectors[0]["reference"], "R1");
    assert_eq!(connectors[0]["selected_by"], "references");
    assert_eq!(
        connectors[0]["pins"][0]["evidence"],
        json!(["power_symbol"])
    );
    assert_eq!(connectors[0]["pins"][1]["status"], "unidentified");
    let empty = check(CONNECTORS, json!({"references": []})).await;
    assert_eq!(empty["connectors"], json!([]));
}

#[tokio::test]
async fn the_library_power_marker_excludes_even_a_power_symbol_with_a_j_reference() {
    let sheet = CONNECTORS.replace("\"#PWR06\"", "\"J11\"");
    let default = check(&sheet, json!({})).await;
    assert_eq!(default["connectors"].as_array().unwrap().len(), 3);
    let explicit = check(&sheet, json!({"references": ["J11"]})).await;
    assert_eq!(explicit["connectors"], json!([]));
    assert_eq!(explicit["unmatched_references"], json!(["J11"]));
}

#[tokio::test]
async fn a_sheet_pin_can_identify_a_signal_without_opening_the_subsheet() {
    let answer = check(SHEET_PINS, json!({"references": ["R11"]})).await;
    let pin = &answer["connectors"][0]["pins"][0];
    assert_eq!(pin["pin"], "1");
    assert_eq!(pin["status"], "identified");
    assert_eq!(pin["evidence"], json!(["sheet_pin"]));
}

#[tokio::test]
async fn malformed_selection_is_refused_without_writing() {
    let body: Value =
        serde_json::from_str(&call(CONNECTORS, json!({"references": [9]}), true).await).unwrap();
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "references[0]");
}

#[tokio::test]
async fn refuses_a_non_schematic_instead_of_reporting_no_findings() {
    let body: Value =
        serde_json::from_str(&call("(kicad_pcb (version 20260306))", json!({}), true).await)
            .unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert_eq!(body["error"]["reason"], "expected a KiCad schematic");
}

#[tokio::test]
async fn refuses_unavailable_pin_geometry_instead_of_reporting_an_incomplete_check() {
    let sheet = CONNECTORS.replacen(
        "(lib_id \"Connector_Generic:Conn_01x06\")",
        "(lib_id \"Missing:Connector\")",
        1,
    );
    assert_ne!(sheet, CONNECTORS);
    let body: Value = serde_json::from_str(&call(&sheet, json!({}), true).await).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert!(body["error"]["reason"]
        .as_str()
        .unwrap()
        .contains("unresolved library pin geometry"));
}

#[tokio::test]
async fn refuses_a_pin_without_a_position_even_when_its_library_definition_exists() {
    let sheet = CONNECTORS.replacen("(at -5.08 2.54 0)", "", 1);
    assert_ne!(sheet, CONNECTORS);
    let body: Value = serde_json::from_str(&call(&sheet, json!({}), true).await).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert!(body["error"]["reason"]
        .as_str()
        .unwrap()
        .contains("invalid geometry"));
}

#[tokio::test]
async fn refuses_an_unplaceable_symbol_even_when_the_library_is_available() {
    let sheet = CONNECTORS.replacen(
        "(lib_id \"Connector_Generic:Conn_01x06\")\n\t\t(at ",
        "(lib_id \"Connector_Generic:Conn_01x06\")\n\t\t(not_at ",
        1,
    );
    assert_ne!(sheet, CONNECTORS);
    let body: Value = serde_json::from_str(&call(&sheet, json!({}), true).await).unwrap();
    assert_eq!(body["error"]["kind"], "malformed_file");
    assert!(body["error"]["reason"]
        .as_str()
        .unwrap()
        .contains("lacks a library ID or position"));
}

#[tokio::test]
async fn prefix_excludes_jp_but_an_explicit_reference_includes_it() {
    let sheet = CONNECTORS.replace("\"J3\"", "\"JP3\"");
    let default = check(&sheet, json!({})).await;
    assert_eq!(default["connectors"].as_array().unwrap().len(), 2);
    let explicit = check(&sheet, json!({"references":["JP3"]})).await;
    assert_eq!(explicit["connectors"][0]["reference"], "JP3");
}

#[tokio::test]
async fn label_distance_stays_on_the_wire_run_before_same_name_merge() {
    // One same-named label is much closer in space on a separate run.
    let sheet = CONNECTORS.replacen(
        "(label \"SIG_B\"",
        "(wire (pts (xy 121.92 82.55) (xy 124.46 82.55)))\n\t\
         (label \"SIG_B\" (at 121.92 82.55 0))\n\t(label \"SIG_B\"",
        1,
    );
    let answer = check(&sheet, json!({})).await;
    assert_mark(&context(&answer, 0, 5)["nearest_label"], "SIG_B", 66.04);
}

#[tokio::test]
async fn global_and_hierarchical_labels_are_evidence() {
    for kind in ["global_label", "hierarchical_label"] {
        let sheet = CONNECTORS.replacen("(label \"SIG_A\"", &format!("({kind} \"SIG_A\""), 1);
        let answer = check(&sheet, json!({})).await;
        assert_eq!(
            answer["connectors"][0]["pins"][0]["evidence"],
            json!(["label"]),
            "{kind}"
        );
        assert_mark(&context(&answer, 0, 1)["nearest_label"], "SIG_A", 5.08);
    }
}

#[tokio::test]
async fn a_no_connect_pin_type_needs_no_flag() {
    let answer = check(SHEET_PINS, json!({"references":["U1"]})).await;
    let pin = &answer["connectors"][0]["pins"][3];
    assert_eq!(pin["pin"], "4");
    assert_eq!(pin["status"], "no_connect");
    assert_eq!(pin["context"]["no_connect"], "pin_type");
}

#[tokio::test]
async fn an_unlabeled_stub_reports_kicads_single_pin_net_name() {
    let answer = check(WIRE_STUB, json!({"references": ["R4"]})).await;
    let pin = &answer["connectors"][0]["pins"][0];
    assert_eq!(pin["pin"], "1");
    assert_eq!(pin["status"], "unidentified");
    // Verified by netlist export of the unchanged fixture in KiCad 10.0.8.
    assert_eq!(pin["context"]["net"], "unconnected-(R4-Pad1)");
}

#[tokio::test]
async fn component_net_queries_still_omit_single_pin_net_names() {
    let text = call_tool(
        "get_component_nets",
        WIRE_STUB,
        json!({"reference": "R4"}),
        false,
    )
    .await;
    let answer: Value = serde_json::from_str(&text).unwrap();
    let pin = &answer["pins"][0];
    assert_eq!(pin["pin"], "1");
    assert_eq!(pin["net"], Value::Null);
}

#[tokio::test]
async fn the_fixture_retains_its_legacy_connectivity_answers() {
    for (reference, expected) in [
        (
            "J1",
            json!(["SIG_A", "Net-(J1-Pin_2)", null, "GND", "SIG_B", null]),
        ),
        (
            "J2",
            json!(["+3V3", "SWDIO", null, "Net-(J2-SWCLK)", "GND", null]),
        ),
        ("J3", json!(["Net-(J2-SWCLK)", "SWDIO"])),
    ] {
        let text = call_tool(
            "get_component_nets",
            CONNECTORS,
            json!({"reference": reference}),
            false,
        )
        .await;
        let answer: Value = serde_json::from_str(&text).unwrap();
        let pins = answer["pins"].as_array().unwrap();
        let actual: Vec<_> = (1..=expected.as_array().unwrap().len())
            .map(|number| {
                let number = number.to_string();
                let pin = pins
                    .iter()
                    .find(|pin| pin["pin"] == number.as_str())
                    .unwrap();
                &pin["net"]
            })
            .collect();
        assert_eq!(json!(actual), expected, "{reference}");
        for pin in pins {
            let mut keys: Vec<_> = pin
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["name", "net", "pin", "x", "y"]);
        }
    }
    for (tool, args, key) in [
        ("find_orphan_items", json!({}), "orphans"),
        (
            "validate_component_connections",
            json!({"references": ["J1", "J2", "J3"]}),
            "unconnected_pins",
        ),
    ] {
        let text = call_tool(tool, CONNECTORS, args, false).await;
        let answer: Value = serde_json::from_str(&text).unwrap();
        let items: Vec<_> = answer[key]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| key != "orphans" || item["type"] == "unconnected_pin")
            .collect();
        assert_eq!(items.len(), 1, "{tool}: {answer}");
        assert_eq!(items[0]["reference"], "J1");
        assert_eq!(items[0]["pin"], "6");
    }
}

#[tokio::test]
async fn blank_pin_numbers_do_not_invalidate_the_sheet() {
    let sheet = r#"(kicad_sch (lib_symbols
        (symbol "Connector:Sample" (pin passive line (at 0 0 0) (number "1")))
        (symbol "Mechanical:Sample" (pin passive line (at 0 0 0) (number ""))))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J1"))
        (symbol (lib_id "Mechanical:Sample") (at 5 0 0) (property "Reference" "M1")))"#;
    let answer = check(sheet, json!({})).await;
    assert_eq!(answer["connectors"].as_array().unwrap().len(), 1);
    assert_eq!(answer["connectors"][0]["reference"], "J1");
    let answer = check(sheet, json!({"references": ["M1"]})).await;
    assert_eq!(answer["connectors"][0]["pins"][0]["pin"], "");
}

#[tokio::test]
async fn references_and_pins_sort_naturally_regardless_of_file_order() {
    let sheet = r#"(kicad_sch (lib_symbols
        (symbol "Connector:Sample"
            (pin passive line (at 0 5 0) (number "10"))
            (pin passive line (at 0 0 0) (number "2"))))
        (symbol (lib_id "Connector:Sample") (at 5 0 0) (property "Reference" "J10"))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J2"))
        (wire (pts (xy 0 0) (xy -5 0)))
        (wire (pts (xy 0 -5) (xy -5 -5)))
        (wire (pts (xy 5 0) (xy 10 0))))"#;
    let answer = check(sheet, json!({})).await;
    assert_eq!(answer["connectors"][0]["reference"], "J2");
    assert_eq!(answer["connectors"][1]["reference"], "J10");
    assert_eq!(answer["connectors"][0]["pins"][0]["pin"], "2");
    assert_eq!(answer["connectors"][0]["pins"][1]["pin"], "10");
    assert_eq!(answer["findings"][0]["reference"], "J2");
    assert_eq!(answer["findings"][0]["pin"], "2");
    assert_eq!(answer["findings"][1]["pin"], "10");
    assert_eq!(answer["findings"][2]["reference"], "J10");
}

#[tokio::test]
async fn a_unit_reports_only_its_active_body_style_pins() {
    let sheet = SHEET_PINS.replace(
        "(at 152.4 50.8 0)\n\t\t(unit 1)",
        "(at 152.4 50.8 0)\n\t\t(unit 2)",
    );
    assert_ne!(sheet, SHEET_PINS);
    let answer = check(&sheet, json!({"references":["U2", "U4"]})).await;
    let units: Vec<_> = answer["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| {
            let pins: Vec<_> = symbol["pins"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pin| pin["pin"].as_str().unwrap())
                .collect();
            (
                symbol["reference"].as_str().unwrap(),
                symbol["unit"].as_u64().unwrap(),
                pins,
            )
        })
        .collect();
    assert_eq!(
        units,
        [
            ("U2", 2, vec!["5", "6", "7"]),
            ("U4", 1, vec!["1", "2", "3"]),
        ]
    );
}

fn select_tck(sheet: &str) -> String {
    let selected = sheet.replacen(
        "(pin \"4\"\n\t\t\t(uuid \"431beb79-2289-47fc-9e6e-d4c5dea03cee\")",
        "(pin \"4\"\n\t\t\t(alternate \"TCK\")\n\t\t\t(uuid \"431beb79-2289-47fc-9e6e-d4c5dea03cee\")",
        1,
    );
    assert_ne!(selected, sheet);
    selected
}

#[tokio::test]
async fn selected_alternate_names_are_visible_but_kicad_nets_use_the_base_name() {
    let sheet = select_tck(CONNECTORS);
    let answer = check(&sheet, json!({})).await;
    assert_eq!(context(&answer, 1, 4)["pin_name"], "TCK");
    assert_eq!(context(&answer, 2, 1)["net"], "Net-(J2-SWCLK)");
    assert_eq!(context(&answer, 2, 1)["other_pin_names"][0]["name"], "TCK");
}

const DUPLICATE_SHOWN_NAME_NET: &str = "Net-(J2-SWCLK-Pad4)";

fn duplicate_shown_name_sheet() -> String {
    // J2.2 and J2.4 now both show TCK, though their library names differ.
    let sheet = select_tck(CONNECTORS).replace("(name \"SWDIO\"", "(name \"TCK\"");
    let j3_library = "(symbol \"Connector_Generic:Conn_01x02\"";
    let (before, j3) = sheet.split_once(j3_library).unwrap();
    // Give J3.1 a numeric name so it does not win the net naming tie.
    format!(
        "{before}{j3_library}{}",
        j3.replacen("(name \"Pin_1\"", "(name \"1\"", 1)
    )
}

#[tokio::test]
async fn a_duplicate_shown_alternate_adds_the_pad_to_the_kicad_net_name() {
    let sheet = duplicate_shown_name_sheet();
    let answer = check(&sheet, json!({})).await;
    assert_eq!(context(&answer, 1, 2)["pin_name"], "TCK");
    assert_eq!(context(&answer, 1, 4)["pin_name"], "TCK");
    assert_eq!(context(&answer, 2, 1)["net"], DUPLICATE_SHOWN_NAME_NET);
}

#[test]
#[ignore = "requires KiCad 10.0.8 CLI"]
fn native_kicad_names_the_duplicate_shown_alternate_net() {
    let dir = tempfile::tempdir().unwrap();
    let sch = dir.path().join("alternate.kicad_sch");
    let netlist = dir.path().join("alternate.net");
    std::fs::write(&sch, duplicate_shown_name_sheet()).unwrap();
    let output = std::process::Command::new("kicad-cli")
        .args(["sch", "export", "netlist", "--output"])
        .arg(&netlist)
        .arg(&sch)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = copperpour_sexp::parse_sexp(&std::fs::read_to_string(netlist).unwrap()).unwrap();
    let net = tree
        .find("nets")
        .unwrap()
        .find_all("net")
        .into_iter()
        .find(|net| net.find_str("name") == Some(DUPLICATE_SHOWN_NAME_NET))
        .expect("KiCad names the net after the library name and pad");
    let nodes: std::collections::BTreeSet<_> = net
        .find_all("node")
        .into_iter()
        .map(|node| (node.find_str("ref").unwrap(), node.find_str("pin").unwrap()))
        .collect();
    assert_eq!(nodes, [("J2", "4"), ("J3", "1")].into_iter().collect());
}

#[tokio::test]
async fn a_pins_own_visible_generic_name_is_not_other_pin_context() {
    let mut sheet = CONNECTORS.to_owned();
    let library = sheet
        .find("(symbol \"Connector_Generic:Conn_01x02\"")
        .unwrap();
    let hide = library + sheet[library..].find("(hide yes)").unwrap();
    sheet.replace_range(hide..hide + "(hide yes)".len(), "(hide no)");
    let answer = check(&sheet, json!({})).await;
    let pin = &answer["connectors"][2]["pins"][0];
    assert_eq!(pin["status"], "unidentified");
    assert_eq!(pin["evidence"], json!([]));
    let other_names = pin["context"]["other_pin_names"].as_array().unwrap();
    assert_eq!(other_names.len(), 1);
    assert_eq!(other_names[0]["reference"], "J2");
    assert_eq!(other_names[0]["name"], "SWCLK");
}

#[tokio::test]
async fn an_unsplit_wire_does_not_touch_a_power_pin_without_a_junction() {
    let sheet = r##"(kicad_sch (lib_symbols
        (symbol "Connector:Sample"
            (pin passive line (at 0 0 0) (number "1")))
        (symbol "power:GND" (power global)
            (pin power_in line (at 0 0 0) (number "1"))))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J1"))
        (symbol (lib_id "power:GND") (at 5 0 0)
            (property "Reference" "#PWR01") (property "Value" "GND"))
        (wire (pts (xy 0 0) (xy 10 0))))"##;
    let detached = check(sheet, json!({})).await;
    assert_eq!(
        detached["connectors"][0]["pins"][0]["status"],
        "unidentified"
    );
    let joined = format!("{} (junction (at 5 0)))", sheet.strip_suffix(')').unwrap());
    let joined = check(&joined, json!({})).await;
    assert_eq!(
        joined["connectors"][0]["pins"][0]["evidence"],
        json!(["power_symbol"])
    );
    assert_mark(&context(&joined, 0, 1)["nearest_power_symbol"], "GND", 5.0);
}

#[tokio::test]
async fn an_inactive_power_body_style_cannot_identify_a_signal() {
    let sheet = r##"(kicad_sch (lib_symbols
        (symbol "Connector:Sample" (pin passive line (at 0 0 0) (number "1")))
        (symbol "power:GND" (power global)
            (symbol "GND_1_1" (pin power_in line (at 0 0 0) (number "1")))
            (symbol "GND_1_2" (pin power_in line (at 0 5 0) (number "1")))))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J1"))
        (symbol (lib_id "power:GND") (at 5 0 0) (body_style 2)
            (property "Reference" "#PWR01") (property "Value" "GND"))
        (wire (pts (xy 0 0) (xy 5 0))))"##;
    let detached = check(sheet, json!({})).await;
    assert_eq!(
        detached["connectors"][0]["pins"][0]["status"],
        "unidentified"
    );
    let joined = check(
        &sheet.replace("(body_style 2)", "(body_style 1)"),
        json!({}),
    )
    .await;
    assert_eq!(
        joined["connectors"][0]["pins"][0]["evidence"],
        json!(["power_symbol"])
    );
}

#[tokio::test]
async fn a_power_symbol_on_another_run_is_evidence_without_a_nearby_mark() {
    let sheet = r##"(kicad_sch (lib_symbols
        (symbol "Connector:Sample" (pin passive line (at 0 0 0) (number "1")))
        (symbol "power:Sample" (power local)
            (pin power_in line (at 0 0 0) (number "1"))))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J1"))
        (label "SIG" (at 5 0 0))
        (wire (pts (xy 0 0) (xy 5 0)))
        (symbol (lib_id "power:Sample") (at 0 5 0)
            (property "Reference" "#PWR01") (property "Value" "SIG"))
        (wire (pts (xy 0 5) (xy 5 5))))"##;
    let answer = check(sheet, json!({})).await;
    assert_eq!(
        answer["connectors"][0]["pins"][0]["evidence"],
        json!(["label", "power_symbol"])
    );
    assert_eq!(context(&answer, 0, 1)["nearest_power_symbol"], Value::Null);
}

#[tokio::test]
async fn an_alternate_no_connect_type_takes_precedence_over_a_wire_and_name() {
    let sheet = r#"(kicad_sch (lib_symbols
        (symbol "Connector:Sample"
            (pin passive line (at 0 0 0) (number "1") (name "SIG")
                (alternate "NC" no_connect line))))
        (symbol (lib_id "Connector:Sample") (at 0 0 0) (property "Reference" "J1")
            (pin "1" (alternate "NC")))
        (wire (pts (xy 0 0) (xy 5 0))))"#;
    let answer = check(sheet, json!({})).await;
    let pin = &answer["connectors"][0]["pins"][0];
    assert_eq!(pin["status"], "no_connect");
    assert_eq!(pin["evidence"], json!([]));
    assert_eq!(pin["context"]["no_connect"], "pin_type");
}
