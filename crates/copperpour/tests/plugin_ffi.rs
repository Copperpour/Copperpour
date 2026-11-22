//! Regression tests for the embedded plugin FFI entry point (`kicad_plugin_init`).
//!
//! Each test runs `copperpour_plugin::kicad_plugin_init` inside an isolated
//! child process (re-invoking this test binary on `plugin_ffi_child_driver`) so
//! process-global `RUNTIME`, `HOME`, `XDG_CONFIG_HOME`, working directory, and
//! stdio pipes never leak across cases.

use serde_json::{json, Value};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "COPPERPOUR_PLUGIN_FFI_CHILD";
const RESULT_FILE_ENV: &str = "COPPERPOUR_PLUGIN_FFI_RESULT";
const STOP_FILE_ENV: &str = "COPPERPOUR_PLUGIN_FFI_STOP";
const PATH_MODE_ENV: &str = "COPPERPOUR_PLUGIN_FFI_PATH_MODE";
const PATH_VALUE_ENV: &str = "COPPERPOUR_PLUGIN_FFI_PATH_VALUE";
const SECOND_PATH_ENV: &str = "COPPERPOUR_PLUGIN_FFI_SECOND_PATH";

fn spawn_child(
    cwd: &Path,
    home: &Path,
    path_mode: &str,
    path_value: Option<&str>,
    second_path: Option<&str>,
) -> (std::process::Child, PathBuf, PathBuf) {
    let result_file = cwd.join(".ffi_result.txt");
    let stop_file = cwd.join(".ffi_stop");

    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", "plugin_ffi_child_driver", "--nocapture"])
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("KICAD_API_SOCKET")
        .env_remove("RUST_LOG")
        .env(CHILD_ENV, "1")
        .env(RESULT_FILE_ENV, &result_file)
        .env(STOP_FILE_ENV, &stop_file)
        .env(PATH_MODE_ENV, path_mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(value) = path_value {
        cmd.env(PATH_VALUE_ENV, value);
    } else {
        cmd.env_remove(PATH_VALUE_ENV);
    }
    if let Some(second) = second_path {
        cmd.env(SECOND_PATH_ENV, second);
    } else {
        cmd.env_remove(SECOND_PATH_ENV);
    }

    let child = cmd.spawn().expect("spawn plugin_ffi child");
    (child, result_file, stop_file)
}

fn initialize_request(id: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "plugin-ffi-test", "version": "0"}
        }
    })
    .to_string()
}

fn tools_call_request(id: u64, name: &str, arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    })
    .to_string()
}

fn read_jsonrpc_response(reader: &mut BufReader<std::process::ChildStdout>, id: u64) -> Value {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).expect("read child stdout");
        assert!(
            n > 0,
            "child stdout closed before JSON-RPC response id={id}"
        );
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            if val.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && val.get("id").and_then(Value::as_u64) == Some(id)
            {
                return val;
            }
        }
    }
}

fn parse_refusal_from_stderr(stderr: &str) -> Option<Value> {
    stderr.lines().find_map(|line| {
        let json_part = line.strip_prefix("kicad_plugin_init: ")?;
        serde_json::from_str::<Value>(json_part.trim()).ok()
    })
}

fn run_refusal_case(
    cwd: &Path,
    home: &Path,
    path_mode: &str,
    path_value: Option<&str>,
) -> (i32, String, String) {
    let (mut child, result_file, _stop_file) = spawn_child(cwd, home, path_mode, path_value, None);

    // Send an MCP initialize request to prove no server started on defaults.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{}", initialize_request(1));
        let _ = stdin.flush();
    }

    let output = child.wait_with_output().expect("wait for refusal child");
    assert!(
        output.status.success(),
        "child driver crashed: status={:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let rc: i32 = std::fs::read_to_string(&result_file)
        .expect("read ffi return code")
        .trim()
        .parse()
        .expect("parse ffi return code");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (rc, stdout, stderr)
}

fn assert_no_mcp_response(stdout: &str) {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            assert!(
                val.get("jsonrpc").is_none(),
                "refused startup must not start an MCP server or answer requests: {trimmed}"
            );
        }
    }
}

#[test]
fn plugin_ffi_child_driver() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let result_file = PathBuf::from(std::env::var_os(RESULT_FILE_ENV).expect("result file env"));
    let stop_file = PathBuf::from(std::env::var_os(STOP_FILE_ENV).expect("stop file env"));
    let path_mode = std::env::var(PATH_MODE_ENV).expect("path mode env");

    let rc = match path_mode.as_str() {
        "null" => unsafe { copperpour_plugin::kicad_plugin_init(std::ptr::null()) },
        "path" => {
            let value = std::env::var(PATH_VALUE_ENV).expect("path value env");
            let cstr = CString::new(value).expect("valid CString");
            unsafe { copperpour_plugin::kicad_plugin_init(cstr.as_ptr()) }
        }
        "invalid_utf8" => {
            let bytes: [u8; 3] = [0xff, 0xfe, 0x00];
            unsafe { copperpour_plugin::kicad_plugin_init(bytes.as_ptr().cast()) }
        }
        other => panic!("unknown path mode: {other}"),
    };

    if let Ok(second) = std::env::var(SECOND_PATH_ENV) {
        let first_refusal = copperpour_plugin::last_startup_refusal();
        assert!(
            first_refusal.is_some(),
            "refused init must populate last_startup_refusal()"
        );
        let cstr = CString::new(second).expect("valid second CString");
        let second_rc = unsafe { copperpour_plugin::kicad_plugin_init(cstr.as_ptr()) };
        if second_rc == 1 {
            assert!(
                copperpour_plugin::last_startup_refusal().is_none(),
                "successful init must clear last_startup_refusal()"
            );
        }
        let third_rc = unsafe { copperpour_plugin::kicad_plugin_init(cstr.as_ptr()) };
        std::fs::write(&result_file, format!("{rc},{second_rc},{third_rc}"))
            .expect("write result codes");
        if second_rc == 1 {
            wait_for_stop(&stop_file);
        }
        copperpour_plugin::kicad_plugin_shutdown();
        return;
    }

    if rc == -1 {
        let refusal = copperpour_plugin::last_startup_refusal()
            .expect("refused init must record last_startup_refusal()");
        assert_eq!(refusal.outcome, "failed");
        assert!(!refusal.started);
        assert!(!refusal.defaults_substituted);
    }

    std::fs::write(&result_file, rc.to_string()).expect("write return code");

    if rc == 1 {
        // Give the spawned server time to serve requests until the parent signals stop.
        wait_for_stop(&stop_file);
    } else {
        // Briefly pause so that if any background server had erroneously spawned,
        // it would have a chance to respond on stdout.
        std::thread::sleep(Duration::from_millis(150));
    }

    copperpour_plugin::kicad_plugin_shutdown();
}

fn wait_for_stop(stop_file: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !stop_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn explicit_missing_config_is_structured_refusal_without_starting_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let missing = tmp.path().join("does-not-exist.toml");

    let (rc, stdout, stderr) =
        run_refusal_case(tmp.path(), &home, "path", Some(missing.to_str().unwrap()));

    assert_eq!(
        rc, -1,
        "missing explicit config must refuse startup: stderr={stderr}"
    );
    assert_no_mcp_response(&stdout);
    let refusal = parse_refusal_from_stderr(&stderr)
        .unwrap_or_else(|| panic!("expected structured JSON refusal on stderr, got:\n{stderr}"));
    assert_eq!(refusal["outcome"], "failed");
    assert_eq!(refusal["started"], false);
    assert_eq!(refusal["defaults_substituted"], false);
    assert_eq!(refusal["origin"], "explicit_path");
    assert_eq!(refusal["error"]["kind"], "file_not_found");
    assert_eq!(refusal["error"]["path"], missing.display().to_string());
}

#[test]
fn explicit_malformed_json_config_is_structured_refusal_even_with_valid_fallback_in_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "{not valid json").unwrap();
    std::fs::write(tmp.path().join("settings.json"), r#"{"log_level":"debug"}"#).unwrap();

    let (rc, stdout, stderr) =
        run_refusal_case(tmp.path(), &home, "path", Some(bad.to_str().unwrap()));

    assert_eq!(
        rc, -1,
        "malformed explicit JSON must refuse startup: stderr={stderr}"
    );
    assert_no_mcp_response(&stdout);
    let refusal = parse_refusal_from_stderr(&stderr)
        .unwrap_or_else(|| panic!("expected structured JSON refusal on stderr, got:\n{stderr}"));
    assert_eq!(refusal["outcome"], "failed");
    assert_eq!(refusal["started"], false);
    assert_eq!(refusal["defaults_substituted"], false);
    assert_eq!(refusal["origin"], "explicit_path");
    assert_eq!(refusal["error"]["kind"], "invalid_configuration");
    assert!(
        refusal["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .starts_with("malformed_json:"),
        "{refusal}"
    );
}

#[test]
fn explicit_malformed_toml_config_is_structured_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bad = tmp.path().join("bad.toml");
    std::fs::write(&bad, "not = valid toml [[[").unwrap();

    let (rc, stdout, stderr) =
        run_refusal_case(tmp.path(), &home, "path", Some(bad.to_str().unwrap()));

    assert_eq!(
        rc, -1,
        "malformed explicit TOML must refuse startup: stderr={stderr}"
    );
    assert_no_mcp_response(&stdout);
    let refusal = parse_refusal_from_stderr(&stderr)
        .unwrap_or_else(|| panic!("expected structured JSON refusal on stderr, got:\n{stderr}"));
    assert_eq!(refusal["outcome"], "failed");
    assert_eq!(refusal["started"], false);
    assert_eq!(refusal["defaults_substituted"], false);
    assert_eq!(refusal["origin"], "explicit_path");
    assert_eq!(refusal["error"]["kind"], "invalid_configuration");
    assert!(
        refusal["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .starts_with("malformed_toml:"),
        "{refusal}"
    );
}

#[test]
fn explicit_empty_and_invalid_utf8_paths_are_structured_refusals() {
    for (mode, val, expected_reason) in [
        ("path", Some(""), "empty_path"),
        ("invalid_utf8", None, "invalid_utf8"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let (rc, stdout, stderr) = run_refusal_case(tmp.path(), &home, mode, val);

        assert_eq!(rc, -1, "mode={mode} must refuse startup: stderr={stderr}");
        assert_no_mcp_response(&stdout);
        let refusal = parse_refusal_from_stderr(&stderr).unwrap_or_else(|| {
            panic!("mode={mode}: expected structured JSON refusal on stderr, got:\n{stderr}")
        });
        assert_eq!(refusal["outcome"], "failed");
        assert_eq!(refusal["started"], false);
        assert_eq!(refusal["defaults_substituted"], false);
        assert_eq!(refusal["origin"], "explicit_path");
        assert_eq!(refusal["error"]["kind"], "invalid_argument");
        assert_eq!(refusal["error"]["reason"], expected_reason);
    }
}

#[test]
fn explicit_unreadable_directory_config_is_structured_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dir_path = tmp.path().join("not-a-file");
    std::fs::create_dir_all(&dir_path).unwrap();

    let (rc, stdout, stderr) =
        run_refusal_case(tmp.path(), &home, "path", Some(dir_path.to_str().unwrap()));

    assert_eq!(
        rc, -1,
        "unreadable directory config must refuse startup: stderr={stderr}"
    );
    assert_no_mcp_response(&stdout);
    let refusal = parse_refusal_from_stderr(&stderr)
        .unwrap_or_else(|| panic!("expected structured JSON refusal on stderr, got:\n{stderr}"));
    assert_eq!(refusal["outcome"], "failed");
    assert_eq!(refusal["started"], false);
    assert_eq!(refusal["defaults_substituted"], false);
    assert_eq!(refusal["origin"], "explicit_path");
    assert_eq!(refusal["error"]["kind"], "invalid_configuration");
    assert!(
        refusal["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .starts_with("unreadable:"),
        "{refusal}"
    );
}

#[test]
fn malformed_search_path_config_with_null_path_refuses_instead_of_falling_back() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(tmp.path().join("copperpour.toml"), "broken = [[[").unwrap();
    std::fs::write(tmp.path().join("settings.json"), "{}").unwrap();

    let (rc, stdout, stderr) = run_refusal_case(tmp.path(), &home, "null", None);

    assert_eq!(
        rc, -1,
        "malformed search-path config must refuse startup: stderr={stderr}"
    );
    assert_no_mcp_response(&stdout);
    let refusal = parse_refusal_from_stderr(&stderr)
        .unwrap_or_else(|| panic!("expected structured JSON refusal on stderr, got:\n{stderr}"));
    assert_eq!(refusal["outcome"], "failed");
    assert_eq!(refusal["started"], false);
    assert_eq!(refusal["defaults_substituted"], false);
    assert_eq!(refusal["origin"], "search_path");
    assert_eq!(refusal["error"]["kind"], "invalid_configuration");
}

#[test]
fn valid_explicit_config_starts_and_reports_explicit_provenance_over_served_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let cfg = tmp.path().join("explicit.toml");
    std::fs::write(&cfg, "transport = \"stdio\"\nlog_level = \"info\"\n").unwrap();

    let (mut child, result_file, stop_file) =
        spawn_child(tmp.path(), &home, "path", Some(cfg.to_str().unwrap()), None);

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    writeln!(stdin, "{}", initialize_request(1)).unwrap();
    stdin.flush().unwrap();
    let init_resp = read_jsonrpc_response(&mut stdout, 1);
    assert!(init_resp.get("result").is_some(), "{init_resp}");

    writeln!(
        stdin,
        "{}",
        tools_call_request(2, "get_installation_info", json!({}))
    )
    .unwrap();
    stdin.flush().unwrap();
    let info_resp = read_jsonrpc_response(&mut stdout, 2);
    let text = info_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("get_installation_info text");
    let body: Value = serde_json::from_str(text).expect("get_installation_info JSON");
    assert_eq!(body["configuration"]["source"], "explicit_path");
    assert_eq!(
        body["configuration"]["selected_path"],
        cfg.canonicalize().unwrap().display().to_string()
    );

    drop(stdin);
    std::fs::write(&stop_file, "stop").unwrap();
    let output = child.wait_with_output().expect("wait for valid child");
    assert!(output.status.success());
    let rc: i32 = std::fs::read_to_string(&result_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(rc, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("configuration: explicit_path from"),
        "expected startup config provenance log on stderr:\n{stderr}"
    );
}

#[test]
fn null_config_without_files_starts_with_defaults_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let (mut child, result_file, stop_file) = spawn_child(tmp.path(), &home, "null", None, None);

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    writeln!(stdin, "{}", initialize_request(1)).unwrap();
    stdin.flush().unwrap();
    let _ = read_jsonrpc_response(&mut stdout, 1);

    writeln!(
        stdin,
        "{}",
        tools_call_request(2, "get_installation_info", json!({}))
    )
    .unwrap();
    stdin.flush().unwrap();
    let info_resp = read_jsonrpc_response(&mut stdout, 2);
    let text = info_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("get_installation_info text");
    let body: Value = serde_json::from_str(text).expect("get_installation_info JSON");
    assert_eq!(body["configuration"]["source"], "defaults");
    assert!(body["configuration"]["selected_path"].is_null());

    drop(stdin);
    std::fs::write(&stop_file, "stop").unwrap();
    let output = child.wait_with_output().expect("wait for defaults child");
    assert!(output.status.success());
    let rc: i32 = std::fs::read_to_string(&result_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(rc, 1);
}

#[test]
fn refusal_leaves_plugin_uninitialized_so_a_subsequent_valid_init_succeeds_and_third_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let missing = tmp.path().join("missing.toml");
    let valid = tmp.path().join("valid.toml");
    std::fs::write(&valid, "transport = \"stdio\"\n").unwrap();

    let (mut child, result_file, stop_file) = spawn_child(
        tmp.path(),
        &home,
        "path",
        Some(missing.to_str().unwrap()),
        Some(valid.to_str().unwrap()),
    );

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    writeln!(stdin, "{}", initialize_request(1)).unwrap();
    stdin.flush().unwrap();
    let _ = read_jsonrpc_response(&mut stdout, 1);

    writeln!(
        stdin,
        "{}",
        tools_call_request(2, "get_installation_info", json!({}))
    )
    .unwrap();
    stdin.flush().unwrap();
    let info_resp = read_jsonrpc_response(&mut stdout, 2);
    let text = info_resp["result"]["content"][0]["text"].as_str().unwrap();
    let body: Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["configuration"]["source"], "explicit_path");
    assert_eq!(
        body["configuration"]["selected_path"],
        valid.canonicalize().unwrap().display().to_string()
    );

    drop(stdin);
    std::fs::write(&stop_file, "stop").unwrap();
    let output = child.wait_with_output().expect("wait for recovery child");
    assert!(output.status.success());
    let codes = std::fs::read_to_string(&result_file).unwrap();
    assert_eq!(
        codes.trim(),
        "-1,1,0",
        "expected refusal (-1), then valid start (1), then already-initialized (0)"
    );
    // The no-op third call must not load the config or start a second server.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("configuration: explicit_path from").count(),
        1,
        "{stderr}"
    );
}
