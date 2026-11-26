use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};

const OPAMP: &str =
    include_str!("fixtures/block_placement_kicad10/opamp_stage/opamp_stage.kicad_sch");

#[tokio::test]
async fn the_offset_label_retains_its_legacy_connectivity_answers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opamp_stage.kicad_sch");
    let (sheet, _) = OPAMP.trim_end().rsplit_once(')').unwrap();
    // This is the output trunk between U1.1 and J2.1. The preview must
    // distinguish a 0.001 mm near-miss, but the existing tools use their
    // legacy 0.01 mm attachment tolerance, pending #901.
    let sheet = format!(
        r#"{sheet}
        (label "TOLERANT_OUT" (at 165.1 88.901 90)
          (effects (font (size 1.27 1.27)) (justify left bottom))
          (uuid "77600000-0000-4000-8000-000000000008"))
        )"#
    );
    std::fs::write(&path, &sheet).unwrap();
    let handler = McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let call = async |name: &str, mut args: Value| {
        args["schematic"] = json!(path);
        let result = handler
            .handle_message(json!({"jsonrpc": "2.0", "id": 776, "method": "tools/call",
                "params": {"name": name, "arguments": args}}))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(result["isError"], true, "{name}: {result}");
        assert_eq!(std::fs::read(&path).unwrap(), sheet.as_bytes());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };

    let trace = call("trace_from_point", json!({"x": 190.5, "y": 88.9})).await;
    let connected = call("get_connected_items", json!({"reference": "J2"})).await;
    assert_eq!(trace["net"], "TOLERANT_OUT", "{trace}");
    assert_eq!(trace["pins_here"][0]["reference"], "J2", "{trace}");
    assert_eq!(trace["pins_here"][0]["pin"], "1", "{trace}");
    assert_eq!(
        connected["nets"],
        json!(["GND", "TOLERANT_OUT"]),
        "{connected}"
    );
    assert!(
        connected["labels"].as_array().unwrap().iter().any(|label| {
            label["net"] == "TOLERANT_OUT" && label["x"] == 165.1 && label["y"] == 88.901
        }),
        "{connected}"
    );
}
