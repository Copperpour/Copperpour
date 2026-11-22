use super::sch_connectivity::{net_graph_for, pt_key, NetRoot};
use crate::mcp::handler::McpHandler;
use crate::tools::{is_power_symbol_reference, PlacedPins, ServerConfig};
use copperpour_sexp::schematic::{
    extract_all_net_labels, extract_no_connects, extract_symbol_instances, extract_wires,
    read_schematic,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/worked_example_kicad10"
);

fn copy_project(dir: &Path, variant: &str) -> PathBuf {
    let source = Path::new(FIXTURES).join(variant);
    let path = dir.join("worked_example.kicad_sch");
    std::fs::copy(source.join("worked_example.kicad_sch"), &path).unwrap();
    std::fs::copy(
        source.join("worked_example.kicad_pro"),
        dir.join("worked_example.kicad_pro"),
    )
    .unwrap();
    path
}

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn call(h: &McpHandler, name: &str, path: &Path, mut args: Value) -> Value {
    args["schematic"] = json!(path);
    let response = h
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 774,
            "method": "tools/call", "params": { "name": name, "arguments": args }
        }))
        .await
        .unwrap();
    let result = response
        .result
        .unwrap_or_else(|| panic!("{name}: {:?}", response.error));
    assert_ne!(result["isError"], true, "{name}: {result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

type Pin = (String, String);
type PinGroup = BTreeSet<Pin>;
type NetNames = BTreeMap<PinGroup, Option<String>>;

const UNUSED: [(&str, &str); 4] = [("SW1", "1"), ("U1", "1"), ("U1", "3"), ("U1", "5")];

#[derive(Debug, PartialEq, Eq)]
struct Circuit {
    uuids: BTreeMap<String, String>,
    groups: NetNames,
}

async fn circuit(h: &McpHandler, path: &Path) -> Circuit {
    let (_, tree) = read_schematic(path).unwrap();
    let labels = extract_all_net_labels(&tree);
    let wires = extract_wires(&tree);
    let mut graph = net_graph_for(&tree, &wires, &labels);
    let mut uuids = BTreeMap::new();
    let mut groups: BTreeMap<NetRoot, (PinGroup, Option<String>)> = BTreeMap::new();
    for symbol in extract_symbol_instances(&tree) {
        let reference = symbol.reference;
        if is_power_symbol_reference(&reference) {
            continue;
        }
        assert!(
            uuids
                .insert(reference.clone(), symbol.uuid.unwrap())
                .is_none(),
            "{reference}"
        );
        let result = call(
            h,
            "get_component_nets",
            path,
            json!({ "reference": reference }),
        )
        .await;
        for pin in result["pins"].as_array().unwrap() {
            let number = pin["pin"].as_str().unwrap().to_owned();
            let name =
                serde_json::from_value::<Option<String>>(pin.get("net").unwrap().clone()).unwrap();
            // Use the graph root for membership; the served name is checked separately.
            let root = graph.root_at(pin["x"].as_f64().unwrap(), pin["y"].as_f64().unwrap());
            let (members, observed_name) = groups
                .entry(root)
                .or_insert_with(|| (BTreeSet::new(), name.clone()));
            assert_eq!(*observed_name, name, "{reference}.{number}");
            assert!(members.insert((reference.clone(), number)));
        }
    }
    Circuit {
        uuids,
        groups: groups.into_values().collect(),
    }
}

fn pins(members: &[(&str, &str)]) -> PinGroup {
    members
        .iter()
        .map(|&(reference, pin)| (reference.to_owned(), pin.to_owned()))
        .collect()
}

fn expected_nets(after: bool) -> NetNames {
    // From the KiCad 10.0.8 netlists (see the fixture README). Copperpour omits
    // the root-sheet slash on local labels and reports unused pins as null.
    let label = |old, new| if after { new } else { old };
    let named = [
        (
            "+3V3",
            pins(&[("BT1", "1"), ("R1", "1"), ("R3", "1"), ("U1", "8")]),
        ),
        (
            "GND",
            pins(&[
                ("SW1", "2"),
                ("R2", "2"),
                ("R4", "2"),
                ("MK1", "1"),
                ("U1", "4"),
            ]),
        ),
        ("Net-(BT1--)", pins(&[("BT1", "2"), ("SW1", "3")])),
        ("Net-(D1-K)", pins(&[("D1", "1"), ("R4", "1")])),
        (
            label("Net-(D1-A)", "LED"),
            pins(&[("D1", "2"), ("U1", "6")]),
        ),
        (
            label("Net-(MK1-+)", "MIC"),
            pins(&[("MK1", "2"), ("R3", "2"), ("U1", "2")]),
        ),
        (
            label("Net-(U1-PB2)", "LDR"),
            pins(&[("R1", "2"), ("R2", "1"), ("U1", "7")]),
        ),
    ];
    named
        .into_iter()
        .map(|(name, members)| (members, Some(name.to_owned())))
        .chain(UNUSED.map(|pin| (pins(&[pin]), None)))
        .collect()
}

#[tokio::test]
async fn after_keeps_every_pin_of_before_on_its_net() {
    let h = handler().await;
    let before_dir = tempfile::tempdir().unwrap();
    let after_dir = tempfile::tempdir().unwrap();
    let before = copy_project(before_dir.path(), "before");
    let after = copy_project(after_dir.path(), "after");
    let applied = call(&h, "annotate_schematic", &before, json!({})).await;
    assert_eq!(applied["remaining_unannotated"], 0, "{applied}");
    assert_eq!(
        applied["assigned"].as_array().unwrap().len(),
        9,
        "{applied}"
    );

    let before = circuit(&h, &before).await;
    let after = circuit(&h, &after).await;
    assert_eq!(before.uuids.len(), 9);
    assert_eq!(before.uuids, after.uuids);
    assert_eq!(before.groups, expected_nets(false), "annotated before");
    assert_eq!(after.groups, expected_nets(true), "after");
}

#[tokio::test]
async fn a_dropped_label_is_an_electrical_change() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = copy_project(dir.path(), "after");
    let intact = circuit(&h, &path).await;
    call(
        &h,
        "delete_schematic_net_label",
        &path,
        json!({ "net": "MIC", "x": 200.66, "y": 83.82 }),
    )
    .await;
    let labels = call(&h, "list_schematic_labels", &path, json!({})).await;
    let mic: Vec<_> = labels["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|label| label["net"] == "MIC")
        .map(|label| (label["x"].as_f64().unwrap(), label["y"].as_f64().unwrap()))
        .collect();
    assert_eq!(
        mic,
        [(124.46, 86.36)],
        "only the MCU-side MIC label should remain"
    );
    let changed = circuit(&h, &path).await;

    let mut expected: BTreeSet<_> = intact.groups.into_keys().collect();
    assert!(expected.remove(&pins(&[("U1", "2"), ("R3", "2"), ("MK1", "2")])));
    expected.insert(pins(&[("U1", "2")]));
    expected.insert(pins(&[("R3", "2"), ("MK1", "2")]));
    assert_eq!(
        changed.groups.into_keys().collect::<BTreeSet<_>>(),
        expected
    );
}

#[tokio::test]
async fn before_has_four_unflagged_pins_and_after_has_none() {
    let h = handler().await;
    for (variant, expected) in [
        ("before", &["SW?.1", "U?.1", "U?.3", "U?.5"][..]),
        ("after", &[]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = copy_project(dir.path(), variant);
        let result = call(&h, "find_orphan_items", &path, json!({})).await;
        let mut found: Vec<_> = result["orphans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|orphan| {
                assert_eq!(orphan["type"], "unconnected_pin", "{variant}: {orphan}");
                format!(
                    "{}.{}",
                    orphan["reference"].as_str().unwrap(),
                    orphan["pin"].as_str().unwrap()
                )
            })
            .collect();
        found.sort();
        assert_eq!(found, expected, "{variant}");
    }
}

#[test]
fn only_after_flags_the_four_unused_pin_endpoints() {
    for variant in ["before", "after"] {
        let path = Path::new(FIXTURES)
            .join(variant)
            .join("worked_example.kicad_sch");
        let (_, tree) = read_schematic(&path).unwrap();
        let flags = extract_no_connects(&tree);
        if variant == "before" {
            assert!(flags.is_empty(), "{flags:?}");
        } else {
            let placed = PlacedPins::new(&tree);
            let expected: BTreeSet<_> = UNUSED
                .iter()
                .map(|&(reference, number)| {
                    let (x, y) = placed.resolve(reference, number).ok().unwrap().endpoint();
                    pt_key(x, y)
                })
                .collect();
            assert_eq!(flags.len(), UNUSED.len());
            assert_eq!(
                flags
                    .into_iter()
                    .map(|(x, y)| pt_key(x, y))
                    .collect::<BTreeSet<_>>(),
                expected
            );
        }
    }
}

#[tokio::test]
async fn dry_run_finds_nine_parts_to_annotate_only_in_before() {
    let h = handler().await;
    for (variant, count) in [("before", 9), ("after", 0)] {
        let dir = tempfile::tempdir().unwrap();
        let path = copy_project(dir.path(), variant);
        let original = std::fs::read(&path).unwrap();
        let plan = call(&h, "annotate_schematic", &path, json!({ "dry_run": true })).await;
        assert_eq!(plan["unannotated_before"], count, "{variant}: {plan}");
        assert_eq!(
            plan["assigned"].as_array().unwrap().len(),
            count,
            "{variant}: {plan}"
        );
        assert!(
            std::fs::read(&path).unwrap() == original,
            "dry run wrote to {variant}"
        );
    }
}
