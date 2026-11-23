//! Read-only tools give the answers they gave before power-symbol stubs
//! (#773) taught the shared overlap, pin-contact and library-pin code to
//! compare snapshots. The tools served every value below identically before
//! and after that change, on the same fixtures.

use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};
use std::path::Path;

const POWER_ORIENTATION: &str = "power_orientation_kicad10/power_orientation.kicad_sch";
const PIN_CONTACT: &str = "pin_contact_kicad10/pin_contact.kicad_sch";
const MIXER: &str = "multichannel_mixer.kicad_sch";
const DIRECT_LANES: &str = "direct_lanes_kicad10/direct_lanes.kicad_sch";
const PIN_NET_NAMES: &str = "pin_net_names_kicad10.kicad_sch";
const ECC83: &str = "ecc83_multiunit.kicad_sch";
const WORKED_EXAMPLE: &str = "worked_example_kicad10/before/worked_example.kicad_sch";
const COINCIDENT_LABELS: &str = "coincident_labels_kicad10.kicad_sch";

/// Two placed symbols without library definitions at one origin, and two
/// local labels with different nets at one anchor: the fallback component
/// overlap and the label overlap.
const STACKED: &str = r#"(kicad_sch (version 20250114) (generator "eeschema") (uuid "5d1c0b1e-0000-4000-8000-000000000001") (paper "A4")
	(lib_symbols)
	(label "SIG_A" (at 50.8 50.8 0) (effects (font (size 1.27 1.27))) (uuid "5d1c0b1e-0000-4000-8000-000000000011"))
	(label "SIG_B" (at 50.8 50.8 0) (effects (font (size 1.27 1.27))) (uuid "5d1c0b1e-0000-4000-8000-000000000012"))
	(symbol (lib_id "Missing:Part") (at 101.6 101.6 0) (unit 1) (uuid "5d1c0b1e-0000-4000-8000-000000000021")
		(property "Reference" "U1" (at 101.6 96.52 0) (effects (font (size 1.27 1.27)) (hide yes))))
	(symbol (lib_id "Missing:Part") (at 101.6 101.6 0) (unit 1) (uuid "5d1c0b1e-0000-4000-8000-000000000022")
		(property "Reference" "U2" (at 101.6 96.52 0) (effects (font (size 1.27 1.27)) (hide yes))))
)
"#;

/// Serve `tool` twice on a copy of `fixture`.
async fn answer(tool: &str, fixture: &str) -> Value {
    answer_with(tool, fixture, json!({})).await
}

async fn answer_with(tool: &str, fixture: &str, args: Value) -> Value {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(source.file_name().unwrap());
    std::fs::copy(&source, &path).unwrap();
    let project = source.with_extension("kicad_pro");
    if project.exists() {
        std::fs::copy(project, path.with_extension("kicad_pro")).unwrap();
    }
    serve(tool, &path, args, fixture).await
}

async fn answer_inline(tool: &str, sheet: &str) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stacked.kicad_sch");
    std::fs::write(&path, sheet).unwrap();
    serve(tool, &path, json!({}), "stacked").await
}

/// Both answers must match, and the sheet must be left untouched.
async fn serve(tool: &str, path: &Path, mut args: Value, name: &str) -> Value {
    let before = std::fs::read(path).unwrap();
    let handler = McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    args["schematic"] = json!(path);
    let request = json!({"jsonrpc": "2.0", "id": 773, "method": "tools/call",
        "params": {"name": tool, "arguments": args}});
    let mut answers = Vec::new();
    for _ in 0..2 {
        let result = handler
            .handle_message(request.clone())
            .await
            .unwrap()
            .result
            .unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_ne!(result["isError"], true, "{tool} on {name}: {text}");
        answers.push(serde_json::from_str::<Value>(text).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), before, "{tool} on {name}");
    }
    assert_eq!(answers[0], answers[1], "{tool} on {name}");
    answers.swap_remove(0)
}

/// `(pin, net)` for each pin `get_component_nets` reports, in its order.
async fn component_nets(fixture: &str, reference: &str) -> Vec<(String, Option<String>)> {
    let answer = answer_with(
        "get_component_nets",
        fixture,
        json!({"reference": reference}),
    )
    .await;
    answer["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pin| {
            (
                pin["pin"].as_str().unwrap().to_owned(),
                pin["net"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn nets(expected: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    expected
        .iter()
        .map(|(pin, net)| ((*pin).to_owned(), net.map(str::to_owned)))
        .collect()
}

fn overlap_kinds(answer: &Value) -> Vec<&str> {
    answer["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|overlap| overlap["type"].as_str().unwrap())
        .collect()
}

fn unconnected(answer: &Value) -> Vec<String> {
    answer["unconnected_pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pin| {
            format!(
                "{}.{}",
                pin["reference"].as_str().unwrap(),
                pin["pin"].as_str().unwrap()
            )
        })
        .collect()
}

/// Whole-sheet overlap answers are unfiltered: no symbol counts as changed,
/// and library text that the clearance helper reports as unmeasured does not
/// mark an otherwise resolved symbol.
#[tokio::test]
async fn whole_sheet_overlap_answers_are_unchanged() {
    for (fixture, resolved, kinds) in [
        (POWER_ORIENTATION, 12, vec![]),
        (PIN_CONTACT, 23, vec![]),
        (MIXER, 25, vec![]),
        (DIRECT_LANES, 3, vec!["wire_field_overlap"; 3]),
        (PIN_NET_NAMES, 29, vec![]),
        (ECC83, 26, vec!["text_overlap"]),
        (WORKED_EXAMPLE, 11, vec!["wire_field_overlap"; 2]),
        (
            COINCIDENT_LABELS,
            1,
            vec!["text_overlap", "wire_label_overlap"],
        ),
    ] {
        let overlaps = answer("check_schematic_overlaps", fixture).await;
        assert_eq!(
            overlaps["bounds_resolved"], resolved,
            "{fixture}: {overlaps}"
        );
        assert_eq!(
            overlaps["bounds_unresolved"],
            json!([]),
            "{fixture}: {overlaps}"
        );
        assert_eq!(overlap_kinds(&overlaps), kinds, "{fixture}: {overlaps}");
        assert_eq!(overlaps["overlap_count"], kinds.len(), "{fixture}");
        for overlap in overlaps["overlaps"].as_array().unwrap() {
            let object = overlap.as_object().unwrap();
            assert!(
                !object.keys().any(|key| key.starts_with('_')),
                "{fixture}: {overlap}"
            );
        }

        let layout = answer("compare_schematic_layout", fixture).await;
        assert_eq!(layout["not_compared"], json!([]), "{fixture}: {layout}");
        assert_eq!(layout["score"]["bounds_unresolved"], json!([]), "{fixture}");
    }

    let lanes = answer("check_schematic_overlaps", DIRECT_LANES).await;
    let crossings: Vec<_> = lanes["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|overlap| {
            (
                overlap["a"]["uuid"].as_str().unwrap(),
                overlap["b"]["reference"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        crossings,
        [
            ("676cb32a-f015-4eb4-8a3f-5b9364d0e345", "C1"),
            ("c9ac38c6-043c-4795-93f9-b053e790821e", "C1"),
            ("c9ac38c6-043c-4795-93f9-b053e790821e", "C1"),
        ]
    );
}

/// The origin fallback and label overlaps keep their public shape: no
/// participant or anchor fields from the snapshot comparison.
#[tokio::test]
async fn fallback_and_label_overlap_answers_are_unchanged() {
    let overlaps = answer_inline("check_schematic_overlaps", STACKED).await;
    assert_eq!(
        overlaps,
        json!({
            "bounds_resolved": 0,
            "bounds_unresolved": ["U1", "U2"],
            "overlap_count": 2,
            "overlaps": [
                {"type": "component_overlap", "a": "U1", "unit_a": 1, "b": "U2", "unit_b": 1,
                 "x": 101.6, "y": 101.6, "detection": "origin_fallback"},
                {"type": "label_overlap", "net_a": "SIG_A", "net_b": "SIG_B", "x": 50.8, "y": 50.8},
            ],
        })
    );
    let layout = answer_inline("compare_schematic_layout", STACKED).await;
    assert_eq!(layout["score"]["bounds_unresolved"], json!(["U1", "U2"]));
    assert_eq!(layout["score"]["label_overlap"], 1);
    assert_eq!(layout["score"]["component_overlap"], Value::Null);
}

/// The public contact report keeps each iterating pin's own position and
/// reports no internal identity fields.
#[tokio::test]
async fn pin_contact_answers_are_unchanged() {
    for (fixture, points, short_wires) in [
        (POWER_ORIENTATION, vec![], 0),
        (
            PIN_CONTACT,
            vec![(76.2, 54.61), (55.88, 50.8), (93.98, 46.99)],
            1,
        ),
        (MIXER, vec![], 0),
        (PIN_NET_NAMES, vec![(165.1, 85.09)], 0),
        (ECC83, vec![], 0),
    ] {
        let contacts = answer("find_pin_contacts", fixture).await;
        let observed: Vec<_> = contacts["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|contact| {
                (
                    contact["point"]["x"].as_f64().unwrap(),
                    contact["point"]["y"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(observed, points, "{fixture}: {contacts}");
        assert_eq!(contacts["short_wire_count"], short_wires, "{fixture}");
        for contact in contacts["contacts"].as_array().unwrap() {
            for pin in contact["pins"].as_array().unwrap() {
                let mut keys: Vec<_> = pin
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    [
                        "is_power_symbol",
                        "pin_number",
                        "reference",
                        "symbol_uuid",
                        "unit"
                    ],
                    "{fixture}: {contact}"
                );
            }
        }
    }
}

/// Pins come from the placed unit and body style only, as before.
#[tokio::test]
async fn unit_and_body_style_pin_answers_are_unchanged() {
    assert_eq!(
        component_nets(PIN_NET_NAMES, "U1").await,
        nets(&[
            ("1", Some("Net-(U1-VIN)")),
            ("2", None),
            ("3", None),
            ("4", None),
            ("5", None)
        ])
    );
    assert_eq!(
        component_nets(PIN_NET_NAMES, "U2").await,
        nets(&[("1", None), ("2", None), ("3", Some("Net-(U2A-+)"))])
    );
    assert_eq!(
        component_nets(PIN_NET_NAMES, "U4").await,
        nets(&[
            ("1", None),
            ("2", None),
            ("3", None),
            ("1", None),
            ("2", None),
            ("3", None)
        ])
    );
    let j1 = component_nets(PIN_NET_NAMES, "J1").await;
    let unnamed: Vec<_> = j1
        .iter()
        .filter(|(_, net)| net.is_none())
        .map(|(pin, _)| pin.as_str())
        .collect();
    assert_eq!(
        unnamed,
        ["A7", "A8", "B5", "B6", "B7", "B8", "SH"],
        "{j1:?}"
    );
    assert_eq!(
        component_nets(ECC83, "U1").await,
        nets(&[
            ("7", Some("Net-(U1A-G)")),
            ("8", Some("Net-(U1A-K)")),
            ("6", Some("Net-(P3-P1)")),
            ("2", Some("Net-(P1-PM)")),
            ("3", Some("Net-(U1B-K)")),
            ("1", Some("Net-(U1A-G)")),
            ("4", Some("Net-(P4-PM)")),
            ("9", Some("Net-(P4-P1)")),
            ("5", Some("Net-(P4-PM)")),
        ])
    );

    let summary = answer("export_netlist_summary", ECC83).await;
    let tubes: Vec<_> = summary["components"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|component| component["reference"] == "U1")
        .map(|component| component["pin_count"].as_u64().unwrap())
        .collect();
    assert_eq!(tubes, [3, 3, 3], "{summary}");
    let ecc83 = answer("validate_component_connections", ECC83).await;
    assert_eq!(ecc83["valid"], true, "{ecc83}");

    let summary = answer("export_netlist_summary", PIN_NET_NAMES).await;
    let pin_counts: Vec<_> = ["U1", "U2", "U3", "U4", "J1", "J2"]
        .into_iter()
        .map(|reference| {
            let component = summary["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|component| component["reference"] == reference)
                .unwrap();
            component["pin_count"].as_u64().unwrap()
        })
        .collect();
    assert_eq!(pin_counts, [5, 3, 8, 6, 17, 17], "{summary}");
    let names = answer("validate_component_connections", PIN_NET_NAMES).await;
    assert_eq!(names["unconnected_count"], 55, "{names}");
    let units: Vec<_> = unconnected(&names)
        .into_iter()
        .filter(|pin| pin.starts_with('U'))
        .collect();
    assert_eq!(
        units,
        [
            "U3.1", "U3.2", "U3.3", "U3.4", "U3.5", "U3.6", "U3.7", "U1.2", "U1.3", "U1.5", "U2.1",
            "U2.2"
        ],
        "{names}"
    );

    let mixer = answer("export_netlist_summary", MIXER).await;
    let amplifiers: Vec<_> = mixer["components"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|component| component["reference"] == "IC1")
        .map(|component| component["pin_count"].as_u64().unwrap())
        .collect();
    assert_eq!(amplifiers, [10, 10], "{mixer}");
    let power = answer("validate_component_connections", POWER_ORIENTATION).await;
    assert_eq!(power["unconnected_count"], 21, "{power}");
}
