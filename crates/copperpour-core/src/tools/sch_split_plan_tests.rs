//! Served coverage for the LED driver, pull-ups and decoupling capacitor from
//! the KiCad-saved sheet-split fixture.

use super::ServerConfig;
use crate::mcp::handler::McpHandler;
use copperpour_sexp::schematic::{extract_wires, read_schematic};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const LED_BLOCK: [&str; 4] = ["R1", "R2", "D1", "Q1"];
const WITH_C1: [&str; 5] = ["R1", "R2", "D1", "Q1", "C1"];

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sheet_split_kicad10");
    let dir = tempfile::tempdir().unwrap();
    for file in ["split.kicad_sch", "split.kicad_pro"] {
        std::fs::copy(src.join("split").join(file), dir.path().join(file)).unwrap();
    }
    std::fs::copy(
        src.join("split_populated.kicad_pcb"),
        dir.path().join("split.kicad_pcb"),
    )
    .unwrap();
    let path = dir.path().join("split.kicad_sch");
    (dir, path)
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig::default()).await.unwrap()
}

async fn call(handler: &McpHandler, arguments: Value) -> (bool, Value) {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 781, "method": "tools/call",
            "params": {
                "name": "call_tool",
                "arguments": {"name": "plan_schematic_split", "arguments": arguments}
            }
        }))
        .await
        .expect("tools/call receives a response");
    let result = response.result.expect("successful JSON-RPC response");
    let text = result["content"][0]["text"].as_str().unwrap();
    let body = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
    (result["isError"] == json!(true), body)
}

fn symbols(path: &Path, references: &[&str]) -> Vec<Value> {
    let tree = copperpour_sexp::parse_sexp(&std::fs::read_to_string(path).unwrap()).unwrap();
    references
        .iter()
        .map(|reference| {
            let node = tree
                .find_all("symbol")
                .into_iter()
                .find(|node| {
                    node.find_all("property").iter().any(|p| {
                        p.get(1).and_then(|v| v.as_str()) == Some("Reference")
                            && p.get(2).and_then(|v| v.as_str()) == Some(*reference)
                    })
                })
                .unwrap_or_else(|| panic!("no {reference} in {}", path.display()));
            json!({
                "uuid": node.find_str("uuid").unwrap(),
                "unit": node.find_f64("unit").unwrap() as u32
            })
        })
        .collect()
}

async fn plan(handler: &McpHandler, path: &Path, references: &[&str]) -> Value {
    let (is_error, body) = call(
        handler,
        json!({"schematic": path, "symbols": symbols(path, references)}),
    )
    .await;
    assert!(!is_error, "{body:#}");
    body
}

fn pins(net: &Value, key: &str) -> Vec<String> {
    net[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|pin| {
            assert!(pin["symbol_uuid"].as_str().is_some());
            assert_eq!(pin["unit"], 1);
            format!(
                "{}.{}",
                pin["reference"].as_str().unwrap(),
                pin["number"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn split_plan_recommends_the_led_driver_and_names_its_interface() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let plan = plan(&handler, &path, &LED_BLOCK).await;
    assert_eq!(plan["recommendation"], "split", "{plan:#}");
    assert_eq!(
        plan["counts"],
        json!({"parts": 4, "internal_nets": 3, "signal_ports": 1, "rails": 2, "global_nets": 0})
    );
    let boundary = plan["boundary_nets"].as_array().unwrap();
    assert_eq!(boundary.len(), 3);
    let by_name = |name: &str| boundary.iter().find(|n| n["name"] == name).unwrap();
    let led_en = by_name("LED_EN");
    assert_eq!(led_en["kind"], "signal");
    assert_eq!(led_en["joined_by"], "wire");
    assert_eq!(
        led_en["aliases"],
        json!([{"name": "LED_EN", "scope": "local"}])
    );
    assert_eq!(pins(led_en, "pins_inside"), ["R1.1"]);
    assert_eq!(pins(led_en, "pins_outside"), ["U1.15"]);
    assert_eq!(
        led_en["sheet_pin"],
        json!({"name": "LED_EN", "name_source": "label", "shape": "input", "requires_confirmation": true})
    );
    let supply = by_name("+5V");
    assert_eq!(supply["kind"], "rail");
    assert_eq!(
        supply["aliases"],
        json!([{"name": "+5V", "scope": "global_power"}])
    );
    assert_eq!(supply["joined_by"], "both");
    assert_eq!(pins(supply, "pins_inside"), ["R2.1"]);
    assert_eq!(
        pins(supply, "pins_outside"),
        ["C1.1", "J1.1", "R3.1", "R4.1", "R5.1", "R6.1", "U1.16"]
    );
    assert_eq!(
        supply["scope_decision"],
        json!(["keep_existing_scope", "stop"])
    );
    let ground = by_name("GND");
    assert_eq!(ground["joined_by"], "name");
    assert_eq!(pins(ground, "pins_inside"), ["Q1.3"]);
    assert_eq!(
        pins(ground, "pins_outside"),
        ["C1.2", "J1.2", "U1.13", "U1.8"]
    );
    let mut internal: Vec<_> = plan["internal_nets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|net| pins(net, "pins"))
        .collect();
    internal.sort_unstable();
    assert_eq!(
        internal,
        [["D1.1", "Q1.1"], ["D1.2", "R2.2"], ["Q1.2", "R1.2"]]
    );
    let selected = &plan["block_objects_for_review"]["symbols"];
    let uuid_set = |ids: Vec<Value>| -> BTreeSet<String> {
        ids.iter().map(|i| i.as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(
        uuid_set(selected.as_array().unwrap().clone()),
        uuid_set(
            symbols(&path, &LED_BLOCK)
                .into_iter()
                .map(|s| s["uuid"].clone())
                .collect()
        ),
    );
    assert_eq!(plan["stranded_parts"], json!(["C1"]), "{plan:#}");
    assert_eq!(plan["requires_membership_review"], true);
    let support = plan["supporting_parts"].as_array().unwrap();
    assert_eq!(support.len(), 1);
    assert_eq!(
        support[0]["symbol_uuid"],
        symbols(&path, &["C1"])[0]["uuid"]
    );
    assert_eq!(support[0]["shared_drawn_wires"][0]["pin"]["number"], "1");
    assert_eq!(
        support[0]["shared_drawn_wires"][0]["block_pins"],
        json!(["R2.1"])
    );
    assert_eq!(support[0]["other_rail_consumers"], json!(["U1.16", "U1.8"]));
    assert_eq!(
        plan["boundary_power_symbols"],
        json!([symbols(&path, &["#PWR08"])[0]["uuid"]])
    );
    assert_eq!(
        plan["block_objects_for_review"]["power_symbols"],
        json!([symbols(&path, &["#PWR010"])[0]["uuid"]])
    );
    assert_eq!(
        support[0]["options"],
        json!([
            "include_with_block",
            "keep_and_place_near_the_part_it_serves"
        ])
    );
}

#[tokio::test]
async fn split_plan_includes_c1_only_after_explicit_selection() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let expanded = plan(&handler, &path, &WITH_C1).await;
    assert_eq!(expanded["stranded_parts"], json!([]));
    assert_eq!(expanded["supporting_parts"], json!([]));
    assert_eq!(expanded["boundary_power_symbols"], json!([]));
    assert_eq!(
        expanded["counts"],
        json!({
            "parts": 5, "internal_nets": 3, "signal_ports": 1, "rails": 2, "global_nets": 0
        })
    );
    let (_, tree) = read_schematic(&path).unwrap();
    let wires = extract_wires(&tree);
    let reported: Vec<_> = expanded["boundary_wires"]
        .as_array()
        .unwrap()
        .iter()
        .map(|uuid| {
            let wire = wires
                .iter()
                .find(|w| w.uuid.as_deref() == uuid.as_str())
                .unwrap();
            let mut ends = [[wire.x1, wire.y1], [wire.x2, wire.y2]];
            ends.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ends
        })
        .collect();
    // The planner lists the whole wired boundary for review. The example cut
    // keeps the lead up to (152.4, 81.28) on the parent.
    assert_eq!(reported.len(), 3, "{reported:?}");
    assert!(reported.contains(&[[130.81, 81.28], [152.4, 81.28]]));
    assert!(reported.contains(&[[152.4, 81.28], [152.4, 83.82]]));
    assert!(reported.contains(&[[152.4, 83.82], [162.56, 83.82]]));
    let boundary_labels = expanded["boundary_labels"].as_array().unwrap();
    let label = tree
        .find_all("label")
        .into_iter()
        .find(|l| l.get(1).and_then(|n| n.as_str()) == Some("LED_EN"))
        .unwrap();
    assert_eq!(boundary_labels, &[json!(label.find_str("uuid").unwrap())]);
}

#[tokio::test]
async fn split_plan_keeps_pull_ups_and_does_not_infer_a_driver() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let plan = plan(&handler, &path, &["R3", "R4", "R5", "R6"]).await;
    assert_eq!(plan["recommendation"], "keep", "{plan:#}");
    assert_eq!(plan["counts"]["internal_nets"], 0);
    assert_eq!(plan["counts"]["signal_ports"], 4);
    assert_eq!(plan["counts"]["rails"], 1);
    let mut ports = BTreeMap::new();
    for net in plan["boundary_nets"].as_array().unwrap() {
        if net["kind"] == "signal" {
            let port = &net["sheet_pin"];
            assert_eq!(port["shape"], "passive");
            assert_eq!(port["name_source"], "pin_name");
            ports.insert(
                port["name"].as_str().unwrap(),
                (pins(net, "pins_inside"), pins(net, "pins_outside")),
            );
        }
    }
    assert_eq!(ports.len(), 4);
    for (name, inside, outside) in [
        ("SER", "R3.2", ["J1.3", "U1.14"]),
        ("SRCLK", "R4.2", ["J1.4", "U1.11"]),
        ("~{SRCLR}", "R5.2", ["J1.5", "U1.10"]),
        ("RCLK", "R6.2", ["J1.6", "U1.12"]),
    ] {
        assert_eq!(ports[name].0, [inside]);
        assert_eq!(ports[name].1, outside);
    }
}

#[tokio::test]
async fn split_plan_declines_a_lone_decoupling_cap() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let plan = plan(&handler, &path, &["C1"]).await;
    assert_eq!(plan["recommendation"], "keep", "{plan:#}");
    assert_eq!(plan["counts"]["signal_ports"], 0);
    assert_eq!(plan["counts"]["rails"], 2);
    assert_eq!(plan["internal_nets"], json!([]));
}

#[tokio::test]
async fn split_plan_is_read_only_and_deterministic() {
    let (dir, path) = fixture();
    let selected = symbols(&path, &LED_BLOCK);
    let before = snapshot(dir.path());
    let first = handler().await;
    let request = json!({"schematic": path, "symbols": selected});
    let (is_error, first) = call(&first, request.clone()).await;
    assert!(!is_error, "{first:#}");
    let (_, second) = call(&handler().await, request).await;
    assert_eq!(first, second);
    assert_eq!(first["source"], "saved_file");
    let tree = copperpour_sexp::parse_sexp(std::str::from_utf8(&before[&path]).unwrap()).unwrap();
    assert_eq!(
        first["sheet_instance_path"],
        format!("/{}", tree.find_str("uuid").unwrap())
    );
    assert_eq!(first["revision"], crate::hex::sha256_hex(&before[&path]));
    assert_eq!(snapshot(dir.path()), before);
}

#[cfg(unix)]
#[tokio::test]
async fn split_plan_does_not_create_a_document_lock() {
    use std::os::unix::ffi::OsStrExt;

    let (_dir, path) = fixture();
    let state = ["COPPERPOUR_STATE_DIR", "KONNECT_STATE_DIR"]
        .into_iter()
        .find_map(|key| {
            std::env::var_os(key)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| crate::app_dirs::local_data_dir().map(|dir| dir.join("konnect")))
        .unwrap();
    // The writer's lock keys use the canonical directory and native basename.
    let parent = path.parent().unwrap().canonicalize().unwrap();
    let mut key = b"konnect-document-lock-v1\0".to_vec();
    for part in [parent.as_os_str(), path.file_name().unwrap()] {
        let bytes = part.as_bytes();
        key.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        key.extend_from_slice(bytes);
    }
    let lock = state
        .join("locks")
        .join(format!("{}.lock", crate::hex::sha256_hex(&key)));
    assert!(!lock.exists());
    plan(&handler().await, &path, &LED_BLOCK).await;
    assert!(!lock.exists());
}

#[tokio::test]
async fn split_plan_refuses_missing_or_stale_selector_and_power() {
    let (dir, path) = fixture();
    let mut selected = symbols(&path, &["R1", "R2"]);
    let power = symbols(&path, &["#PWR01"]);
    let before = snapshot(dir.path());
    let handler = handler().await;
    let (_, good) = call(&handler, json!({"schematic":path, "symbols":selected})).await;
    assert_eq!(good["counts"]["parts"], 2, "{good:#}");
    let uuid = selected[0]["uuid"].clone();
    selected[0]["uuid"] = json!("cc48d8b5-6ef7-4afe-a645-09527fe834b9");
    let (error, body) = call(&handler, json!({"schematic":path, "symbols":selected})).await;
    assert!(error, "{body:#}");
    assert_eq!(body["error"]["kind"], "stale_target");
    assert!(body["error"]["reason"]
        .as_str()
        .unwrap()
        .contains("symbols[0].uuid"));
    selected[0]["uuid"] = uuid;
    selected[0]["unit"] = json!(2);
    let (error, body) = call(&handler, json!({"schematic":path, "symbols":selected})).await;
    assert!(error, "{body:#}");
    assert_eq!(body["error"]["kind"], "stale_target");
    assert!(body["error"]["reason"]
        .as_str()
        .unwrap()
        .contains("symbols[0].unit"));
    let (error, body) = call(&handler, json!({"schematic":path, "symbols":power})).await;
    assert!(error, "{body:#}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "symbols[0].uuid");
    assert_eq!(snapshot(dir.path()), before);
}

fn copy_fixture(dir: &Path, name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let path = dir.join(Path::new(name).file_name().unwrap());
    std::fs::copy(src.join(name), &path).unwrap();
    path
}

async fn refuses(path: &Path, selected: Vec<Value>, kind: &str, detail: &str) -> Value {
    let before = snapshot(path.parent().unwrap());
    let (error, body) = call(
        &handler().await,
        json!({"schematic":path, "symbols":selected}),
    )
    .await;
    assert!(error, "{body:#}");
    assert_eq!(body["error"]["kind"], kind, "{body:#}");
    assert!(body.to_string().contains(detail), "{body:#}");
    assert_eq!(snapshot(path.parent().unwrap()), before);
    body
}

#[tokio::test]
async fn split_plan_refuses_bus_membership_instead_of_reporting_a_partial_interface() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path(), "bus_connectivity_kicad10.kicad_sch");
    refuses(
        &path,
        vec![json!({"uuid":"b95744ac-f878-4db0-99d4-d0a0d39f2fe0", "unit":1})],
        "plan_blocked",
        "Bus membership",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_a_reused_sheet_before_choosing_a_symbol_instance() {
    let dir = tempfile::tempdir().unwrap();
    for file in [
        "project_ownership/complex_hierarchy.kicad_pro",
        "project_ownership/complex_hierarchy.kicad_sch",
    ] {
        copy_fixture(dir.path(), file);
    }
    let path = copy_fixture(dir.path(), "project_ownership/ampli_ht.kicad_sch");
    let body = refuses(
        &path,
        vec![json!({"uuid":"dab4138b-dcef-4b79-861c-576ddb9d70f2", "unit":1})],
        "ambiguous_target",
        "saved sheet instance",
    )
    .await;
    assert_eq!(body["error"]["candidates"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn split_plan_refuses_scoped_aliases_instead_of_widening_a_net() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path(), "two_name_nets.kicad_sch");
    refuses(
        &path,
        symbols(&path, &["C3"]),
        "plan_blocked",
        "mixed label scopes",
    )
    .await;
}

#[tokio::test]
async fn split_plan_counts_local_aliases_on_one_root_and_sees_name_only_joins() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path(), "two_name_nets.kicad_sch");
    let report = plan(&handler().await, &path, &["TP2"]).await;
    assert_eq!(report["counts"]["signal_ports"], 1);
    let nets = report["boundary_nets"].as_array().unwrap();
    assert_eq!(nets.len(), 1);
    assert_eq!(nets[0]["name"], "AAA");
    assert_eq!(
        nets[0]["aliases"],
        json!([
            {"name":"AAA", "scope":"local"}, {"name":"ZZZ", "scope":"local"}
        ])
    );
    assert_eq!(nets[0]["joined_by"], "name");
    assert_eq!(
        nets[0]["scope_decision"],
        json!(["use_sheet_pin_without_widening_scope", "stop"])
    );
    assert_eq!(pins(&nets[0], "pins_inside"), ["TP2.1"]);
    assert_eq!(pins(&nets[0], "pins_outside"), ["TP3.1"]);
    assert_eq!(nets[0]["sheet_pin"]["shape"], "passive");
    assert_ne!(nets[0]["sheet_pin"]["name_source"], "label");
    assert_eq!(report["boundary_wires"], json!([]));
}

#[tokio::test]
async fn split_plan_refuses_duplicate_placed_uuid_on_corrupt_input() {
    let (_dir, path) = fixture();
    let selected = symbols(&path, &["R1"]);
    let other = symbols(&path, &["R2"]);
    let source = std::fs::read_to_string(&path).unwrap();
    // Deliberately duplicate a placement identifier, not geometry. The input
    // is a corrupt copy of the KiCad-saved fixture, not a new KiCad fixture.
    let uuid = selected[0]["uuid"].as_str().unwrap();
    for duplicate in [uuid.to_owned(), uuid.to_ascii_uppercase()] {
        let corrupt = source.replacen(other[0]["uuid"].as_str().unwrap(), &duplicate, 1);
        std::fs::write(&path, corrupt).unwrap();
        refuses(&path, selected.clone(), "ambiguous_target", "placed UUID").await;
    }
}

#[tokio::test]
async fn split_plan_resolves_uuid_spelling_to_the_saved_placement() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let expected = plan(&handler, &path, &["R1"]).await;
    let mut selected = symbols(&path, &["R1"]);
    selected[0]["uuid"] = json!(selected[0]["uuid"].as_str().unwrap().to_ascii_uppercase());
    let (error, actual) = call(&handler, json!({"schematic":path, "symbols":selected})).await;
    assert!(!error, "{actual:#}");
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn split_plan_refuses_unresolved_symbol_geometry_on_corrupt_input() {
    let (_dir, path) = fixture();
    let selected = symbols(&path, &["R1"]);
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        source.replacen("(lib_id \"Device:C\")", "(lib_id \"Missing:Symbol\")", 1),
    )
    .unwrap();
    refuses(&path, selected, "plan_blocked", "symbol pin geometry").await;
}

#[tokio::test]
async fn split_plan_refuses_unmatched_body_style_with_or_without_saved_pins() {
    let body = "(lib_id \"Device:R\")\n\t\t(at 166.37 83.82 90)\n\t\t(unit 1)\n\t\t(body_style 1)";
    let pins = concat!(
        "\t\t(pin \"1\"\n\t\t\t(uuid \"6fe46a78-9a98-45b0-b86e-156f8f6d167f\")\n\t\t)\n",
        "\t\t(pin \"2\"\n\t\t\t(uuid \"d26b80e5-5f13-4b8f-b93b-27b7dd7368d1\")\n\t\t)\n",
    );
    for omit_saved_pins in [false, true] {
        let (_dir, path) = fixture();
        let selected = symbols(&path, &LED_BLOCK);
        if omit_saved_pins {
            corrupt(&path, pins, "");
        }
        let report = plan(&handler().await, &path, &LED_BLOCK).await;
        assert_eq!(report["counts"]["signal_ports"], 1);
        corrupt(&path, body, &body.replace("body_style 1", "body_style 2"));
        refuses(
            &path,
            selected,
            "plan_blocked",
            "saved symbol pin geometry on R1",
        )
        .await;
    }
}

#[tokio::test]
async fn split_plan_refuses_a_partial_saved_pin_inventory() {
    let (_dir, path) = fixture();
    corrupt(
        &path,
        "\t\t(pin \"2\"\n\t\t\t(uuid \"d26b80e5-5f13-4b8f-b93b-27b7dd7368d1\")\n\t\t)\n",
        "",
    );
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "saved symbol pin geometry on R1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_a_saved_pin_without_embedded_geometry() {
    let (_dir, path) = fixture();
    let source = std::fs::read_to_string(&path).unwrap();
    let marker = "\t\t\t\t(pin tri_state line\n\t\t\t\t\t(at 10.16 10.16 180)";
    let start = source.find(marker).unwrap();
    let end = start + source[start..].find("\t\t\t\t(pin power_in line").unwrap();
    corrupt(&path, &source[start..end], "");
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "saved symbol pin geometry on U1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_an_embedded_pin_without_a_number() {
    let (_dir, path) = fixture();
    let source = std::fs::read_to_string(&path).unwrap();
    let start = source.find("\t\t\t(symbol \"C_1_1\"\n").unwrap();
    let suffix = source[start..].replacen("(number \"1\"", "(number \"\"", 1);
    std::fs::write(&path, format!("{}{suffix}", &source[..start])).unwrap();
    let records = concat!(
        "\t\t(pin \"1\"\n\t\t\t(uuid \"6c4e79b7-b15b-4161-8223-f9bd51edd95a\")\n\t\t)\n",
        "\t\t(pin \"2\"\n\t\t\t(uuid \"51ad80c2-ec26-4996-bd82-27436f60a914\")\n\t\t)\n",
    );
    corrupt(&path, records, "");
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "saved symbol pin geometry on C1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_accepts_a_symbol_with_no_library_or_placed_pins() {
    let (_dir, path) = fixture();
    let source = std::fs::read_to_string(&path).unwrap();
    let start = source.find("\t\t\t(symbol \"C_1_1\"\n").unwrap();
    let stop = start + source[start..].find("\t\t\t(embedded_fonts no)").unwrap();
    let records = concat!(
        "\t\t(pin \"1\"\n\t\t\t(uuid \"6c4e79b7-b15b-4161-8223-f9bd51edd95a\")\n\t\t)\n",
        "\t\t(pin \"2\"\n\t\t\t(uuid \"51ad80c2-ec26-4996-bd82-27436f60a914\")\n\t\t)\n",
    );
    resaved_edits(
        &path,
        &[(&source[start..stop], ""), (records, "")],
        "526c0d415aaad3922596e578058b8a94a7515a487b67500889373e4f42c835f3",
    );
    let report = plan(&handler().await, &path, &["C1"]).await;
    assert_eq!(report["counts"]["parts"], 1);
    assert_eq!(report["internal_nets"], json!([]));
    assert_eq!(report["boundary_nets"], json!([]));
    assert_eq!(report["recommendation"], "keep");
}

#[tokio::test]
async fn split_plan_refuses_non_scalar_names_from_library_pins() {
    for (pin, digest, reason) in [
        (
            "DATA[0..7]",
            "5abc808a077f1b144f560f0c0659f1f2aad7c78d1c4e814256b768e63209a272",
            "Bus membership is not supported: suggested sheet pin 'DATA[0..7]'",
        ),
        (
            "${SER}",
            "b25a0dfc8ee1e17c0f7d60583c7cc859a2b776642fb328aeca5eb7e66f451458",
            "Cannot resolve a saved suggested sheet pin name",
        ),
    ] {
        let (_dir, path) = fixture();
        resaved_variant(&path, "(name \"SER\"", &format!("(name \"{pin}\""), digest);
        refuses(&path, symbols(&path, &["R3"]), "plan_blocked", reason).await;
    }
}

#[tokio::test]
async fn split_plan_keeps_a_block_with_more_signals_than_hidden_nets() {
    let (_dir, path) = fixture();
    let report = plan(&handler().await, &path, &["Q1", "R1"]).await;
    assert_eq!(report["counts"]["internal_nets"], 1);
    assert_eq!(report["counts"]["signal_ports"], 2);
    assert_eq!(report["recommendation"], "keep");
}

#[tokio::test]
async fn split_plan_suggests_output_when_the_block_contains_the_driver() {
    let (_dir, path) = fixture();
    let report = plan(&handler().await, &path, &["U1"]).await;
    let nets = report["boundary_nets"].as_array().unwrap();
    let driven = nets.iter().find(|net| net["name"] == "LED_EN").unwrap();
    assert_eq!(driven["sheet_pin"]["shape"], "output");
    assert_eq!(pins(driven, "pins_inside"), ["U1.15"]);
    assert_eq!(pins(driven, "pins_outside"), ["R1.1"]);
}

#[tokio::test]
async fn split_plan_refuses_duplicate_suggested_ports_on_distinct_nets() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(
        dir.path(),
        "block_placement_kicad10/two_blocks/two_blocks.kicad_pro",
    );
    let path = copy_fixture(
        dir.path(),
        "block_placement_kicad10/two_blocks/two_blocks.kicad_sch",
    );
    refuses(
        &path,
        symbols(&path, &["J1", "J2"]),
        "plan_blocked",
        "More than one signal would use sheet pin 'Pin_1'",
    )
    .await;
}

#[tokio::test]
async fn split_plan_preserves_repeated_top_level_placements_with_the_same_path() {
    let (_dir, path) = fixture();
    let pro = path.with_extension("kicad_pro");
    let mut project: Value = serde_json::from_slice(&std::fs::read(&pro).unwrap()).unwrap();
    let entry = json!({"filename": "split.kicad_sch", "name": "split"});
    project["schematic"]["top_level_sheets"] = json!([entry, entry]);
    std::fs::write(&pro, serde_json::to_vec(&project).unwrap()).unwrap();
    let body = refuses(
        &path,
        symbols(&path, &["R1"]),
        "ambiguous_target",
        "saved sheet instance",
    )
    .await;
    assert_eq!(
        body["error"]["candidates"],
        json!(["/3739b6a5-020c-434f-b147-58f1f845430d"])
    );
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("multiple placements"));
}

/// Reconstruct one resaved variant without duplicating the complete fixture.
/// The hash is from `kicad-cli sch upgrade --force` on KiCad 10.0.8.
fn resaved_variant(path: &Path, old: &str, new: &str, resaved_sha256: &str) {
    resaved_edits(path, &[(old, new)], resaved_sha256);
}

fn resaved_edits(path: &Path, edits: &[(&str, &str)], resaved_sha256: &str) {
    let mut source = std::fs::read_to_string(path).unwrap();
    for (old, new) in edits {
        assert_eq!(source.matches(old).count(), 1);
        source = source.replace(old, new);
    }
    assert_eq!(crate::hex::sha256_hex(source.as_bytes()), resaved_sha256);
    std::fs::write(path, source).unwrap();
}

#[tokio::test]
async fn split_plan_refuses_different_top_level_files_with_the_same_path() {
    let (dir, path) = fixture();
    std::fs::copy(&path, dir.path().join("other.kicad_sch")).unwrap();
    let pro = path.with_extension("kicad_pro");
    let mut project: Value = serde_json::from_slice(&std::fs::read(&pro).unwrap()).unwrap();
    let root = "3739b6a5-020c-434f-b147-58f1f845430d";
    let other = "a71750b6-17ea-4a8e-8383-7943abbcd660";
    project["schematic"]["top_level_sheets"] = json!([
        {"filename": "split.kicad_sch", "name": "split", "uuid": root},
        {"filename": "other.kicad_sch", "name": "other", "uuid": other}
    ]);
    std::fs::write(&pro, serde_json::to_vec(&project).unwrap()).unwrap();
    let instance = format!("/{root}");
    let report = plan(&handler().await, &path, &["R1"]).await;
    assert_eq!(report["sheet_instance_path"], instance);
    project["schematic"]["top_level_sheets"][1]["uuid"] = json!(root.to_ascii_uppercase());
    std::fs::write(&pro, serde_json::to_vec(&project).unwrap()).unwrap();
    let body = refuses(
        &path,
        symbols(&path, &["R1"]),
        "ambiguous_target",
        "saved sheet instance",
    )
    .await;
    assert_eq!(body["error"]["candidates"], json!([&instance]));
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("duplicate placement UUIDs"));
}

#[tokio::test]
async fn split_plan_refuses_sibling_files_with_the_same_placement_uuid() {
    let dir = tempfile::tempdir().unwrap();
    let root = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
    let path = copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
    let mut child = std::fs::read_to_string(&path).unwrap();
    let parent_id = "8f963024-f701-4c8a-84ca-3e41a181c121";
    let placement = "f7757b17-8bab-483c-b920-389a4c502640";
    let instance = format!("/{parent_id}/{placement}");
    child = child
        .replace("(project \"\"", "(project \"single_pin_nets\"")
        .replace("/e1253673-85ae-4c03-a31b-1875f3acaa82", &instance);
    std::fs::write(&path, &child).unwrap();
    std::fs::write(dir.path().join("single_pin_nets.kicad_pro"), "{}\n").unwrap();
    let report = plan(&handler().await, &path, &["R10"]).await;
    assert_eq!(report["sheet_instance_path"], instance);

    // Corrupt only the parent placement identity. The files have different
    // names; the selected child's saved metadata still matches its path.
    let decoy = "a71750b6-17ea-4a8e-8383-7943abbcd660";
    std::fs::write(
        dir.path().join("other.kicad_sch"),
        child.replace("e1253673-85ae-4c03-a31b-1875f3acaa82", decoy),
    )
    .unwrap();
    let source = std::fs::read_to_string(&root).unwrap();
    let (body, sheet) = source.split_once("\n\t(sheet\n").unwrap();
    let other_sheet = sheet
        .replace("single_pin_nets_child.kicad_sch", "other.kicad_sch")
        .replace(placement, decoy);
    let with_sibling = format!(
        "{}\n\t(sheet\n{other_sheet}",
        source.trim_end().strip_suffix(')').unwrap().trim_end()
    );
    assert!(!body.contains(decoy));
    std::fs::write(&root, &with_sibling).unwrap();
    let report = plan(&handler().await, &path, &["R10"]).await;
    assert_eq!(report["sheet_instance_path"], instance);
    std::fs::write(
        &root,
        with_sibling.replace(decoy, &placement.to_ascii_uppercase()),
    )
    .unwrap();
    let error = refuses(
        &path,
        symbols(&path, &["R10"]),
        "ambiguous_target",
        "saved sheet instance",
    )
    .await;
    assert_eq!(error["error"]["candidates"], json!([&instance]));
}

#[tokio::test]
async fn split_plan_refuses_power_names_that_require_text_expansion() {
    let (_dir, path) = fixture();
    let pro = path.with_extension("kicad_pro");
    let mut project: Value = serde_json::from_slice(&std::fs::read(&pro).unwrap()).unwrap();
    project["text_variables"]["LED_RAIL"] = json!("+5V");
    std::fs::write(&pro, serde_json::to_vec(&project).unwrap()).unwrap();
    resaved_variant(
        &path,
        "(property \"Value\" \"+5V\"\n\t\t\t(at 139.7 48.514 0)",
        "(property \"Value\" \"${LED_RAIL}\"\n\t\t\t(at 139.7 48.514 0)",
        "472dcec720267582c556792b5ee44df51efc1cb07fbc58fe89572aa7f3ee9b29",
    );
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "Cannot resolve a saved power name on #PWR08.1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_uses_only_the_active_pin_alternate() {
    let (_dir, path) = fixture();
    let next = "\t\t\t\t)\n\t\t\t\t(pin tri_state line\n\t\t\t\t\t(at 10.16 10.16 180)";
    resaved_variant(
        &path,
        next,
        &format!("\t\t\t\t\t(alternate \"SER_OUT\" output line)\n{next}"),
        "750651879cfbf98f93acc30c96572e8c8f556fceb9c11c3f786a2e0e15cc42a3",
    );
    let handler = handler().await;
    let check = |report: &Value, shape, name| {
        let net = report["boundary_nets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|net| pins(net, "pins_inside") == ["R3.2"])
            .unwrap();
        assert_eq!(net["sheet_pin"]["shape"], shape);
        assert_eq!(net["sheet_pin"]["name"], name);
        assert_eq!(pins(net, "pins_outside"), ["J1.3", "U1.14"]);
    };
    check(&plan(&handler, &path, &["R3"]).await, "passive", "SER");
    let pin = "(uuid \"5c1ff40e-97e0-4c42-b74d-3abf51ab0597\")";
    resaved_variant(
        &path,
        pin,
        &format!("{pin}\n\t\t\t(alternate \"SER_OUT\")"),
        "dee193f031c125f6faf64963bfbd01e618e6bf608bc84ef01456d959c7fe4428",
    );
    check(&plan(&handler, &path, &["R3"]).await, "input", "SER_OUT");
    corrupt(&path, "(alternate \"SER_OUT\")", "(alternate \"UNKNOWN\")");
    refuses(
        &path,
        symbols(&path, &["R3"]),
        "plan_blocked",
        "Cannot resolve pin alternate 'UNKNOWN' on U1.14",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_canonical_collisions_with_a_label_or_proposed_port() {
    for (name, escaped, alias_sha, pair_sha) in [
        (
            "A/B",
            "A{slash}B",
            "ecad8981522d275816f99264fed1c47ac44a7d95b79df58a9eae23381afc5cbc",
            "4b7dde11b244274eba6f060ed763f187df89219926bf167ebb5ad2a232064213",
        ),
        (
            "A|B",
            "A{bar}B",
            "0f8e9f7150fc4074eeb9854624af6aeb4baf044487c3e05190fa57962e6438ec",
            "1de170ff266b0c2707472b1886c954df3924cef2179424ce81805875c7410836",
        ),
    ] {
        let (_dir, path) = fixture();
        let pin = format!("(name \"{name}\"");
        resaved_edits(
            &path,
            &[
                ("(name \"SER\"", &pin),
                ("(label \"LED_K\"", &format!("(label \"{escaped}\"")),
            ],
            alias_sha,
        );
        refuses(
            &path,
            symbols(&path, &["R3", "R1", "R2", "D1", "Q1", "C1"]),
            "plan_blocked",
            &format!("Suggested sheet pin '{name}' matches a different net"),
        )
        .await;
        let (_dir, path) = fixture();
        resaved_edits(
            &path,
            &[
                ("(name \"SER\"", &pin),
                ("(name \"SRCLK\"", &format!("(name \"{escaped}\"")),
            ],
            pair_sha,
        );
        let report = plan(&handler().await, &path, &["R3"]).await;
        let net = report["boundary_nets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|net| pins(net, "pins_inside") == ["R3.2"])
            .unwrap();
        assert_eq!(net["sheet_pin"]["name"], name);
        refuses(
            &path,
            symbols(&path, &["R3", "R4"]),
            "plan_blocked",
            "More than one signal would use sheet pin",
        )
        .await;
    }
}

#[tokio::test]
async fn split_plan_names_a_hidden_rail_from_the_pin_base_name() {
    let (_dir, path) = fixture();
    let next = "\t\t\t\t)\n\t\t\t)\n\t\t\t(symbol \"74HC595_1_1\"";
    resaved_edits(
        &path,
        &[
            (
                "\t\t\t\t\t(name \"VCC\"",
                "\t\t\t\t\t(hide yes)\n\t\t\t\t\t(name \"+5V\"",
            ),
            (
                next,
                &format!("\t\t\t\t\t(alternate \"VDD_ALT\" power_in line)\n{next}"),
            ),
        ],
        "7b30998a4962e44971a77cc6670c498e1327b97ceccfc2702c210ab099540288",
    );
    let handler = handler().await;
    let baseline = plan(&handler, &path, &["U1"]).await;
    let pin = "(uuid \"cd30d07d-48c9-4685-a926-91a11dc226ad\")";
    resaved_variant(
        &path,
        pin,
        &format!("{pin}\n\t\t\t(alternate \"VDD_ALT\")"),
        "b50ac25bcecaa40bcd3c33a64d3ea290f83709df8dd82a3842341363a2396b41",
    );
    let active = plan(&handler, &path, &["U1"]).await;
    assert_eq!(active["boundary_nets"], baseline["boundary_nets"]);
    let supply = active["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["name"] == "+5V")
        .unwrap();
    assert_eq!(
        supply["aliases"],
        json!([{"name":"+5V", "scope":"global_power"}])
    );
    assert_eq!(pins(supply, "pins_inside"), ["U1.16"]);
}

#[tokio::test]
async fn split_plan_refuses_a_bus_label_even_without_drawn_bus_segments() {
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(label \"LED_EN\"",
        "(label \"DATA[0..7]\"",
        "d928b71ca9fbc79ee71a87f2d765c2554751bb09afc456151871419fd11f7228",
    );
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "Bus membership is not supported: net label 'DATA[0..7]'",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_a_sheet_local_rail_crossing_the_block_boundary() {
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(symbol \"power:+5V\"\n\t\t\t(power global)",
        "(symbol \"power:+5V\"\n\t\t\t(power local)",
        "4b1a6eb9a9da17e1cd819522f2ca2c53ad83e6d9133af4fac7fa6ff0b5a7ba61",
    );
    refuses(
        &path,
        symbols(&path, &LED_BLOCK),
        "plan_blocked",
        "Sheet-local power net '+5V' crosses",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_a_suggested_port_matching_a_different_child_net() {
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(label \"LED_K\"",
        "(label \"SER\"",
        "42e57b44631e632884a9923e20cf3d0d82478c9f40c21bc1404522c5a27e0e23",
    );
    let report = plan(&handler().await, &path, &["R3"]).await;
    let nets = report["boundary_nets"].as_array().unwrap();
    let signal = nets
        .iter()
        .find(|net| pins(net, "pins_inside") == ["R3.2"])
        .unwrap();
    assert_eq!(signal["sheet_pin"]["name"], "SER");
    refuses(
        &path,
        symbols(&path, &["R3", "R1", "R2", "D1", "Q1", "C1"]),
        "plan_blocked",
        "Suggested sheet pin 'SER' matches a different net in the block",
    )
    .await;
}

#[tokio::test]
async fn split_plan_retained_bidirectional_pin_does_not_make_child_bidirectional() {
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(pin tri_state line\n\t\t\t\t\t(at 10.16 10.16 180)",
        "(pin bidirectional line\n\t\t\t\t\t(at 10.16 10.16 180)",
        "a56f0584a5fb521ad040389259e075627a68f0d7ba16f7ee162b2b48644ed6f8",
    );
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    let nets = report["boundary_nets"].as_array().unwrap();
    let signal = nets.iter().find(|net| net["name"] == "LED_EN").unwrap();
    assert_eq!(signal["sheet_pin"]["shape"], "input");
    assert_eq!(pins(signal, "pins_inside"), ["R1.1"]);
    assert_eq!(pins(signal, "pins_outside"), ["U1.15"]);
}

#[tokio::test]
async fn split_plan_reports_a_sheet_port_even_without_retained_component_pins() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
    let path = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
    let report = plan(&handler().await, &path, &["R8"]).await;
    let net = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["leaves_sheet"] == true)
        .unwrap();
    assert_eq!(pins(net, "pins_inside"), ["R8.2"]);
    assert_eq!(pins(net, "pins_outside"), Vec::<String>::new());
    assert_eq!(net["joined_by"], "wire");
    assert_eq!(net["sheet_pin"]["shape"], "passive");
}

#[tokio::test]
async fn split_plan_does_not_count_a_no_connect_as_a_hidden_net() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
    let path = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
    let report = plan(&handler().await, &path, &["R5"]).await;
    assert_eq!(report["internal_nets"], json!([]));
    let nets = report["boundary_nets"].as_array().unwrap();
    assert_eq!(nets.len(), 1);
    assert_eq!(pins(&nets[0], "pins_inside"), ["R5.1"]);
    assert_eq!(pins(&nets[0], "pins_outside"), ["R4.2"]);
}

#[tokio::test]
async fn split_plan_reports_the_retained_port_on_a_supporting_part() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
    let path = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
    let report = plan(&handler().await, &path, &["R1"]).await;
    let parts = report["supporting_parts"].as_array().unwrap();
    let part = parts.iter().find(|part| part["reference"] == "R8").unwrap();
    assert_eq!(part["shared_drawn_wires"].as_array().unwrap().len(), 1);
    assert_eq!(part["shared_drawn_wires"][0]["pin"]["number"], "1");
    assert_eq!(part["retained_port"], true);
    assert_eq!(part["would_be_stranded"], false);
}

#[tokio::test]
async fn split_plan_reports_an_alias_on_an_island_without_component_pins() {
    let (_dir, path) = fixture();
    let alias = concat!(
        "\t(label \"LED_EN\"\n",
        "\t\t(at 100.33 40.64 0)\n",
        "\t\t(effects\n\t\t\t(font\n\t\t\t\t(size 1.27 1.27)\n\t\t\t)\n",
        "\t\t\t(justify left bottom)\n\t\t)\n",
        "\t\t(uuid \"8bee21d8-478d-49d4-a0a3-0dfc6d238a32\")\n\t)\n",
    );
    let power = "\t(symbol\n\t\t(lib_id \"power:+5V\")\n\t\t(at 139.7 52.07 0)";
    resaved_variant(
        &path,
        power,
        &format!("{alias}{power}"),
        "571443b6f7ebf00ff09ea1b91854cebe736a1d8ab9f78c85f6b19eb96e07e187",
    );
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    let net = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["name"] == "LED_EN")
        .unwrap();
    assert_eq!(pins(net, "pins_inside"), ["R1.1"]);
    assert_eq!(pins(net, "pins_outside"), ["U1.15"]);
    assert_eq!(net["joined_by"], "both");
}

/// Negative controls for corrupt input. Each edit starts from the same
/// KiCad-saved file; these are deliberately not new schematic fixtures.
fn corrupt(path: &Path, before: &str, after: &str) {
    let source = std::fs::read_to_string(path).unwrap();
    assert!(source.contains(before), "{before}");
    std::fs::write(path, source.replacen(before, after, 1)).unwrap();
}

#[tokio::test]
async fn split_plan_refuses_partial_units() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(
        dir.path(),
        "pcb_sync_multi_unit_keyed_kicad10/kk_sync.kicad_pro",
    );
    let path = copy_fixture(
        dir.path(),
        "pcb_sync_multi_unit_keyed_kicad10/kk_sync.kicad_sch",
    );
    refuses(
        &path,
        symbols(&path, &["U1"]),
        "plan_blocked",
        "Select every placed unit of U1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_accepts_saved_pin_records_for_other_units() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(
        dir.path(),
        "pcb_sync_multi_unit_keyed_kicad10/kk_sync.kicad_pro",
    );
    let path = copy_fixture(
        dir.path(),
        "pcb_sync_multi_unit_keyed_kicad10/kk_sync.kicad_sch",
    );
    let before = snapshot(dir.path());
    let handler = handler().await;
    let unit_one = "ea566a33-6106-497d-ab3a-228e365858d7";
    let report = plan(&handler, &path, &["R1"]).await;
    let boundary = report["boundary_nets"].as_array().unwrap();
    assert_eq!(boundary.len(), 2);
    let peers: BTreeSet<_> = boundary
        .iter()
        .flat_map(|net| {
            net["pins_outside"].as_array().unwrap().iter().map(|pin| {
                (
                    pin["reference"].as_str().unwrap(),
                    pin["number"].as_str().unwrap(),
                    pin["unit"].as_u64().unwrap(),
                )
            })
        })
        .collect();
    assert_eq!(peers, BTreeSet::from([("P1", "1", 1), ("U1", "2", 1)]));
    assert!(boundary
        .iter()
        .flat_map(|n| n["pins_outside"].as_array().unwrap())
        .any(|pin| pin["symbol_uuid"] == unit_one && pin["number"] == "2"));

    let (error, block) = call(
        &handler,
        json!({"schematic": path, "symbols": [
            {"uuid": unit_one, "unit": 1},
            {"uuid": "d6286f98-cc91-4832-97c0-cec9c712e8d3", "unit": 2},
            {"uuid": "1951944c-c6bb-40ae-8bca-507fb207334e", "unit": 5}
        ]}),
    )
    .await;
    assert!(!error, "{block:#}");
    assert_eq!(block["counts"]["parts"], 1);
    let boundary = block["boundary_nets"].as_array().unwrap();
    assert_eq!(boundary.len(), 2);
    let pins: Vec<_> = boundary
        .iter()
        .flat_map(|net| {
            net["pins_inside"].as_array().unwrap().iter().map(|pin| {
                (
                    pin["number"].as_str().unwrap(),
                    pin["unit"].as_u64().unwrap(),
                )
            })
        })
        .collect();
    assert_eq!(pins.len(), 2);
    assert_eq!(
        pins.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([("2", 1), ("4", 1)])
    );
    assert_eq!(snapshot(dir.path()), before);

    // The raw record for an inactive unit is allowed only when the embedded
    // definition is available in this placement's saved body style.
    corrupt(&path, "(symbol \"LM339_3_1\"", "(symbol \"LM339_3_2\"");
    refuses(
        &path,
        symbols(&path, &["R1"]),
        "plan_blocked",
        "saved symbol pin geometry on U1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_ignores_unselected_unannotated_reference_collisions() {
    let (dir, path) = fixture();
    let placeholders = symbols(&path, &["R3", "R4"]);
    resaved_edits(
        &path,
        &[
            (
                "(property \"Reference\" \"R3\"",
                "(property \"Reference\" \"R?\"",
            ),
            (
                "(property \"Reference\" \"R4\"",
                "(property \"Reference\" \"R?\"",
            ),
            ("(reference \"R3\")", "(reference \"R?\")"),
            ("(reference \"R4\")", "(reference \"R?\")"),
        ],
        "b56a4d2db73cf4663e2061cc07414b48e2e07543a33fdbf24b3c5324ca0417ad",
    );
    let before = snapshot(dir.path());
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    assert_eq!(report["recommendation"], "split");
    assert_eq!(report["counts"]["parts"], 4);
    let rail = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["name"] == "+5V")
        .unwrap();
    let peers: Vec<_> = rail["pins_outside"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|pin| pin["reference"] == "R?")
        .collect();
    assert_eq!(peers.len(), 2);
    for selector in &placeholders {
        assert!(peers
            .iter()
            .any(|pin| pin["symbol_uuid"] == selector["uuid"]));
    }
    assert_eq!(snapshot(dir.path()), before);
    let body = refuses(
        &path,
        vec![placeholders[0].clone()],
        "ambiguous_target",
        "placed unit 1 of R?",
    )
    .await;
    assert_eq!(body["error"]["candidates"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn split_plan_refuses_corrupt_symbol_identity() {
    for (before, after, kind, reason) in [
        (
            "(uuid \"3739b6a5-020c-434f-b147-58f1f845430d\")",
            "",
            "stale_target",
            "sheet has no saved UUID",
        ),
        (
            "(project \"split\"",
            "(project \"other\"",
            "stale_target",
            "instance metadata disagrees",
        ),
        (
            "(uuid \"7d78f64c-28f2-4b70-baf6-2b2df536df02\")",
            "",
            "plan_blocked",
            "has no placed UUID",
        ),
    ] {
        let (_dir, path) = fixture();
        let selected = symbols(&path, &["R1"]);
        corrupt(&path, before, after);
        refuses(&path, selected, kind, reason).await;
    }
    let (_dir, path) = fixture();
    let selected = symbols(&path, &["R1"]);
    corrupt(
        &path,
        "(property \"Reference\" \"R2\"",
        "(property \"Reference\" \"R1\"",
    );
    corrupt(&path, "(reference \"R2\")", "(reference \"R1\")");
    refuses(&path, selected, "ambiguous_target", "placed unit 1 of R1").await;
    refuses(
        &path,
        symbols(&path, &["D1"]),
        "ambiguous_target",
        "placed unit 1 of R1",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_unlinked_sheet_and_invalid_source() {
    let (dir, path) = fixture();
    let selected = symbols(&path, &["R1"]);
    let loose = dir.path().join("unlinked.kicad_sch");
    std::fs::copy(&path, &loose).unwrap();
    refuses(&loose, selected.clone(), "conflict", "not linked").await;
    refuses(
        &dir.path().join("missing.kicad_sch"),
        selected.clone(),
        "file_not_found",
        "Cannot read schematic",
    )
    .await;
    std::fs::write(&path, "(not_a_sheet)").unwrap();
    refuses(
        &path,
        selected,
        "malformed_file",
        "expected a KiCad schematic",
    )
    .await;
}

#[tokio::test]
async fn split_plan_refuses_invalid_electrical_objects_independently() {
    for (before, after, reason) in [
        (
            "(xy 63.5 77.47)",
            "(xy invalid 77.47)",
            "endpoints of every wire",
        ),
        (
            "(xy 63.5 77.47)",
            "(xy NaN 77.47)",
            "endpoints of every wire",
        ),
        (
            "(label \"LED_EN\"\n\t\t(at 139.7 81.28 0)",
            "(label \"LED_EN\"",
            "label position",
        ),
        (
            "(at 139.7 81.28 0)\n\t\t(effects",
            "(at NaN 81.28 0)\n\t\t(effects",
            "label position",
        ),
        (
            "(at 139.7 81.28 0)\n\t\t(effects",
            "(at 139.7 NaN 0)\n\t\t(effects",
            "label position",
        ),
        (
            "(label \"LED_EN\"",
            "(label \"\"",
            "Cannot resolve a saved net label name",
        ),
        (
            "(label \"LED_EN\"",
            "(label",
            "Cannot resolve a saved net label name",
        ),
        (
            "(label \"LED_EN\"",
            "(label \"${LED_EN}\"",
            "Cannot resolve a saved net label name",
        ),
        (
            "(uuid \"3d483ff4-a983-4887-8f7f-56f8a12a3c19\")",
            "",
            "saved label has no UUID",
        ),
    ] {
        let (_dir, path) = fixture();
        let selected = symbols(&path, &["R1"]);
        corrupt(&path, before, after);
        refuses(&path, selected, "plan_blocked", reason).await;
    }
    for (tag, uuid_reason, at_reason) in [
        ("wire", "saved wire has no UUID", "endpoints of every wire"),
        (
            "junction",
            "saved junction has no UUID",
            "junction position",
        ),
        (
            "no_connect",
            "saved no_connect has no UUID",
            "no_connect position",
        ),
    ] {
        for (member, reason) in [
            ("uuid", uuid_reason),
            (if tag == "wire" { "pts" } else { "at" }, at_reason),
        ] {
            let (_dir, path) = fixture();
            let source = std::fs::read_to_string(&path).unwrap();
            let offset = source.find(&format!("\n\t({tag}\n")).unwrap();
            let object = &source[offset..];
            let member_start = offset + object.find(&format!("\n\t\t({member}")).unwrap();
            let suffix = &source[member_start..];
            let member_end = if member == "pts" {
                suffix.find("\n\t\t)").unwrap() + 4
            } else {
                suffix.find(')').unwrap() + 1
            };
            let selected = symbols(&path, &["R1"]);
            let mut damaged = source.clone();
            damaged.replace_range(member_start..member_start + member_end, "");
            std::fs::write(&path, damaged).unwrap();
            refuses(&path, selected, "plan_blocked", reason).await;
        }
    }
}

#[tokio::test]
async fn split_plan_refuses_invalid_sheet_pin_names_or_positions() {
    for (from, to, reason) in [
        (
            "(pin \"SHEET_NET\"",
            "(pin \"BUS[0..7]\"",
            "Bus membership is not supported: sheet pin",
        ),
        (
            "(pin \"SHEET_NET\"",
            "(pin \"\"",
            "Cannot resolve a saved sheet pin name",
        ),
        (
            "(pin \"SHEET_NET\" input\n\t\t\t(at 149.86 132.08 180)",
            "(pin \"SHEET_NET\" input",
            "sheet pin position",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
        let path = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
        let selected = symbols(&path, &["R8"]);
        corrupt(&path, from, to);
        refuses(&path, selected, "plan_blocked", reason).await;
    }
}

#[tokio::test]
async fn split_plan_reports_contact_without_treating_it_as_a_drawn_wire() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path(), "pin_contact_kicad10/pin_contact.kicad_sch");
    let report = plan(&handler().await, &path, &["J1"]).await;
    let nets = report["boundary_nets"].as_array().unwrap();
    assert_eq!(nets.len(), 1);
    assert_eq!(pins(&nets[0], "pins_inside"), ["J1.1"]);
    assert_eq!(pins(&nets[0], "pins_outside"), ["R1.1"]);
    assert_eq!(nets[0]["joined_by"], "contact");
    assert_eq!(report["boundary_wires"], json!([]));
    assert_eq!(report["supporting_parts"], json!([]));
}

#[tokio::test]
async fn split_plan_separates_cross_sheet_rails_from_hidden_nets() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let report = plan(
        &handler,
        &path,
        &[
            "C1", "D1", "J1", "Q1", "R1", "R2", "R3", "R4", "R5", "R6", "U1",
        ],
    )
    .await;
    let rail = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "GND")
        .unwrap();
    assert_eq!(report["counts"]["rails"], 2);
    assert_eq!(report["counts"]["signal_ports"], 0);
    assert_eq!(rail["pins_outside"], json!([]));
    assert_eq!(rail["joined_by"], "off_sheet");
    assert_eq!(
        rail["scope_decision"],
        json!(["keep_existing_scope", "stop"])
    );
}

#[tokio::test]
async fn split_plan_respects_integral_json_unit_spellings() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let mut selected = symbols(&path, &["R1"]);
    let (_, normal) = call(&handler, json!({"schematic":path,"symbols":selected})).await;
    selected[0]["unit"] = json!(1.0);
    let (error, decimal) = call(&handler, json!({"schematic":path,"symbols":selected})).await;
    assert!(!error, "{decimal:#}");
    assert_eq!(normal, decimal);
    selected[0]["unit"] = json!(0.5);
    let (error, fraction) = call(&handler, json!({"schematic":path,"symbols":selected})).await;
    assert!(error, "{fraction:#}");
    assert_eq!(fraction["error"]["field"], "symbols[0].unit");
}

#[tokio::test]
async fn split_plan_separates_scope_from_pin_populations() {
    let (_dir, path) = fixture();
    let handler = handler().await;
    let report = plan(&handler, &path, &["R3"]).await;
    assert_eq!(report["counts"]["signal_ports"], 1);
    let support = report["supporting_parts"].as_array().unwrap();
    // J1 shares the SER wire but is not a two-terminal part.
    assert_eq!(
        support
            .iter()
            .filter(|part| part["reference"] == "J1")
            .count(),
        0
    );
    assert_eq!(report["counts"]["rails"], 1);
    let report = plan(&handler, &path, &["R2"]).await;
    let support = report["supporting_parts"].as_array().unwrap();
    assert_eq!(support.len(), 2);
    assert!(support.iter().any(|part| part["reference"] == "C1"));
    assert!(support.iter().any(|part| part["reference"] == "D1"));
    assert_eq!(report["stranded_parts"], json!(["C1"]));
}

#[tokio::test]
async fn split_plan_distinguishes_two_drivers_from_a_bidirectional_child_pin() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(
        dir.path(),
        "block_placement_kicad10/shared_rail/shared_rail.kicad_pro",
    );
    let path = copy_fixture(
        dir.path(),
        "block_placement_kicad10/shared_rail/shared_rail.kicad_sch",
    );
    let handler = handler().await;
    let report = plan(&handler, &path, &["U1", "J1"]).await;
    let driven = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| pins(net, "pins_inside") == ["U1.4"])
        .unwrap();
    assert_eq!(driven["sheet_pin"]["shape"], "unresolved");
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(pin tri_state line\n\t\t\t\t\t(at 10.16 10.16 180)",
        "(pin bidirectional line\n\t\t\t\t\t(at 10.16 10.16 180)",
        "a56f0584a5fb521ad040389259e075627a68f0d7ba16f7ee162b2b48644ed6f8",
    );
    let report = plan(&handler, &path, &["U1"]).await;
    let net = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["name"] == "LED_EN")
        .unwrap();
    assert_eq!(net["sheet_pin"]["shape"], "bidirectional");
}

#[tokio::test]
async fn split_plan_prefers_named_nonpassive_pins_then_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(
        dir.path(),
        "block_placement_kicad10/two_blocks/two_blocks.kicad_pro",
    );
    let path = copy_fixture(
        dir.path(),
        "block_placement_kicad10/two_blocks/two_blocks.kicad_sch",
    );
    let report = plan(&handler().await, &path, &["U1"]).await;
    let nets = report["boundary_nets"].as_array().unwrap();
    let input = nets
        .iter()
        .find(|net| pins(net, "pins_inside") == ["U1.3"])
        .unwrap();
    assert_eq!(input["sheet_pin"]["name"], "U1_3");
    assert_eq!(input["sheet_pin"]["name_source"], "generated");
    let (_dir, path) = fixture();
    let report = plan(&handler().await, &path, &["J1"]).await;
    let ser = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| pins(net, "pins_inside") == ["J1.3"])
        .unwrap();
    assert_eq!(ser["sheet_pin"]["name"], "SER");
}

#[tokio::test]
async fn split_plan_excludes_a_support_part_with_a_nonpassive_pin() {
    let (_dir, path) = fixture();
    resaved_variant(
        &path,
        "(symbol \"C_1_1\"\n\t\t\t\t(pin passive line",
        "(symbol \"C_1_1\"\n\t\t\t\t(pin input line",
        "e5a526fcf4acb2135505b93f92c39691d88462e3b1a0e9d41cfaa1340bb6247d",
    );
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    assert_eq!(report["supporting_parts"], json!([]));
    assert_eq!(report["stranded_parts"], json!([]));
}

#[tokio::test]
async fn split_plan_does_not_suggest_support_through_a_rail_name_alone() {
    let (_dir, path) = fixture();
    let source = std::fs::read_to_string(&path).unwrap();
    let power_marker = "\t(symbol\n\t\t(lib_id \"power:+5V\")\n\t\t(at 139.7 52.07 0)";
    let power_start = source.find(power_marker).unwrap();
    let power_end = power_start + source[power_start + 1..].find("\n\t(symbol\n").unwrap() + 2;
    let supply = source[power_start..power_end]
        .replace("(at 139.7 ", "(at 186.69 ")
        .replace("#PWR08", "#PWR0999")
        .replace(
            "12c755d4-a823-4ac2-94b5-184fb8867c3c",
            "d2630e45-1731-5312-86ea-176ee586bbe2",
        )
        .replace(
            "b723460f-1347-4df2-87a6-8fe5b8ab80df",
            "4e4dfaab-86bf-5bb9-a4a9-c81babfa4836",
        );
    let wire = concat!(
        "\t(wire\n\t\t(pts\n\t\t\t(xy 139.7 52.07) (xy 186.69 52.07)\n\t\t)\n",
        "\t\t(stroke\n\t\t\t(width 0)\n\t\t\t(type default)\n\t\t)\n",
        "\t\t(uuid \"42479cf7-b378-4678-943a-631dedd045bc\")\n\t)\n",
    );
    let next = "\t(symbol\n\t\t(lib_id \"power:GND\")\n\t\t(at 48.26 88.9 0)";
    let variant = source
        .replacen(wire, "", 1)
        .replacen(next, &format!("{supply}{next}"), 1);
    resaved_variant(
        &path,
        &source,
        &variant,
        "34ed8083a6ad470d1193c43e8288398db344bbcd69427854071024268fef074c",
    );
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    assert_eq!(report["supporting_parts"], json!([]));
    assert_eq!(report["stranded_parts"], json!([]));
    let supply = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|net| net["name"] == "+5V")
        .unwrap();
    assert_eq!(supply["joined_by"], "name");
    assert_eq!(pins(supply, "pins_inside"), ["R2.1"]);
    assert_eq!(
        pins(supply, "pins_outside"),
        ["C1.1", "J1.1", "R3.1", "R4.1", "R5.1", "R6.1", "U1.16"]
    );
}

fn wire_consumer_to_support(path: &Path) {
    let line = |ends: &str, uuid: &str, next: &str| {
        format!(
            "{ends}\n\t\t)\n\t\t(stroke\n\t\t\t(width 0)\n\t\t\t(type default)\n\t\t)\n\
             \t\t(uuid \"{uuid}\")\n\t)\n\t(wire\n\t\t(pts\n{next}"
        )
    };
    let after_first = "\t\t\t(xy 78.74 77.47) (xy 78.74 99.06)";
    let after_second = "\t\t\t(xy 63.5 81.28) (xy 110.49 81.28)";
    resaved_edits(
        path,
        &[
            (
                after_first,
                &line(
                    "\t\t\t(xy 120.65 52.07) (xy 139.7 52.07)",
                    "2c43ae03-cf27-5e90-a33a-5a0ca5b9dad3",
                    after_first,
                ),
            ),
            (
                after_second,
                &line(
                    "\t\t\t(xy 120.65 52.07) (xy 120.65 76.2)",
                    "467e8400-0d99-5378-8a0d-cf7980061619",
                    after_second,
                ),
            ),
        ],
        "17d8c2d55af882740fed5692d1438fb1b2fe754b14f423c106024c93c5085b8e",
    );
}

#[tokio::test]
async fn split_plan_excludes_a_drawn_consumer_from_name_only_alternatives() {
    let (_dir, path) = fixture();
    wire_consumer_to_support(&path);
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    let parts = report["supporting_parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["reference"], "C1");
    assert_eq!(parts[0]["other_rail_consumers"], json!(["U1.8"]));
}

#[tokio::test]
async fn split_plan_measures_nearby_consumers_from_pin_endpoints() {
    let (dir, path) = fixture();
    wire_consumer_to_support(&path);
    resaved_edits(
        &path,
        &[
            ("(at 0 15.24 270)", "(at 0 21.59 270)"),
            (
                "(xy 120.65 52.07) (xy 120.65 76.2)",
                "(xy 120.65 52.07) (xy 120.65 69.85)",
            ),
            (
                "(lib_id \"power:+5V\")\n\t\t(at 120.65 76.2 0)",
                "(lib_id \"power:+5V\")\n\t\t(at 120.65 69.85 0)",
            ),
        ],
        "64a5992b281b88d2ea7a7956c3dac95cfa03aed866b8a1770561da34632f93e9",
    );
    let before = snapshot(dir.path());
    let report = plan(&handler().await, &path, &LED_BLOCK).await;
    let parts = report["supporting_parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["reference"], "C1");
    assert_eq!(parts[0]["retained_nearby_pins"], json!(["U1.16"]));
    assert_eq!(parts[0]["would_be_stranded"], false);
    assert_eq!(report["stranded_parts"], json!([]));
    assert_eq!(snapshot(dir.path()), before);
}

#[tokio::test]
async fn split_plan_joins_selected_islands_to_a_retained_sheet_pin() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path(), "single_pin_nets_child.kicad_sch");
    let path = copy_fixture(dir.path(), "single_pin_nets.kicad_sch");
    let label = concat!(
        "\t(label \"SPLIT\"\n",
        "\t\t(at 140.97 132.08 0)\n",
        "\t\t(effects\n\t\t\t(font\n\t\t\t\t(size 1.27 1.27)\n\t\t\t)\n",
        "\t\t\t(justify left bottom)\n\t\t)\n",
        "\t\t(uuid \"8bee21d8-478d-49d4-a0a3-0dfc6d238a32\")\n\t)\n",
    );
    resaved_variant(
        &path,
        "\t(label \"SHEET_NET\"",
        &format!("{label}\t(label \"SHEET_NET\""),
        "c07eb03dc54c21832078bf1fc7f3238819f3d7d7ae544fc7420b3a7a398f0842",
    );
    let report = plan(&handler().await, &path, &["R4", "R5", "R8"]).await;
    let net = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["leaves_sheet"] == true)
        .unwrap();
    assert_eq!(pins(net, "pins_inside"), ["R4.2", "R5.1", "R8.2"]);
    assert_eq!(pins(net, "pins_outside"), Vec::<String>::new());
    assert_eq!(net["joined_by"], "both");
}

#[tokio::test]
async fn split_plan_requires_preserving_project_wide_signal_scope() {
    let (_dir, path) = fixture();
    let start = std::fs::read_to_string(&path).unwrap();
    let first = start.find("\t(label \"LED_EN\"").unwrap();
    let second = start.find("\t(label \"LED_K\"").unwrap();
    let rest = start[second..].find("\t(symbol\n").unwrap() + second;
    let global = start[first..second].replacen(
        "(label \"LED_EN\"\n",
        "(global_label \"LED_EN\"\n\t\t(shape input)\n",
        1,
    );
    let uuid = "\t\t(uuid \"3d483ff4-a983-4887-8f7f-56f8a12a3c19\")\n";
    let meta = concat!(
        "\t\t(property \"Intersheetrefs\" \"${INTERSHEET_REFS}\"\n",
        "\t\t\t(at 139.7 81.28 0)\n\t\t\t(hide yes)\n",
        "\t\t\t(show_name no)\n\t\t\t(do_not_autoplace no)\n",
        "\t\t\t(effects\n\t\t\t\t(font\n\t\t\t\t\t(size 1.27 1.27)\n",
        "\t\t\t\t)\n\t\t\t)\n\t\t)\n",
    );
    let global = global.replace(uuid, &format!("{uuid}{meta}"));
    resaved_variant(
        &path,
        &start[first..rest],
        &format!("{}{global}", &start[second..rest]),
        "a3c7185fa0e0221e997d8c77bc17fc6f51ae338293e6b85c8f0a2ce2f456d23d",
    );
    let report = plan(
        &handler().await,
        &path,
        &["U1", "R1", "J1", "R2", "R3", "R4", "R5", "R6"],
    )
    .await;
    let net = report["boundary_nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "LED_EN")
        .unwrap();
    assert_eq!(report["counts"]["global_nets"], 1);
    assert_eq!(net["kind"], "global");
    assert_eq!(net["aliases"], json!([{"name":"LED_EN", "scope":"global"}]));
    assert_eq!(net["pins_outside"], json!([]));
    assert_eq!(
        net["scope_decision"],
        json!(["keep_existing_scope", "stop"])
    );
    assert!(net.get("sheet_pin").is_none());
}
