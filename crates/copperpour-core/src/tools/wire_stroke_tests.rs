//! A new wire is written with the stroke KiCad writes for it (#899), whichever
//! tool draws it.
use super::batch_power_tests::{handler, pins, serve};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SHEET: &[u8] =
    include_bytes!("../../tests/fixtures/power_orientation_kicad10/power_orientation.kicad_sch");
/// KiCad 10.0.8's resave of `SHEET` after `add_wire` from (152.4, 101.6) to
/// (165.1, 101.6); see `tests/fixtures/wire_stroke_kicad10.README.md`.
const RESAVED: &str = include_str!("../../tests/fixtures/wire_stroke_kicad10.kicad_sch");

const STROKE: &str = "\t\t(stroke\n\t\t\t(width 0)\n\t\t\t(type default)\n\t\t)\n";

fn sheet() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // The sheet's instance records name this project.
    let path = dir.path().join("power_orientation.kicad_sch");
    std::fs::write(&path, SHEET).unwrap();
    (dir, path)
}

/// Each top-level wire of `text` as (uuid, block), the block from `(wire` to
/// its closing parenthesis with the uuid line left out.
fn wires(text: &str) -> Vec<(String, String)> {
    let text = text.replace("\r\n", "\n");
    text.split("\n\t(wire\n")
        .skip(1)
        .map(|rest| {
            // The split took the newline after the previous wire's `\t)`.
            let block = &rest[..rest.find("\n\t)").expect("a wire closes")];
            let uuid_at = block.find("\t\t(uuid \"").expect("a wire has a uuid");
            let uuid = block[uuid_at + 9..].split('"').next().unwrap().to_owned();
            let without_uuid = block[..uuid_at].to_owned();
            (uuid, without_uuid)
        })
        .collect()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The wire `add_wire` writes is the wire KiCad writes when it resaves the
/// sheet: the same lines, stroke included, apart from its uuid.
#[tokio::test]
async fn a_new_wire_is_written_as_kicad_writes_it() {
    let handler = handler().await;
    let (_dir, path) = sheet();
    let reply = serve(
        &handler,
        "add_wire",
        &path,
        json!({"x1": 152.4, "y1": 101.6, "x2": 165.1, "y2": 101.6}),
    )
    .await;
    assert_eq!(reply["is_error"], false, "{reply}");
    let at = |text: &str| -> String {
        wires(text)
            .into_iter()
            .map(|(_, block)| block)
            .find(|block| block.contains("(xy 152.4 101.6) (xy 165.1 101.6)"))
            .expect("the new wire")
    };
    let kicad = at(RESAVED);
    assert!(kicad.contains(STROKE), "{kicad}");
    assert_eq!(at(&read(&path)), kicad);
}

/// Every tool that draws a wire through the schematic editor writes each new
/// wire with that stroke: `add_wire`, `batch_add_wire`, and the stubs
/// `add_power_symbol` and `batch_add_power_symbol` draw.
#[tokio::test]
async fn every_tool_that_draws_a_wire_writes_its_stroke() {
    let handler = handler().await;
    let (_dir, path) = sheet();
    let calls: [(&str, Value); 4] = [
        (
            "add_wire",
            json!({"x1": 152.4, "y1": 101.6, "x2": 165.1, "y2": 101.6}),
        ),
        (
            "batch_add_wire",
            json!({"wires": [
                {"x1": 152.4, "y1": 106.68, "x2": 165.1, "y2": 106.68},
                {"x1": 152.4, "y1": 111.76, "x2": 165.1, "y2": 111.76}
            ]}),
        ),
        (
            "add_power_symbol",
            json!({"power_net": "+3V3", "reference": "J6", "pin_number": "1"}),
        ),
        (
            "batch_add_power_symbol",
            json!({"power_net": "GND", "pins": pins(&["J6.3", "J6.6"])}),
        ),
    ];
    let mut seen: BTreeSet<String> = wires(&read(&path)).into_iter().map(|(u, _)| u).collect();
    for (tool, arguments) in calls {
        let reply = serve(&handler, tool, &path, arguments).await;
        assert_eq!(reply["is_error"], false, "{tool}: {reply}");
        let new: Vec<(String, String)> = wires(&read(&path))
            .into_iter()
            .filter(|(uuid, _)| !seen.contains(uuid))
            .collect();
        assert!(!new.is_empty(), "{tool} drew no wire: {reply}");
        for (uuid, block) in &new {
            assert!(block.contains(STROKE), "{tool} wrote {uuid} as\n{block}");
        }
        seen.extend(new.into_iter().map(|(uuid, _)| uuid));
    }
}
