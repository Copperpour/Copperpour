use super::{cleanup_net_graph, net_graph_for, snapshot_net_graph, NetGraph};
use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;
use copperpour_sexp::schematic::{extract_all_net_labels, extract_wires, read_schematic};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

struct Witness {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl Witness {
    fn new(case: &str) -> Self {
        let fixture = Path::new("power_driver_priority_kicad10")
            .join(case)
            .join("ERC_same_name_local_global_power_port.kicad_sch");
        Self::from_fixture(&fixture)
    }

    fn from_fixture(fixture: &Path) -> Self {
        let file = fixture.file_name().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file);
        fs::copy(&fixture, &path).unwrap();
        let child = fixture.with_file_name("B.kicad_sch");
        if child.is_file() {
            fs::copy(child, dir.path().join("B.kicad_sch")).unwrap();
        }
        Self { dir, path }
    }

    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<_> = fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name().into(), fs::read(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }

    async fn call(&self, handler: &McpHandler, tool: &str, mut args: Value) -> (bool, Value) {
        args["schematic"] = json!(self.path);
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "method": "tools/call", "id": 823,
                "params": { "name": tool, "arguments": args }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let text = reply["content"][0]["text"].as_str().unwrap();
        (
            reply["isError"] == true,
            serde_json::from_str(text).unwrap(),
        )
    }

    async fn ok(&self, handler: &McpHandler, tool: &str, args: Value) -> Value {
        let (error, body) = self.call(handler, tool, args).await;
        assert!(!error, "{tool}: {body}");
        body
    }
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

fn scoped(name: &str, scope: u8) -> (String, u8) {
    (name.into(), scope)
}

fn rows(value: &Value) -> &[Value] {
    value.as_array().unwrap()
}

fn string_column<'a>(value: &'a Value, key: &str) -> Vec<&'a str> {
    let mut column: Vec<_> = rows(value)
        .iter()
        .map(|row| row[key].as_str().unwrap())
        .collect();
    column.sort_unstable();
    column
}

#[tokio::test]
async fn both_power_scopes_and_the_native_winner_reach_each_graph() {
    let handler = handler().await;
    for (case, alias, winner, scope) in [
        ("before", "GND", "VCC", 0),
        ("after", "ZZZ_LOCAL", "VCC", 0),
        ("same_name", "VCC", "VCC", 0),
        ("local_label", "VCC", "VCC", 0),
        ("global_label", "VCC", "ZZZ_LABEL", 0),
        ("local_power_label", "GND", "GND", 1),
    ] {
        let sheet = Witness::new(case);
        let files = sheet.files();
        for (reference, name) in [("R1", winner), ("R3", winner), ("R2", "ZZZ_LOCAL_ONLY")] {
            let body = sheet
                .ok(
                    &handler,
                    "get_pin_net_name",
                    json!({"reference": reference, "pin_number": "1"}),
                )
                .await;
            assert_eq!(body["net"], name, "{case} {reference}: {body}");
        }
        assert_eq!(sheet.files(), files);

        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let wires = extract_wires(&tree);
        let labels = extract_all_net_labels(&tree);
        for mut graph in [
            net_graph_for(&tree, &wires, &labels),
            snapshot_net_graph(&tree, &wires),
            cleanup_net_graph(&tree, &wires),
        ] {
            let root = graph.root_at(134.62, 88.9);
            assert_eq!(graph.root_at(121.92, 88.9), root, "{case}");
            assert_ne!(graph.root_at(160.02, 88.9), root, "{case}");
            let chosen = scoped(winner, scope);
            assert_eq!(
                graph.scoped_name_of_root(root),
                Some(chosen.clone()),
                "{case}"
            );
            let mut expected = BTreeSet::from([scoped(alias, 1)]);
            if case != "local_power_label" {
                expected.insert(scoped("VCC", 0));
            }
            if matches!(case, "local_label" | "global_label" | "local_power_label") {
                expected.insert(scoped("AAA_LABEL", 1));
            }
            if case == "global_label" {
                expected.insert(scoped("ZZZ_LABEL", 0));
            }
            assert_eq!(scoped_aliases(&mut graph, &chosen), expected, "{case}");
        }
    }
}

fn scoped_aliases(graph: &mut NetGraph, winner: &(String, u8)) -> BTreeSet<(String, u8)> {
    graph
        .named_net_aliases()
        .into_iter()
        .filter_map(|(name, alias)| (&name == winner).then_some(alias))
        .collect()
}

#[tokio::test]
async fn a_losing_local_power_alias_still_joins_separate_drawn_islands() {
    let handler = handler().await;
    let sheet = Witness::new("local_alias_island");
    let original = sheet.files();
    let result = sheet
        .ok(&handler, "get_net_connections", json!({"net": "VCC"}))
        .await;
    assert_eq!(
        string_column(&result["pins"], "reference"),
        ["R1", "R3", "R4"]
    );
    assert_eq!(string_column(&result["pins"], "pin"), ["1", "1", "1"]);
    assert_eq!(
        string_column(&result["labels"], "net"),
        ["GND", "GND", "VCC"]
    );
    let (_, tree) = read_schematic(&sheet.path).unwrap();
    let wires = extract_wires(&tree);
    for mut graph in [
        net_graph_for(&tree, &wires, &extract_all_net_labels(&tree)),
        snapshot_net_graph(&tree, &wires),
        cleanup_net_graph(&tree, &wires),
    ] {
        let rail = graph.root_at(134.62, 88.9);
        assert_eq!(graph.root_at(171.45, 88.9), rail);
        assert_eq!(graph.name_of_root(rail).as_deref(), Some("VCC"));
    }
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn layout_comparison_reports_the_local_alias_change_without_a_primary_rename() {
    let handler = handler().await;
    let baseline = Witness::new("before");
    let candidate = Witness::new("after");
    let same = Witness::new("same_name");
    let original = [baseline.files(), candidate.files(), same.files()];
    let compare = "compare_schematic_layout";
    let result = candidate
        .ok(&handler, compare, json!({"baseline": baseline.path}))
        .await;
    assert_eq!(result["verdict"], "blocked", "{result}");
    assert_eq!(
        result["reasons"],
        json!([
            "symbol 1fc1cfd6-80d5-4eaf-9101-2601aa18535d (#PWR01) changed field \"Value\"",
            "net GND no longer has its baseline name and scope on the sheet",
            "net ZZZ_LOCAL has a new name or scope on the sheet"
        ])
    );
    let identical = same
        .ok(&handler, compare, json!({"baseline": same.path}))
        .await;
    assert_eq!(identical["verdict"], "unchanged", "{identical}");
    assert_eq!(identical["reasons"], json!([]));
    assert_eq!(
        [baseline.files(), candidate.files(), same.files()],
        original
    );
}

#[tokio::test]
async fn a_custom_global_power_value_outranks_a_local_power_name() {
    let handler = handler().await;
    let baseline = Witness::new("before");
    let candidate = Witness::new("before");
    let custom = "ZZZ_ISO";
    for sheet in [&baseline, &candidate] {
        sheet
            .ok(
                &handler,
                "edit_schematic_component",
                json!({"reference": "#PWR03", "value": custom}),
            )
            .await;
    }
    candidate
        .ok(
            &handler,
            "edit_schematic_component",
            json!({"reference": "#PWR01", "value": "AAA_LOCAL"}),
        )
        .await;
    let original = [baseline.files(), candidate.files()];
    for (sheet, local) in [(&baseline, "GND"), (&candidate, "AAA_LOCAL")] {
        let pin = sheet
            .ok(
                &handler,
                "get_pin_net_name",
                json!({"reference": "R1", "pin_number": "1"}),
            )
            .await;
        assert_eq!(pin["net"], custom, "{pin}");
        let aliases = sheet.ok(&handler, "list_schematic_nets", json!({})).await;
        let mut expected = ["AAA_PORT", local, "ROUTE_DECOY", custom, "ZZZ_LOCAL_ONLY"];
        expected.sort_unstable();
        assert_eq!(aliases["nets"], json!(expected));
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let mut graph = snapshot_net_graph(&tree, &extract_wires(&tree));
        let root = graph.root_at(134.62, 88.9);
        assert_eq!(graph.scoped_name_of_root(root), Some(scoped(custom, 0)));
    }
    let result = candidate
        .ok(
            &handler,
            "compare_schematic_layout",
            json!({"baseline": baseline.path}),
        )
        .await;
    assert_eq!(result["verdict"], "blocked", "{result}");
    assert_eq!(
        result["reasons"],
        json!([
            "symbol 1fc1cfd6-80d5-4eaf-9101-2601aa18535d (#PWR01) changed field \"Value\"",
            "net GND no longer has its baseline name and scope on the sheet",
            "net AAA_LOCAL has a new name or scope on the sheet"
        ])
    );
    assert_eq!([baseline.files(), candidate.files()], original);
}

#[tokio::test]
async fn readers_report_the_primary_name_without_losing_aliases_or_pin_membership() {
    let handler = handler().await;
    let sheet = Witness::new("before");
    let original = sheet.files();
    let pin = sheet
        .ok(
            &handler,
            "get_pin_connections",
            json!({"reference": "R1", "pin_number": "1"}),
        )
        .await;
    assert_eq!(pin["net"], "VCC");
    assert_eq!(pin["reference"], "R1");
    assert_eq!(pin["pin"], "1");
    let component = sheet
        .ok(&handler, "get_component_nets", json!({"reference": "R1"}))
        .await;
    let pin_nets: Vec<_> = rows(&component["pins"])
        .iter()
        .map(|pin| (&pin["pin"], &pin["net"]))
        .collect();
    assert_eq!(
        pin_nets,
        vec![(&json!("1"), &json!("VCC")), (&json!("2"), &Value::Null)]
    );
    let trace = sheet
        .ok(
            &handler,
            "trace_from_point",
            json!({"x": 134.62, "y": 88.9}),
        )
        .await;
    assert_eq!(trace["net"], "VCC");
    assert_eq!(string_column(&trace["pins_here"], "reference"), ["R1"]);
    let items = sheet
        .ok(&handler, "get_connected_items", json!({"reference": "R1"}))
        .await;
    assert_eq!(items["nets"], json!(["VCC"]));
    assert_eq!(string_column(&items["labels"], "net"), ["GND", "VCC"]);
    assert_eq!(
        string_column(&items["connected_components"], "reference"),
        ["#PWR01", "#PWR03", "R3"]
    );
    let summary = sheet
        .ok(&handler, "export_netlist_summary", json!({}))
        .await;
    assert_eq!(
        sheet.ok(&handler, "list_schematic_nets", json!({})).await["nets"],
        summary["nets"]
    );
    assert_eq!(
        summary["nets"],
        json!(["AAA_PORT", "GND", "ROUTE_DECOY", "VCC", "ZZZ_LOCAL_ONLY"])
    );
    let mut ordinary_members = Vec::new();
    for entry in rows(&summary["components"]) {
        let reference = entry["reference"].as_str().unwrap();
        if reference.starts_with('R') {
            for pin in rows(&entry["pins"]) {
                if pin["net"] == "VCC" {
                    ordinary_members.push((reference, pin["number"].as_str().unwrap()));
                }
            }
        }
    }
    ordinary_members.sort_unstable();
    assert_eq!(ordinary_members, [("R1", "1"), ("R3", "1")]);
    let mut counts = Vec::new();
    for alias in ["GND", "VCC"] {
        let pins = sheet
            .ok(&handler, "get_net_connections", json!({"net": alias}))
            .await;
        assert_eq!(pins["net"], alias);
        assert_eq!(string_column(&pins["labels"], "net"), ["GND", "VCC"]);
        assert_eq!(string_column(&pins["pins"], "reference"), ["R1", "R3"]);
        assert_eq!(string_column(&pins["pins"], "pin"), ["1", "1"]);
        let connectivity = sheet
            .ok(&handler, "get_net_connectivity", json!({"net": alias}))
            .await;
        assert_eq!(connectivity["net"], alias);
        counts.push(rows(&connectivity["wires"]).len());
    }
    assert_eq!(counts, [5, 5]);
    let shorts = sheet.ok(&handler, "find_shorted_nets", json!({})).await;
    let mut actual: Vec<_> = rows(&shorts["shorts"])
        .iter()
        .map(|row| row["shorted_nets"].clone())
        .collect();
    actual.sort_by_key(Value::to_string);
    assert_eq!(
        actual,
        [json!(["AAA_PORT", "ZZZ_LOCAL_ONLY"]), json!(["GND", "VCC"])]
    );
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn audits_use_the_primary_name_and_still_classify_the_ground_alias() {
    let handler = handler().await;
    let sheet = Witness::new("audits");
    let original = sheet.files();
    let decoupling = "Power pin 'VDD' on U1 has no decoupling capacitor (net: VCC)";
    let connections = "Reset pin 'RESET' on U1 (net: VCC) has no pull-up resistor";
    let power = "Power rail 'VCC' has no decoupling capacitors";
    let test_point = "Power rail 'VCC' has no test point";
    for (tool, expected) in [
        ("audit_decoupling", vec![decoupling]),
        ("audit_connections", vec![connections]),
        ("audit_power_rails", vec![power, test_point]),
    ] {
        let body = sheet.ok(&handler, tool, json!({})).await;
        assert_eq!(
            string_column(&body["findings"], "issue"),
            expected,
            "{tool}: {body}"
        );
        if tool == "audit_power_rails" {
            assert_eq!(body["power_nets"], json!(["VCC"]));
            assert_eq!(string_column(&body["findings"], "rail"), ["VCC", "VCC"]);
            assert!(rows(&body["findings"])
                .iter()
                .all(|f| f["global_rail"] == true));
        }
    }
    let review = sheet
        .ok(
            &handler,
            "run_design_review",
            json!({"severity_filter": "info"}),
        )
        .await;
    let mut issues: Vec<_> = rows(&review["design_review"]["findings"])
        .iter()
        .filter(|finding| {
            matches!(
                finding["category"].as_str(),
                Some("decoupling" | "connection" | "power")
            )
        })
        .map(|finding| finding["issue"].as_str().unwrap())
        .collect();
    issues.sort_unstable();
    assert_eq!(issues, [decoupling, power, test_point, connections]);
    let ground = Witness::new("before");
    let files = ground.files();
    let rails = ground.ok(&handler, "audit_power_rails", json!({})).await;
    assert_eq!(rails["power_nets"], json!(["VCC"]));
    assert_eq!(rails["findings"], json!([]), "{rails}");
    assert_eq!(ground.files(), files);
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn wire_run_findings_name_the_same_mixed_rail_as_pin_queries() {
    let handler = handler().await;
    let sheet = Witness::new("before");
    let original = sheet.files();
    let body = sheet
        .ok(
            &handler,
            "check_schematic_wire_runs",
            json!({"long_run_mm": 10}),
        )
        .await;
    assert_eq!(
        string_column(&body["findings"], "net"),
        ["VCC", "VCC"],
        "{body}"
    );
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn local_child_capacitor_does_not_decouple_the_global_root_rail() {
    let handler = handler().await;
    let sheet = Witness::new("hierarchy");
    let original = sheet.files();
    // KiCad exports root U1.1/U1.2 on VCC and child C1.1 on /B/VCC.
    let hierarchy = sheet
        .ok(
            &handler,
            "audit_power_rails",
            json!({"schematic_scope": "hierarchy"}),
        )
        .await;
    assert_eq!(hierarchy["status"], "complete");
    assert_eq!(hierarchy["coverage"]["audited_sheet_instances"], 2);
    let review = sheet
        .ok(
            &handler,
            "run_design_review",
            json!({"severity_filter": "info"}),
        )
        .await;
    let review = &review["design_review"];
    assert_eq!(review["audits"]["power_rails"]["completed"], 2);
    for findings in [&hierarchy["findings"], &review["findings"]] {
        let mut power: Vec<_> = rows(findings)
            .iter()
            .filter(|row| row["rail"].is_string())
            .map(|row| {
                (
                    row["issue"].as_str().unwrap(),
                    row["severity"].as_str().unwrap(),
                    row["global_rail"].as_bool().unwrap(),
                    Path::new(row["source"].as_str().unwrap())
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap(),
                )
            })
            .collect();
        power.sort_unstable();
        let root = sheet.path.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            power,
            [
                (
                    "Power rail 'VCC' has no decoupling capacitors",
                    "error",
                    true,
                    root
                ),
                (
                    "Power rail 'VCC' has no test point",
                    "info",
                    false,
                    "B.kicad_sch"
                ),
                ("Power rail 'VCC' has no test point", "info", true, root),
            ],
            "{findings}"
        );
    }
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn a_midwire_power_pin_needs_a_junction_to_name_the_wire() {
    let handler = handler().await;
    // KiCad leaves R1.1/R3.1 on /GND without the dot and exports VCC with it.
    for (case, net) in [("midwire", "GND"), ("midwire_junction", "VCC")] {
        let sheet = Witness::new(case);
        let original = sheet.files();
        for reference in ["R1", "R3"] {
            let pin = sheet
                .ok(
                    &handler,
                    "get_pin_net_name",
                    json!({"reference": reference, "pin_number": "1"}),
                )
                .await;
            assert_eq!(pin["net"], net, "{case} {reference}: {pin}");
        }
        let runs = sheet
            .ok(
                &handler,
                "check_schematic_wire_runs",
                json!({"long_run_mm": 7}),
            )
            .await;
        let expected = if case == "midwire" {
            vec![net, net]
        } else {
            vec![net]
        };
        assert_eq!(string_column(&runs["findings"], "net"), expected, "{runs}");
        assert_eq!(sheet.files(), original);
    }
}

#[tokio::test]
async fn a_selected_passive_alternate_does_not_name_the_power_net() {
    let handler = handler().await;
    let sheet = Witness::new("passive_alternate");
    let original = sheet.files();
    // KiCad exports the joined R1.1/R3.1 rail as /GND: VCC's placed pin is passive.
    let pin = sheet
        .ok(
            &handler,
            "get_pin_net_name",
            json!({"reference": "R1", "pin_number": "1"}),
        )
        .await;
    assert_eq!(pin["net"], "GND", "{pin}");
    let runs = sheet
        .ok(
            &handler,
            "check_schematic_wire_runs",
            json!({"long_run_mm": 7}),
        )
        .await;
    assert_eq!(
        string_column(&runs["findings"], "net"),
        ["GND"; 3],
        "{runs}"
    );

    let (_, tree) = read_schematic(&sheet.path).unwrap();
    let wires = extract_wires(&tree);
    for mut graph in [
        snapshot_net_graph(&tree, &wires),
        cleanup_net_graph(&tree, &wires),
    ] {
        let root = graph.root_at(134.62, 88.9);
        assert_eq!(graph.scoped_name_of_root(root), Some(scoped("GND", 1)));
        assert_eq!(graph.aliases_of_root(root), BTreeSet::from(["GND".into()]));
    }
    assert_eq!(sheet.files(), original);
}

#[tokio::test]
async fn pin_derived_names_use_the_selected_alternate_type() {
    let sheet = Witness::from_fixture(Path::new("pin_fallback_alternate.kicad_sch"));
    let original = sheet.files();
    let pins = sheet
        .ok(
            &handler().await,
            "get_component_nets",
            json!({"reference": "U2"}),
        )
        .await;
    let mut pin_nets: Vec<_> = rows(&pins["pins"])
        .iter()
        .map(|pin| (pin["pin"].as_str().unwrap(), pin["net"].as_str()))
        .collect();
    pin_nets.sort_unstable();
    // KiCad names this unlabeled wire after AUX, not the hidden base power pin.
    assert_eq!(
        pin_nets,
        [("2", Some("Net-(U2-AUX)")), ("6", Some("Net-(U2-AUX)"))]
    );
    assert_eq!(sheet.files(), original);
}

#[test]
fn an_inactive_power_pin_and_a_hidden_passive_alternate_cannot_supply_a_name() {
    use super::{PinNetNames, UnitNotation};
    use copperpour_sexp::schematic::extract_placed_pins;
    use copperpour_sexp::{parse_sexp, SexpNode};

    // Small parser-level witness: no full schematic fixture or file mutation.
    let lib = parse_sexp(
        r#"(symbol "VCC"
            (power)
            (symbol "VCC_1_1"
                (pin power_in line (at 0 0 0) (hide yes) (name "VCC") (number "1")
                    (alternate "PASSIVE_WITNESS" passive line)))
            (symbol "VCC_1_2"
                (pin power_in line (at 0 0 0) (hide yes) (name "VCC") (number "1"))))"#,
    )
    .unwrap();
    let placed = parse_sexp(
        r#"(symbol (lib_id "VCC") (at 0 0 0) (body_style 1)
            (pin "1" (alternate "PASSIVE_WITNESS")))"#,
    )
    .unwrap();
    let pins = extract_placed_pins(&placed, &lib, 1);
    assert_eq!(pins.len(), 1);
    assert!(pins[0].hidden);
    assert_eq!(pins[0].electrical_type, "passive");

    let standard = parse_sexp(r#"(symbol (lib_id "VCC") (at 0 0 0) (body_style 1))"#).unwrap();
    for (instance, expected) in [(placed, None), (standard, Some("VCC"))] {
        let tree = SexpNode::List(vec![
            SexpNode::Atom("kicad_sch".into()),
            SexpNode::List(vec![SexpNode::Atom("lib_symbols".into()), lib.clone()]),
            instance,
        ]);
        // No explicit labels: exercise the pin-derived fallback independently.
        let mut graph = net_graph_for(&tree, &[], &[]);
        let driver_names = PinNetNames::build(&tree, &mut graph, UnitNotation::default());
        assert_eq!(
            driver_names
                .pin_name_of_root(graph.root_at(0.0, 0.0))
                .as_deref(),
            expected
        );
    }
}

#[tokio::test]
async fn route_conflicts_report_the_primary_name_without_relaxing_the_guard() {
    let handler = handler().await;
    let pair = json!({"ref1": "R1", "pin1": "2", "ref2": "R2", "pin2": "1"});
    let horizontal = "the horizontal-first route would join R2 pin 2 at (160.02, 96.52)";
    let vertical = concat!(
        "the vertical-first route would join ",
        "#PWR01 pin 1 and power symbol GND at (134.62, 81.28) on net VCC, ",
        "#PWR03 pin 1 and power symbol VCC at (143.51, 83.82) on net VCC, ",
        "R1 pin 1 at (134.62, 88.9) on net VCC, R3 pin 1 at (121.92, 88.9) on net VCC"
    );
    for (tool, args) in [
        ("connect_pins", pair.clone()),
        ("batch_connect_pins", json!({"connections": [pair]})),
        (
            "add_schematic_connection",
            json!({
                "x1": 134.62, "y1": 96.52, "x2": 160.02, "y2": 88.9
            }),
        ),
    ] {
        let sheet = Witness::new("before");
        let original = sheet.files();
        let (error, body) = sheet.call(&handler, tool, args).await;
        assert!(error, "{tool}: {body}");
        if tool == "batch_connect_pins" {
            assert_eq!(body["connected_count"], 0);
            assert_eq!(body["already_connected"], json!([]));
            assert_eq!(
                body["errors"],
                json!([format!("R1.2 -> R2.1: {horizontal}; {vertical}")])
            );
        } else {
            assert_eq!(body["error"]["kind"], "plan_blocked", "{tool}: {body}");
            assert_eq!(body["error"]["reasons"], json!([horizontal, vertical]));
        }
        assert_eq!(sheet.files(), original);
    }

    let clear = json!({"ref1": "R1", "pin1": "2", "ref2": "R2", "pin2": "2"});
    for (tool, args) in [
        ("connect_pins", clear.clone()),
        ("batch_connect_pins", json!({"connections": [clear]})),
        (
            "add_schematic_connection",
            json!({
                "x1": 134.62, "y1": 96.52, "x2": 160.02, "y2": 96.52
            }),
        ),
    ] {
        let sheet = Witness::new("before");
        let (_, old) = read_schematic(&sheet.path).unwrap();
        sheet.ok(&handler, tool, args).await;
        let (_, new) = read_schematic(&sheet.path).unwrap();
        let wires = extract_wires(&new);
        assert_eq!(wires.len(), extract_wires(&old).len() + 1);
        let labels = extract_all_net_labels(&new);
        let mut graph = net_graph_for(&new, &wires, &labels);
        let routed = graph.root_at(134.62, 96.52);
        assert_eq!(graph.root_at(160.02, 96.52), routed);
        assert_ne!(graph.root_at(134.62, 88.9), routed);
        assert_ne!(graph.root_at(121.92, 96.52), routed);
    }
}

#[tokio::test]
async fn moving_the_mixed_rail_keeps_the_connected_pin_set() {
    let handler = handler().await;
    let sheet = Witness::new("before");
    let body = sheet
        .ok(
            &handler,
            "move_region",
            json!({
                "x1": 95, "y1": 65, "x2": 180, "y2": 105, "dx": 5.08, "dy": 0, "include": "all"
            }),
        )
        .await;
    assert_eq!(body["moved_count"], 5, "{body}");
    let result = sheet
        .ok(&handler, "get_net_connections", json!({"net": "GND"}))
        .await;
    assert_eq!(string_column(&result["pins"], "reference"), ["R1", "R3"]);
    assert_eq!(string_column(&result["labels"], "net"), ["GND", "VCC"]);
}
