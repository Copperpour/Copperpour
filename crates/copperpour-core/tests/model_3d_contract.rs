//! Served contract for #543; independent KiCad/OCCT checks are opt-in below.
use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}
async fn call(h: &McpHandler, name: &str, args: Value) -> Value {
    let response=h.handle_message(json!({"jsonrpc":"2.0","id":543,"method":"tools/call","params":{"name":name,"arguments":args}})).await.unwrap();
    let result = response.result.expect("JSON-RPC result");
    let mut body: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    body["is_error"] = json!(result["isError"] == true);
    body
}
fn args(path: &Path) -> Value {
    json!({"output":path,"name":"package","body":{"width_mm":4.,"length_mm":6.,"height_mm":2.,"standoff_mm":0.3,"color":[0.2,0.3,0.4]},
    "pin_rows":[
        {"axis":"x","count":3,"pitch_mm":1.5,"width_mm":0.4,"length_mm":0.8,"height_mm":0.3,"center_x_mm":0.,"center_y_mm":-3.4},
        {"axis":"x","count":3,"pitch_mm":1.5,"width_mm":0.4,"length_mm":0.8,"height_mm":0.3,"center_x_mm":0.,"center_y_mm":3.4}]})
}
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/model-3d")
        .join(name)
}

#[tokio::test]
async fn step_model_served_success_and_existing_destinations_are_preserved() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generated.step");
    let result = call(&h, "create_3d_model", args(&path)).await;
    assert_eq!(result["is_error"], false, "{result}");
    assert_eq!(result["solid_count"], 7);
    assert_eq!(result["body_source"]["mode"], "explicit");
    assert_eq!(result["attached"], false);
    assert_eq!(result["solids"][0]["min_mm"], json!([-2., -3., 0.3]));
    let original = std::fs::read(&path).unwrap();
    let integral_path = dir.path().join("integral.step");
    let mut integral_args = args(&integral_path);
    for row in integral_args["pin_rows"].as_array_mut().unwrap() {
        row["count"] = json!(3.0);
    }
    let integral_result = call(&h, "create_3d_model", integral_args).await;
    assert_eq!(integral_result["is_error"], false, "{integral_result}");
    assert_eq!(std::fs::read(integral_path).unwrap(), original);
    let minimum_path = dir.path().join("minimum.step");
    let mut minimum = args(&minimum_path);
    minimum["body"]["height_mm"] = json!(0.001);
    minimum["body"]["standoff_mm"] = json!(1.0);
    minimum["pin_rows"] = json!([{"axis":"x","count":1,"pitch_mm":0.001,"width_mm":0.001,"length_mm":0.001,"height_mm":0.001,"center_x_mm":1.,"center_y_mm":2.,"bottom_z_mm":3.}]);
    let result = call(&h, "create_3d_model", minimum).await;
    assert_eq!(result["is_error"], false, "{result}");
    assert_eq!(result["solid_count"], 2);
    let result = call(&h, "create_3d_model", args(&path)).await;
    assert_eq!(result["error"]["kind"], "conflict", "{result}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    #[cfg(unix)]
    {
        let link = dir.path().join("link.step");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let result = call(&h, "create_3d_model", args(&link)).await;
        assert_eq!(result["error"]["kind"], "conflict", "{result}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let destination = dir.path().join("directory.step");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep"), "sentinel").unwrap();
    let result = call(&h, "create_3d_model", args(&destination)).await;
    assert_eq!(result["error"]["kind"], "conflict", "{result}");
    assert_eq!(
        std::fs::read_to_string(destination.join("keep")).unwrap(),
        "sentinel"
    );
}

#[tokio::test]
async fn step_model_served_rejects_malformed_inputs_and_call_tool_writes() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("never.step");
    let mut cases = Vec::new();
    for (field, value) in [
        ("height_mm", json!(null)),
        ("width_mm", json!("4")),
        ("standoff_mm", json!(-1)),
        ("color", json!([0.2, 0.3])),
        ("typo", json!(4)),
    ] {
        let mut a = args(&path);
        a["body"][field] = value;
        cases.push(a);
    }
    let mut a = args(&path);
    a["pin_rows"][0]["count"] = json!(-1);
    cases.push(a);
    let mut a = args(&path);
    a["pin_rows"][0]["pitch_mm"] = json!(10000);
    cases.push(a);
    let mut a = args(&path);
    a["body"]["footprint_path"] = json!(fixture("outlines.pretty/rectangle-fab.kicad_mod"));
    cases.push(a);
    let mut a = args(&path);
    a["name"] = json!("bad\\escape");
    cases.push(a);
    for args in cases {
        let result = call(&h, "create_3d_model", args).await;
        assert_eq!(result["is_error"], true, "{result}");
        assert_eq!(result["error"]["kind"], "invalid_argument", "{result}");
        assert!(!path.exists());
    }
    let result = call(
        &h,
        "call_tool",
        json!({"name":"create_3d_model","arguments":args(&path)}),
    )
    .await;
    assert_eq!(result["error"]["kind"], "tool_not_permitted", "{result}");
    assert!(!path.exists());
}

#[tokio::test]
async fn step_model_saved_footprint_source_is_reported_and_never_modified() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    for (file, layer, min, max) in [
        (
            "rectangle-fab.kicad_mod",
            "F.Fab",
            [-1., -0.625, 0.],
            [1., 0.625, 2.],
        ),
        (
            "rectangle-courtyard.kicad_mod",
            "F.CrtYd",
            [1., -8., 0.],
            [5., -2., 2.],
        ),
    ] {
        let source = fixture(&format!("outlines.pretty/{file}"));
        let before = std::fs::read(&source).unwrap();
        let output = dir.path().join(format!("{file}.step"));
        let result = call(
            &h,
            "create_3d_model",
            json!({"output":output,"body":{"footprint_path":source,"height_mm":2.}}),
        )
        .await;
        assert_eq!(result["is_error"], false, "{result}");
        assert_eq!(result["body_source"]["layer"], layer);
        assert_eq!(result["solids"][0]["min_mm"], json!(min));
        assert_eq!(result["solids"][0]["max_mm"], json!(max));
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
    let output = dir.path().join("unsupported.step");
    let result=call(&h,"create_3d_model",json!({"output":output,"body":{"footprint_path":fixture("outlines.pretty/unsupported-fab.kicad_mod"),"height_mm":2.}})).await;
    assert_eq!(result["error"]["kind"], "invalid_argument", "{result}");
    assert!(!output.exists());
}

#[tokio::test]
#[ignore = "requires kicad-cli and Python cadquery-ocp; see docs/3d-model-generation.md"]
async fn step_model_kicad_assembly_and_independent_occt_oracle() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("generated.step");
    let result = call(&h, "create_3d_model", args(&model)).await;
    assert_eq!(result["is_error"], false, "{result}");
    let board = dir.path().join("assembly.kicad_pcb");
    std::fs::copy(fixture("assembly.kicad_pcb"), &board).unwrap();
    let before = std::fs::read(&board).unwrap();
    let assembly = dir.path().join("assembly.step");
    let exported = Command::new("kicad-cli")
        .args(["pcb", "export", "step", "--force", "-o"])
        .arg(&assembly)
        .arg(&board)
        .output()
        .expect("kicad-cli");
    assert!(
        exported.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&exported.stdout),
        String::from_utf8_lossy(&exported.stderr)
    );
    assert_eq!(std::fs::read(&board).unwrap(), before);
    let oracle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check_step_model.py");
    let checked = Command::new("python3")
        .arg(oracle)
        .arg(&model)
        .arg(&assembly)
        .output()
        .expect("Python OCCT oracle");
    assert!(
        checked.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    println!("{}", String::from_utf8_lossy(&checked.stdout));
}
