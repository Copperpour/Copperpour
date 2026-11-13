//! Full MCP-tool regression against a running KiCad PCB Editor.
//!
//! The workflow opens a disposable board under Xvfb and supplies its socket.
//! This test intentionally crosses every layer: JSON-RPC stdio, tool routing,
//! platform footprint-library discovery, `.kicad_mod` preparation, and live IPC.

use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_sexp::{parse_sexp, SexpNode};
use prost::Message;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

/// `COPPERPOUR_<name>`, or else `KONNECT_<name>`, its name before the rename.
fn test_env(name: &str) -> Option<String> {
    copperpour_core::app_dirs::env_var(name).and_then(|value| value.into_string().ok())
}

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn spawn(socket: &str) -> Self {
        let config = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        let kicad_cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        std::fs::write(
            config.path(),
            serde_json::to_vec(&json!({"ipc_address": socket, "kicad_cli": kicad_cli})).unwrap(),
        )
        .unwrap();
        let (_, config_path) = config.keep().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_copperpour"))
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to start Copperpour MCP server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut process = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        process.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "live-kicad-tools", "version": "0"}
            }),
        );
        process
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "Copperpour exited before replying"
            );
            let response: Value = serde_json::from_str(line.trim()).unwrap();
            if response["id"] == id {
                return response;
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        assert_ne!(
            result["isError"], true,
            "tool {name} failed: {}",
            result["content"][0]["text"]
        );
        result.clone()
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn footprint<'a>(tree: &'a SexpNode, reference: &str) -> &'a SexpNode {
    tree.find_all("footprint")
        .into_iter()
        .find(|node| {
            node.find_all("property").into_iter().any(|property| {
                property.get(1).and_then(SexpNode::as_str) == Some("Reference")
                    && property.get(2).and_then(SexpNode::as_str) == Some(reference)
            })
        })
        .unwrap_or_else(|| panic!("placed footprint {reference} is missing from saved board"))
}

fn live_footprint(
    ipc: &KiCadIpcClient,
    board: &std::path::Path,
    reference: &str,
) -> kiapi::board::types::FootprintInstance {
    let document = ipc.find_open_board(board).unwrap();
    ipc.get_items_in(
        document,
        kiapi::common::types::KiCadObjectType::KotPcbFootprint,
    )
    .unwrap()
    .into_iter()
    .filter_map(|item| kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).ok())
    .find(|footprint| {
        footprint
            .reference_field
            .as_ref()
            .and_then(|field| field.text.as_ref())
            .and_then(|text| text.text.as_ref())
            .map(|text| text.text.as_str())
            == Some(reference)
    })
    .unwrap_or_else(|| panic!("live footprint {reference} is missing"))
}

fn footprint_pad_size(
    footprint: &kiapi::board::types::FootprintInstance,
    number: &str,
) -> (i64, i64) {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
        .filter_map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).ok())
        .find(|pad| pad.number == number)
        .and_then(|pad| pad.pad_stack)
        .and_then(|stack| stack.copper_layers.into_iter().next())
        .and_then(|layer| layer.size)
        .map(|size| (size.x_nm, size.y_nm))
        .unwrap_or_else(|| panic!("pad {number} is missing its copper size"))
}

fn footprint_models(
    footprint: &kiapi::board::types::FootprintInstance,
) -> Vec<kiapi::board::types::Footprint3DModel> {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| {
            item.type_url
                .ends_with("kiapi.board.types.Footprint3DModel")
        })
        .map(|item| kiapi::board::types::Footprint3DModel::decode(item.value.as_slice()).unwrap())
        .collect()
}

fn footprint_text_stroke_widths(footprint: &kiapi::board::types::FootprintInstance) -> Vec<i64> {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.BoardText"))
        .map(|item| kiapi::board::types::BoardText::decode(item.value.as_slice()).unwrap())
        .filter_map(|text| {
            text.text
                .and_then(|text| text.attributes)
                .and_then(|attributes| attributes.stroke_width)
                .map(|width| width.value_nm)
        })
        .collect()
}

#[test]
#[ignore = "requires a running KiCad GUI, API socket, and standard footprint libraries"]
fn place_component_loads_real_library_geometry() {
    let board = test_env("LIVE_KICAD_BOARD")
        .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board");
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let lib_id =
        test_env("LIVE_KICAD_FOOTPRINT").unwrap_or_else(|| "Resistor_SMD:R_0402_1005Metric".into());
    let reference = test_env("LIVE_KICAD_PLACE_REFERENCE").unwrap_or_else(|| "R900".into());

    let ipc = KiCadIpcClient::new(&socket);
    let ready = (0..100).any(|_| {
        if ipc.ping().unwrap_or(false) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
        false
    });
    assert!(ready, "KiCad IPC socket never became ready");

    let mut mcp = McpProcess::spawn(&socket);
    mcp.tool("load_toolset", json!({"name": "pcb_components"}));
    let placed = mcp.tool(
        "place_component",
        json!({
            "board": board,
            "footprint": lib_id,
            "reference": reference,
            "x": 42.0,
            "y": 37.0,
            "rotation": 90.0,
            "layer": "F.Cu"
        }),
    );
    let body: Value = serde_json::from_str(placed["content"][0]["text"].as_str().unwrap())
        .expect("place_component did not return JSON");
    assert_eq!(body["placed"], reference);
    assert_eq!(body["footprint"], lib_id);

    let wrong_board = std::path::Path::new(&board)
        .with_file_name("not-the-active-board.kicad_pcb")
        .to_string_lossy()
        .into_owned();
    let rejected = mcp.request(
        "tools/call",
        json!({
            "name": "move_component",
            "arguments": {
                "board": wrong_board,
                "reference": reference,
                "x": 99.0,
                "y": 99.0
            }
        }),
    );
    assert_eq!(
        rejected["result"]["isError"], true,
        "wrong-board mutation was not rejected: {rejected}"
    );

    let edited = mcp.tool(
        "edit_component",
        json!({"board": board, "reference": reference, "value": "10k"}),
    );
    let edited: Value = serde_json::from_str(edited["content"][0]["text"].as_str().unwrap())
        .expect("edit_component did not return JSON");
    assert_eq!(edited["value"], "10k");

    let array = mcp.tool(
        "place_component_array",
        json!({
            "board": board,
            "footprint": lib_id,
            "start_x": 30.0,
            "start_y": 50.0,
            "count_x": 2,
            "count_y": 1,
            "spacing_x": 5.0,
            "ref_prefix": "R",
            "ref_start": 910
        }),
    );
    let array: Value = serde_json::from_str(array["content"][0]["text"].as_str().unwrap())
        .expect("place_component_array did not return JSON");
    assert_eq!(array["placed_count"], 2);

    let aligned = mcp.tool(
        "align_components",
        json!({
            "board": board,
            "references": ["R910", "R911"],
            "axis": "y",
            "value": 55.0
        }),
    );
    let aligned: Value = serde_json::from_str(aligned["content"][0]["text"].as_str().unwrap())
        .expect("align_components did not return JSON");
    assert_eq!(aligned["aligned_count"], 2);

    KiCadIpcClient::new(&socket)
        .save_board()
        .expect("failed to save board after placement");
    let tree = parse_sexp(&std::fs::read_to_string(&board).unwrap()).unwrap();
    let placed = footprint(&tree, &reference);
    assert!(
        placed.find_all("pad").len() >= 2,
        "placed library footprint lost its pads"
    );
    assert!(placed.find_all("property").into_iter().any(|property| {
        property.get(1).and_then(SexpNode::as_str) == Some("Value")
            && property.get(2).and_then(SexpNode::as_str) == Some("10k")
    }));
    let at = placed
        .find("at")
        .expect("placed footprint has no board position");
    assert!((at.get_f64(1).unwrap() - 42.0).abs() < 1e-6);
    assert!((at.get_f64(2).unwrap() - 37.0).abs() < 1e-6);
    assert!((at.get_f64(3).unwrap() - 90.0).abs() < 1e-6);

    for array_reference in ["R910", "R911"] {
        let array_footprint = footprint(&tree, array_reference);
        let at = array_footprint
            .find("at")
            .expect("array footprint has no board position");
        assert!((at.get_f64(2).unwrap() - 55.0).abs() < 1e-6);
    }
}

/// Issue #462 through the public MCP boundary and a real KiCad PCB Editor.
///
/// This proves more than a successful tool response: the tool must select IPC,
/// KiCad must expose the new footprint through live readback, and a KiCad save
/// must persist the exact shipped library id and NPTH drill geometry.
#[test]
#[ignore = "requires a running KiCad GUI, API socket, disposable open board, and standard footprint libraries"]
fn add_mounting_hole_round_trips_through_live_kicad() {
    let board = std::path::PathBuf::from(
        test_env("LIVE_KICAD_BOARD")
            .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board"),
    );
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let reference = test_env("LIVE_KICAD_HOLE_REFERENCE").unwrap_or_else(|| "H900".to_string());
    let expected_lib_id = "MountingHole:MountingHole_3.2mm_M3";

    let ipc = KiCadIpcClient::new(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match ipc.find_open_board(&board) {
            Ok(_) => break,
            Err(error)
                if error.to_string().contains("AS_NOT_READY")
                    && std::time::Instant::now() < deadline => {}
            Err(error) => panic!("KiCad did not open the disposable board: {error:#}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let mut mcp = McpProcess::spawn(&socket);
    mcp.tool("load_toolset", json!({"name": "pcb_board"}));
    let result = mcp.tool(
        "add_mounting_hole",
        json!({
            "board": board,
            "x": 42.0,
            "y": 37.0,
            "drill_diameter": 3.2,
            "reference": reference
        }),
    );
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .expect("add_mounting_hole did not return JSON");
    assert_eq!(body["source"], "ipc", "{body}");
    assert_eq!(body["reference"], reference, "{body}");
    assert_eq!(body["footprint"], expected_lib_id, "{body}");

    let live = live_footprint(&ipc, &board, &reference);
    assert_eq!(
        live.definition
            .as_ref()
            .and_then(|definition| definition.id.as_ref())
            .map(|id| format!("{}:{}", id.library_nickname, id.entry_name)),
        Some(expected_lib_id.to_string()),
        "live KiCad readback did not preserve the shipped library identity"
    );

    ipc.save_board().expect("failed to save live board");
    let tree = parse_sexp(&std::fs::read_to_string(&board).unwrap()).unwrap();
    let saved = footprint(&tree, &reference);
    assert_eq!(
        saved.get(1).and_then(SexpNode::as_str),
        Some(expected_lib_id)
    );
    let pad = saved
        .find_all("pad")
        .into_iter()
        .next()
        .expect("saved mounting hole has no pad");
    assert_eq!(pad.get(2).and_then(SexpNode::as_str), Some("np_thru_hole"));
    assert!((pad.find("drill").unwrap().get_f64(1).unwrap() - 3.2).abs() < 1e-6);
}

#[test]
#[ignore = "requires a running KiCad GUI, API socket, saved schematic, and matching open board"]
fn schematic_sync_apply_then_dry_run_is_noop() {
    let board = test_env("LIVE_KICAD_BOARD")
        .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board");
    let schematic = test_env("LIVE_KICAD_SCHEMATIC")
        .expect("COPPERPOUR_LIVE_KICAD_SCHEMATIC must name the saved, closed root schematic");
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let mut mcp = McpProcess::spawn(&socket);
    mcp.tool("load_toolset", json!({"name": "sch_export"}));

    let dry_run = mcp.tool(
        "update_pcb_from_schematic",
        json!({"schematic": schematic, "board": board, "dry_run": true}),
    );
    let dry_run: Value =
        serde_json::from_str(dry_run["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        dry_run["status"], "ready",
        "fixture must require a sync: {dry_run}"
    );
    let revision = dry_run["plan_revision"]
        .as_str()
        .expect("dry run returned no plan revision");

    let applied = mcp.tool(
        "update_pcb_from_schematic",
        json!({
            "schematic": schematic,
            "board": board,
            "dry_run": false,
            "expected_plan_revision": revision
        }),
    );
    let applied: Value =
        serde_json::from_str(applied["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(applied["status"], "applied", "{applied}");

    let after = mcp.tool(
        "update_pcb_from_schematic",
        json!({"schematic": schematic, "board": board, "dry_run": true}),
    );
    let after: Value = serde_json::from_str(after["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(after["status"], "noop", "apply did not converge: {after}");
}

#[test]
#[ignore = "requires a running KiCad GUI and a disposable open board"]
fn footprint_library_update_apply_then_dry_run_is_noop() {
    let board = std::path::PathBuf::from(
        test_env("LIVE_KICAD_BOARD")
            .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board"),
    );
    let project = board.with_extension("kicad_pro");
    assert!(
        project.is_file(),
        "the disposable board needs a sibling .kicad_pro"
    );
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let project_dir = board.parent().unwrap();
    let library_dir = project_dir.join("konnect-update-fixture.pretty");
    let footprint_path = library_dir.join("RefreshFixture.kicad_mod");
    let reference = "REFRESH900";
    let library_id = "konnect-update-fixture:RefreshFixture";

    let ipc = KiCadIpcClient::new(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match ipc.find_open_board(&board) {
            Ok(_) => break,
            Err(error)
                if error.to_string().contains("AS_NOT_READY")
                    && std::time::Instant::now() < deadline => {}
            Err(error) => panic!("KiCad did not open the disposable board: {error:#}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let mut mcp = McpProcess::spawn(&socket);
    mcp.tool(
        "load_toolset",
        json!({"name": ["library", "pcb_components"]}),
    );
    mcp.tool(
        "create_footprint",
        json!({
            "output": footprint_path,
            "name": "RefreshFixture",
            "description": "revision A",
            "pads": [
                {
                    "number": "1",
                    "type": "smd",
                    "shape": "rect",
                    "x": -1.0,
                    "y": 0.0,
                    "width": 1.0,
                    "height": 1.0
                },
                {
                    "number": "2",
                    "type": "smd",
                    "shape": "rect",
                    "x": 1.0,
                    "y": 0.0,
                    "width": 1.0,
                    "height": 1.0
                }
            ],
            "body_width": 3.0,
            "body_height": 2.0,
            "model": {
                "path": "../models/revision-a.step",
                "offset": {"x": 0.0, "y": 0.0, "z": 0.0},
                "scale": {"x": 1.0, "y": 1.0, "z": 1.0},
                "rotate": {"x": 0.0, "y": 0.0, "z": 0.0}
            }
        }),
    );
    let mut footprint_source = std::fs::read_to_string(&footprint_path).unwrap();
    let root_end = footprint_source
        .rfind("\n)")
        .expect("created footprint must have a closing root");
    footprint_source.insert_str(
        root_end,
        r#"
  (fp_text user "${REFERENCE}" (at 0 1.5 0) (layer "F.Fab")
    (uuid "f96c2efe-5925-4f74-81d2-f89a56f57e13")
    (effects (font (size 0.8 0.8) (thickness 0.11))))"#,
    );
    std::fs::write(&footprint_path, footprint_source).unwrap();
    mcp.tool(
        "register_footprint_library",
        json!({
            "library_path": library_dir,
            "nickname": "konnect-update-fixture",
            "scope": "project",
            "project": project
        }),
    );
    mcp.tool(
        "place_component",
        json!({
            "board": board,
            "footprint": library_id,
            "reference": reference,
            "x": 42.0,
            "y": 37.0,
            "rotation": 37.0,
            "layer": "F.Cu"
        }),
    );

    let before = live_footprint(&ipc, &board, reference);
    let before_id = before.id.clone();
    let before_position = before.position;
    let before_orientation = before.orientation;
    let before_layer = before.layer;
    let before_pad_one = footprint_pad_size(&before, "1");
    let before_models = footprint_models(&before);

    mcp.tool(
        "edit_footprint_pad",
        json!({
            "footprint_path": footprint_path,
            "pad_number": "1",
            "width": 2.0,
            "height": 1.5
        }),
    );
    mcp.tool(
        "set_footprint_graphics",
        json!({
            "footprint_path": footprint_path,
            "selector": {"layer": "F.CrtYd"},
            "mode": "replace",
            "graphics": [{
                "type": "rect",
                "start": {"x": -2.5, "y": -1.5},
                "end": {"x": 2.5, "y": 1.5},
                "stroke_width_mm": 0.05,
                "fill": "none"
            }]
        }),
    );
    mcp.tool(
        "set_footprint_metadata",
        json!({
            "footprint_path": footprint_path,
            "description": "revision B",
            "tags": ["konnect", "refresh"]
        }),
    );
    mcp.tool(
        "set_footprint_models",
        json!({
            "footprint_path": footprint_path,
            "mode": "replace",
            "models": [{
                "path": "../models/revision-b.step",
                "offset": {"x": 1.0, "y": 2.0, "z": 3.0},
                "scale": {"x": 1.0, "y": 1.0, "z": 1.0},
                "rotate": {"x": 90.0, "y": 0.0, "z": 45.0}
            }]
        }),
    );

    let dry_run = mcp.tool(
        "update_footprints_from_library",
        json!({
            "board": board,
            "references": [reference],
            "dry_run": true
        }),
    );
    let dry_run: Value =
        serde_json::from_str(dry_run["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(dry_run["status"], "ready", "{dry_run}");
    assert_eq!(dry_run["coverage"]["selected"]["planned"], 1);
    assert_eq!(dry_run["coverage"]["changed"]["planned"], 1);
    let revision = dry_run["plan_revision"].as_str().unwrap();

    let applied = mcp.tool(
        "update_footprints_from_library",
        json!({
            "board": board,
            "references": [reference],
            "dry_run": false,
            "expected_plan_revision": revision
        }),
    );
    let applied: Value =
        serde_json::from_str(applied["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(applied["status"], "applied", "{applied}");

    let after = live_footprint(&ipc, &board, reference);
    assert_eq!(after.id, before_id);
    assert_eq!(after.position, before_position);
    assert_eq!(after.orientation, before_orientation);
    assert_eq!(after.layer, before_layer);
    assert_ne!(footprint_pad_size(&after, "1"), before_pad_one);
    assert_eq!(footprint_pad_size(&after, "1"), (2_000_000, 1_500_000));
    assert_ne!(footprint_models(&after), before_models);
    assert_eq!(
        footprint_models(&after)[0].filename,
        "../models/revision-b.step"
    );
    assert!(
        footprint_text_stroke_widths(&after).contains(&110_000),
        "explicit fab text thickness was not preserved"
    );

    let converged = mcp.tool(
        "update_footprints_from_library",
        json!({
            "board": board,
            "references": [reference],
            "dry_run": true
        }),
    );
    let converged: Value =
        serde_json::from_str(converged["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(converged["status"], "noop", "{converged}");

    ipc.save_board().unwrap();
    let saved_converged = mcp.tool(
        "update_footprints_from_library",
        json!({
            "board": board,
            "references": [reference],
            "dry_run": true
        }),
    );
    let saved_converged: Value =
        serde_json::from_str(saved_converged["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        saved_converged["status"], "noop",
        "KiCad save changed the rebuilt fab text: {saved_converged}"
    );
    mcp.tool("load_toolset", json!({"name": "verification"}));
    let drc = mcp.tool(
        "run_drc",
        json!({
            "board": board,
            "severity": "info",
            "tests": ["lib_footprint_mismatch"],
            "limit": 100
        }),
    );
    let drc: Value = serde_json::from_str(drc["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        drc["violations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|violation| {
                !violation["description"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("doesn't match the copy in the library")
            }),
        "{drc}"
    );
}

/// What a real KiCad puts in `DocumentSpecifier`, recorded rather than assumed.
///
/// The ambiguity gate in `find_open_board` refuses whenever an open PCB
/// document cannot be placed on disk, and the shapes it refuses were derived
/// from the vendored proto's own contract — `board_filename` is documented as
/// "a PCB with a given filename, e.g. `board.kicad_pcb`", with
/// `ProjectSpecifier.path` supplying the directory. A gate built on that
/// reading is only as good as the reading, so this prints every field of every
/// open document and then asserts the property the gate depends on: a live
/// KiCad's open-document list is one Copperpour can resolve in full.
///
/// Run it against a KiCad holding the disposable board:
///
/// ```text
/// KICAD_API_SOCKET=… COPPERPOUR_LIVE_KICAD_BOARD=… \
///   cargo test -p copperpour --test live_kicad_tools -- --ignored --nocapture \
///   real_kicad_open_documents_resolve_to_comparable_paths
/// ```
#[test]
#[ignore = "requires a running KiCad GUI with a board open and its API socket"]
fn real_kicad_open_documents_resolve_to_comparable_paths() {
    let board = std::path::PathBuf::from(
        test_env("LIVE_KICAD_BOARD")
            .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board"),
    );
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let ipc = KiCadIpcClient::new(&socket);

    let documents = ipc.get_open_documents().expect("KiCad answered");
    assert!(
        !documents.is_empty(),
        "open the disposable board in KiCad before running this"
    );
    for document in &documents {
        println!(
            "open PCB document: type={} identifier={:?} project={:?}",
            document.r#type, document.identifier, document.project
        );
    }

    // The property the gate rests on: against a real KiCad, the requested
    // board is positively identified rather than refused as ambiguous.
    ipc.find_open_board(&board)
        .unwrap_or_else(|error| panic!("KiCad's open-document list was not resolvable: {error:#}"));

    // And a board that is genuinely not open is reported as such, not as an
    // ambiguity — the distinction that decides whether a file write may run.
    let absent = board.with_file_name("konnect-not-open-probe.kicad_pcb");
    let error = ipc
        .find_open_board(&absent)
        .expect_err("that board is not open");
    assert!(
        matches!(
            copperpour_ipc::IpcFailure::from_error(error),
            copperpour_ipc::IpcFailure::Target {
                error: copperpour_ipc::BoardTargetError::WrongDocument { .. },
                ..
            }
        ),
        "a complete open-document list must prove absence, not merely fail to confirm it"
    );
}

/// `get_board_extents` against a real KiCad (#688): the live answer comes from
/// KiCad, not silently from the file, and on an unedited board it agrees with
/// the saved file measured the way KiCad measures it.
///
/// The saved read cannot measure text or dimensions, so its box lies inside the
/// live one, and equals it when nothing went unmeasured.
#[test]
#[ignore = "requires a running KiCad GUI with an unedited board open and its API socket"]
fn board_extents_live_and_saved_agree_on_an_unedited_board() {
    let board = std::path::PathBuf::from(
        test_env("LIVE_KICAD_BOARD").expect("COPPERPOUR_LIVE_KICAD_BOARD must name the open board"),
    );
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let ipc = KiCadIpcClient::new(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match ipc.find_open_board(&board) {
            Ok(_) => break,
            Err(error)
                if error.to_string().contains("AS_NOT_READY")
                    && std::time::Instant::now() < deadline => {}
            Err(error) => panic!("KiCad did not open the board: {error:#}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let mut mcp = McpProcess::spawn(&socket);
    mcp.tool("load_toolset", json!({"name": "pcb_board"}));
    let mut extents = |source: &str| -> Value {
        let result = mcp.tool(
            "get_board_extents",
            json!({"board": board, "board_source": source}),
        );
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
            .expect("get_board_extents did not return JSON")
    };
    let live = extents("live");
    let saved = extents("saved");
    println!("live:  {live}\nsaved: {saved}");

    assert_eq!(live["sources"]["bounds"], "ipc", "{live}");
    assert_eq!(saved["sources"]["bounds"], "saved_board", "{saved}");
    assert!(
        live["item_count"].as_u64().unwrap() > 0,
        "KiCad measured nothing: {live}"
    );
    assert_eq!(saved["skipped_item_count"], 0, "{saved}");
    // KiCad 10 will not list tables or generators. The live answer names both
    // with KiCad's own refusal instead of reading as complete (#688).
    let unavailable = live["unavailable_item_classes"]
        .as_array()
        .expect("the live answer names what KiCad would not list");
    for class in ["tables", "generators"] {
        let named = unavailable
            .iter()
            .find(|entry| entry["class"] == class)
            .unwrap_or_else(|| panic!("{class} is not named: {live}"));
        assert_eq!(named["reason"], "refused", "{live}");
        assert_eq!(named["kiapi_status"], "AS_BAD_REQUEST", "{live}");
    }
    assert_eq!(saved["unavailable_item_classes"], json!([]), "{saved}");

    let coordinate = |body: &Value, key: &str| body[key].as_f64().unwrap();
    let tolerance = 1e-4;
    assert!(coordinate(&saved, "x_min") >= coordinate(&live, "x_min") - tolerance);
    assert!(coordinate(&saved, "y_min") >= coordinate(&live, "y_min") - tolerance);
    assert!(coordinate(&saved, "x_max") <= coordinate(&live, "x_max") + tolerance);
    assert!(coordinate(&saved, "y_max") <= coordinate(&live, "y_max") + tolerance);
    if saved["unmeasured_item_counts"] == json!({}) {
        for key in ["x_min", "y_min", "x_max", "y_max"] {
            let off = (coordinate(&saved, key) - coordinate(&live, key)).abs();
            assert!(
                off <= tolerance,
                "{key} differs by {off} mm\n{live}\n{saved}"
            );
        }
    }
}

fn tool_body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .expect("the tool did not return JSON")
}

/// Every pad of a live footprint as (number, position), in KiCad's order.
fn footprint_pads(
    footprint: &kiapi::board::types::FootprintInstance,
) -> Vec<(String, Option<(i64, i64)>)> {
    footprint
        .definition
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
        .map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap())
        .map(|pad| (pad.number, pad.position.map(|at| (at.x_nm, at.y_nm))))
        .collect()
}

/// Whether a saved text's `(effects …)` carries `(justify … mirror …)`.
fn saved_mirrored(text: &SexpNode) -> bool {
    text.find("effects")
        .and_then(|effects| effects.find("justify"))
        .and_then(SexpNode::children)
        .is_some_and(|tokens| tokens.iter().any(|token| token.as_str() == Some("mirror")))
}

fn assert_mm(body: &Value, key: &str, expected: f64) {
    let actual = body[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} missing: {body}"));
    assert!(
        (actual - expected).abs() < 1e-6,
        "{key}: expected {expected}, got {actual}: {body}"
    );
}

/// The `pcb_text` tools against a real KiCad (#97): a placed footprint's
/// Reference and Value fields and one board text are edited in place. Each
/// edit must be proven by KiCad's own readback, asking again must send
/// nothing, the footprint's identity, pose and pads must not move, and a
/// KiCad save must write what the readback reported, including the raw
/// `${REVISION}` variable.
///
/// It places a resistor and adds a board text, so run it against a fresh
/// disposable board:
///
/// ```text
/// KICAD_API_SOCKET=… COPPERPOUR_LIVE_KICAD_BOARD=… \
///   cargo test -p copperpour --test live_kicad_tools -- --ignored --nocapture \
///   pcb_text_edits_round_trip_through_live_kicad
/// ```
#[test]
#[ignore = "requires a running KiCad GUI, API socket, disposable open board, and standard footprint libraries"]
fn pcb_text_edits_round_trip_through_live_kicad() {
    let board = std::path::PathBuf::from(
        test_env("LIVE_KICAD_BOARD")
            .expect("COPPERPOUR_LIVE_KICAD_BOARD must name the disposable open board"),
    );
    let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET is required");
    let reference = test_env("LIVE_KICAD_TEXT_REFERENCE").unwrap_or_else(|| "R910".to_string());
    let label = "Rev ${REVISION} pcb_text";

    let ipc = KiCadIpcClient::new(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match ipc.find_open_board(&board) {
            Ok(_) => break,
            Err(error)
                if error.to_string().contains("AS_NOT_READY")
                    && std::time::Instant::now() < deadline => {}
            Err(error) => panic!("KiCad did not open the disposable board: {error:#}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let mut mcp = McpProcess::spawn(&socket);
    for toolset in ["pcb_components", "pcb_board", "pcb_text"] {
        mcp.tool("load_toolset", json!({"name": toolset}));
    }
    let placed = tool_body(&mcp.tool(
        "place_component",
        json!({
            "board": board,
            "footprint": "Resistor_SMD:R_0402_1005Metric",
            "reference": reference,
            "x": 60.0,
            "y": 40.0,
            "rotation": 0.0,
            "layer": "F.Cu"
        }),
    ));
    assert_eq!(placed["placed"], reference, "{placed}");
    let added = tool_body(&mcp.tool(
        "add_board_text",
        json!({"board": board, "text": label, "x": 60.0, "y": 50.0, "layer": "B.SilkS", "size": 1.5}),
    ));
    assert_eq!(added["source"], "ipc", "{added}");
    // The saved-file variable check needs the text on disk first.
    ipc.save_board().expect("failed to save live board");
    let pristine = live_footprint(&ipc, &board, &reference);

    // Reference field: move, rotate, resize, restyle, and flip to the back.
    let edit = json!({
        "board": board, "reference": reference, "field": "reference",
        "x": 61.5, "y": 38.0, "rotation": 90.0, "size_x": 0.8, "size_y": 1.2,
        "bold": true, "layer": "B.SilkS", "mirror": "auto"
    });
    let body = tool_body(&mcp.tool("edit_footprint_field", edit.clone()));
    println!("edit_footprint_field reference: {body}");
    assert_eq!(body["status"], "applied", "{body}");
    let after = &body["after"];
    assert_mm(after, "x_mm", 61.5);
    assert_mm(after, "y_mm", 38.0);
    assert_mm(after, "angle_degrees", 90.0);
    assert_mm(after, "size_x_mm", 0.8);
    assert_mm(after, "size_y_mm", 1.2);
    assert_eq!(after["layer"], "B.SilkS", "{body}");
    assert_eq!(after["mirrored"], true, "{body}");
    assert_eq!(after["bold"], true, "{body}");
    assert_eq!(after["text"], reference, "{body}");
    assert_eq!(body["mirror"]["side"], "back", "{body}");

    let again = tool_body(&mcp.tool("edit_footprint_field", edit));
    assert_eq!(
        again["status"], "unchanged",
        "asking again must send nothing: {again}"
    );

    // Value field: flip its visibility. Whether place_component leaves it
    // shown is its own business (#157), so read it first.
    let fields = tool_body(&mcp.tool(
        "list_footprint_fields",
        json!({"board": board, "reference": reference}),
    ));
    let value_shown = fields["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == "value")
        .unwrap_or_else(|| panic!("no value field listed: {fields}"))["visible"]
        .as_bool()
        .unwrap();
    let body = tool_body(&mcp.tool(
        "edit_footprint_field",
        json!({"board": board, "reference": reference, "field": "value", "visible": !value_shown}),
    ));
    println!("edit_footprint_field value: {body}");
    assert_eq!(body["status"], "applied", "{body}");
    assert_eq!(body["before"]["visible"], value_shown, "{body}");
    assert_eq!(body["after"]["visible"], !value_shown, "{body}");

    // The footprint itself did not move.
    let live = live_footprint(&ipc, &board, &reference);
    assert_eq!(live.id, pristine.id, "the footprint's KIID changed");
    assert_eq!(live.position, pristine.position);
    assert_eq!(live.orientation, pristine.orientation);
    assert_eq!(live.layer, pristine.layer);
    assert_eq!(footprint_pads(&live), footprint_pads(&pristine));

    // Board text: mirror it where it is, then move it to the front.
    let listing = tool_body(&mcp.tool("list_board_texts", json!({"board": board})));
    let uuid = listing["texts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|text| text["text"] == label)
        .unwrap_or_else(|| panic!("the added text is not listed raw: {listing}"))["uuid"]
        .as_str()
        .unwrap()
        .to_string();

    let body = tool_body(&mcp.tool(
        "edit_board_text",
        json!({"board": board, "uuid": uuid, "x": 62.0, "y": 52.0, "bold": true, "mirror": "auto"}),
    ));
    println!("edit_board_text back: {body}");
    assert_eq!(body["status"], "applied", "{body}");
    assert_eq!(body["text_check"], "matches_saved_file", "{body}");
    assert_eq!(body["after"]["text"], label, "{body}");
    assert_eq!(body["after"]["mirrored"], true, "{body}");
    assert_eq!(body["after"]["bold"], true, "{body}");
    assert_mm(&body["after"], "x_mm", 62.0);
    assert_mm(&body["after"], "y_mm", 52.0);

    let body = tool_body(&mcp.tool(
        "edit_board_text",
        json!({"board": board, "uuid": uuid, "layer": "F.SilkS", "mirror": "auto"}),
    ));
    println!("edit_board_text front: {body}");
    assert_eq!(body["status"], "applied", "{body}");
    assert_eq!(body["after"]["layer"], "F.SilkS", "{body}");
    assert_eq!(body["after"]["mirrored"], false, "{body}");
    assert_eq!(body["after"]["text"], label, "{body}");

    // A KiCad save writes what the readback reported.
    ipc.save_board().expect("failed to save live board");
    let tree = parse_sexp(&std::fs::read_to_string(&board).unwrap()).unwrap();
    let saved = footprint(&tree, &reference);
    let at = saved.find("at").expect("saved footprint has no position");
    assert_eq!(
        (at.get_f64(1), at.get_f64(2)),
        (Some(60.0), Some(40.0)),
        "the footprint moved in the saved file"
    );
    let property = |name: &str| {
        saved
            .find_all("property")
            .into_iter()
            .find(|property| property.get(1).and_then(SexpNode::as_str) == Some(name))
            .unwrap_or_else(|| panic!("saved footprint has no {name} property"))
    };
    let saved_reference = property("Reference");
    assert_eq!(saved_reference.find_str("layer"), Some("B.SilkS"));
    assert!(
        saved_mirrored(saved_reference),
        "Reference is not mirrored on disk"
    );
    assert_eq!(
        property("Value").find_str("hide") == Some("yes"),
        value_shown,
        "the Value field's visibility did not reach the file"
    );

    let saved_text = tree
        .find_all("gr_text")
        .into_iter()
        .find(|text| text.get(1).and_then(SexpNode::as_str) == Some(label))
        .expect("the saved board lost the raw ${REVISION} text");
    assert_eq!(saved_text.find_str("layer"), Some("F.SilkS"));
    assert!(
        !saved_mirrored(saved_text),
        "front text is still mirrored on disk"
    );
    let at = saved_text.find("at").expect("saved text has no position");
    assert_eq!((at.get_f64(1), at.get_f64(2)), (Some(62.0), Some(52.0)));
}
