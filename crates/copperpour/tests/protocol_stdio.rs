//! MCP protocol tests over stdio — spawn the real binary and speak JSON-RPC.
//!
//! Codifies the smoke tests that were run by hand at release time: handshake,
//! toolset loading for the entire registry, a real file-based tool call, and
//! the structured-error taxonomy the LLM relies on for recovery.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[cfg(unix)]
#[path = "../../test-support/write_executable_script.rs"]
mod write_executable_script;
#[cfg(unix)]
use write_executable_script::write_executable_script;

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
    /// The handshake's `initialize` response.
    initialized: Value,
    _tempdir: Option<tempfile::TempDir>,
}

impl McpProcess {
    fn spawn() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Self::spawn_with_env(Some(tmp.path()), true, &[]);
        p._tempdir = Some(tmp);
        p
    }

    /// Spawn with the process working directory set to `dir`, so
    /// the config search's first path (`konnect.toml` in cwd) picks up
    /// a test config file placed there.
    fn spawn_in_dir(dir: Option<&std::path::Path>) -> Self {
        Self::spawn_configured(dir, false)
    }

    /// Spawn with the working directory set to `dir` and, when `isolate_home`,
    /// the platform config directory redirected there too.
    ///
    /// `dirs_config_path()` is the last search candidate and is built from
    /// `HOME` (`APPDATA` on Windows), so on a developer machine that already has
    /// `~/Library/Application Support/konnect/config.toml` a "no config exists"
    /// test silently resolves to that real file instead. Redirecting HOME makes
    /// the candidate list depend only on the temp dir.
    fn spawn_configured(dir: Option<&std::path::Path>, isolate_home: bool) -> Self {
        Self::spawn_with_env(dir, isolate_home, &[])
    }

    /// As `spawn_configured`, with extra environment variables for the child.
    /// Set on the child rather than this process because `std::env::set_var` is
    /// process-wide and the unit tests run in parallel — doing it in-process
    /// raced the #39 env-fallback test.
    fn spawn_with_env(
        dir: Option<&std::path::Path>,
        isolate_home: bool,
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn_with_env_without(dir, isolate_home, env, &[])
    }

    fn spawn_with_env_without(
        dir: Option<&std::path::Path>,
        isolate_home: bool,
        env: &[(&str, &str)],
        removed_env: &[&str],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_copperpour"));
        for (key, value) in env {
            command.env(key, value);
        }
        for name in removed_env {
            command.env_remove(name);
        }
        if let Some(dir) = dir {
            command.current_dir(dir);
            if isolate_home {
                command.env("HOME", dir);
                command.env("APPDATA", dir);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn copperpour binary");
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut p = McpProcess {
            child,
            stdin,
            reader,
            next_id: 1,
            initialized: Value::Null,
            _tempdir: None,
        };
        // MCP handshake
        let init = p.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "protocol-test", "version": "0"}
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "copperpour");
        p.initialized = init;
        p.notify("notifications/initialized");
        p
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{}", msg).unwrap();
        self.stdin.flush().unwrap();
        // Read lines until the response with our id arrives (skips any
        // notifications the server might emit).
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).unwrap();
            assert!(
                n > 0,
                "server closed stdout waiting for response to {method}"
            );
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{}", msg).unwrap();
        self.stdin.flush().unwrap();
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Value {
        let resp = self.request("tools/call", json!({"name": name, "arguments": args}));
        resp["result"].clone()
    }

    /// Send a `tools/call`, then a fencing `ping`, and return every line the
    /// server emits up to and including the ping response. The fence
    /// guarantees the read loop terminates even when the tool call emits no
    /// notification (as in bug #19), so a test can assert on side-effect
    /// notifications without risking a hang.
    fn call_tool_then_fence(&mut self, name: &str, args: Value) -> Vec<Value> {
        let call_id = self.next_id;
        self.next_id += 1;
        let call = json!({
            "jsonrpc": "2.0", "id": call_id, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        });
        writeln!(self.stdin, "{}", call).unwrap();
        let fence_id = self.next_id;
        self.next_id += 1;
        let fence = json!({"jsonrpc": "2.0", "id": fence_id, "method": "ping", "params": {}});
        writeln!(self.stdin, "{}", fence).unwrap();
        self.stdin.flush().unwrap();

        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).unwrap();
            assert!(n > 0, "server closed stdout before fence response");
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            let is_fence = v.get("id").and_then(Value::as_i64) == Some(fence_id);
            lines.push(v);
            if is_fence {
                break;
            }
        }
        lines
    }

    /// Parse the JSON body of a tool result's first text content.
    fn tool_body(result: &Value) -> Value {
        let text = result["content"][0]["text"].as_str().unwrap_or("{}");
        serde_json::from_str(text).unwrap_or(Value::Null)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn handshake_baseline_and_full_registry_loads() {
    let mut p = McpProcess::spawn();

    // Baseline tools/list: starter kit + meta-tools only (small context).
    let list = p.request("tools/list", json!({}));
    let baseline = list["result"]["tools"].as_array().unwrap().len();
    assert!(
        (10..30).contains(&baseline),
        "baseline tools/list should be the small starter kit, got {baseline}"
    );

    // list_toolboxes reports the registry; every toolset must load.
    let boxes = McpProcess::tool_body(&p.call_tool("list_toolboxes", json!({})));
    let toolsets: Vec<String> = boxes["toolsets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        toolsets.len() >= 17,
        "expected 17+ toolsets, got {}",
        toolsets.len()
    );
    // No license-era fields may reappear.
    assert!(boxes.get("license_tier").is_none());
    assert!(boxes["toolsets"][0].get("tier").is_none());

    let mut total = 0u64;
    for name in &toolsets {
        let loaded = McpProcess::tool_body(&p.call_tool("load_toolset", json!({"name": name})));
        let added = loaded["tools_added"].as_u64().unwrap_or(0);
        assert!(added > 0, "toolset '{name}' loaded no tools");
        total += added;
    }
    assert_eq!(
        total,
        boxes["total_tools"].as_u64().unwrap(),
        "sum of loaded tools disagrees with list_toolboxes total"
    );
}

#[cfg(unix)]
#[test]
fn prerequisite_health_is_cached_until_explicit_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("kicad-cli");
    write_executable_script(&cli, "#!/bin/sh\nprintf '10.0.5\\n'\n");
    let missing_ipc = format!("ipc://{}", tmp.path().join("missing.sock").display());
    std::fs::write(
        tmp.path().join("konnect.toml"),
        format!(
            "kicad_cli = {}\nipc_address = {}\n",
            json!(cli),
            json!(missing_ipc)
        ),
    )
    .unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let installation_info = |p: &mut McpProcess, args| {
        McpProcess::tool_body(&p.call_tool("get_installation_info", args))
    };

    let first = installation_info(&mut p, json!({}));
    let startup = &first["prerequisite_health"];
    assert_eq!(startup["source"], "startup", "{first:#}");
    assert!(startup["checked_at_unix_ms"].is_number());
    assert_eq!(startup["kicad_cli"]["status"], "ready");
    assert_eq!(startup["kicad_cli"]["version"], "10.0.5");
    assert_eq!(startup["ipc"]["status"], "unreachable", "{first:#}");
    assert_eq!(startup["ipc"]["reason"], "no_listener", "{first:#}");
    assert_eq!(startup["ipc"]["source"], "config");
    assert_eq!(startup["ipc"]["endpoint"], missing_ipc);

    std::fs::remove_file(&cli).unwrap();
    assert_eq!(
        installation_info(&mut p, json!({}))["prerequisite_health"],
        *startup
    );
    let refreshed = installation_info(&mut p, json!({"refresh_prerequisite_health": true}));
    assert_eq!(refreshed["prerequisite_health"]["source"], "refresh");
    assert_eq!(
        refreshed["prerequisite_health"]["kicad_cli"]["reason"],
        "not_found"
    );
    assert_eq!(refreshed["kicad"]["probe_status"], "launch_failed");
    assert_eq!(
        installation_info(&mut p, json!({}))["prerequisite_health"],
        refreshed["prerequisite_health"]
    );
}

#[test]
fn prerequisite_health_respects_disabled_cli_and_requires_a_boolean_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("konnect.toml"),
        "kicad_cli = '  '\nipc_address = 'ipc://prerequisite-health-disabled.sock'\n",
    )
    .unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let info = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(
        info["prerequisite_health"]["kicad_cli"]["reason"],
        "disabled"
    );
    assert!(info["prerequisite_health"]["kicad_cli"]["executable_path"].is_null());
    assert!(info["prerequisite_health"]["kicad_cli"]["version"].is_null());

    let result = p.call_tool(
        "get_installation_info",
        json!({"refresh_prerequisite_health": "true"}),
    );
    assert_eq!(result["isError"], true);
    let error = McpProcess::tool_body(&result);
    assert_eq!(error["error"]["kind"], "invalid_argument");
    assert_eq!(error["error"]["field"], "refresh_prerequisite_health");
}

#[cfg(unix)]
#[test]
fn prerequisite_health_cli_timeout_leaves_initialization_and_other_tools_available() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("kicad-cli");
    write_executable_script(&cli, "#!/bin/sh\nexec sleep 30\n");
    std::fs::write(
        tmp.path().join("konnect.toml"),
        format!(
            "kicad_cli = {}\nipc_address = {}\n",
            json!(cli),
            json!(format!(
                "ipc://{}",
                tmp.path().join("missing.sock").display()
            )),
        ),
    )
    .unwrap();

    let start = Instant::now();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let tools = McpProcess::tool_body(&p.call_tool("list_toolboxes", json!({})));
    assert!(tools["toolsets"].is_array(), "{tools}");
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "initialization or toolbox discovery waited for the CLI timeout"
    );

    let info = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(
        info["prerequisite_health"]["kicad_cli"]["reason"], "timed_out",
        "{info:#}"
    );
}

#[cfg(unix)]
#[test]
fn prerequisite_health_distinguishes_old_and_unexecutable_cli() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("kicad-cli");
    write_executable_script(&cli, "#!/bin/sh\nprintf '9.0.8\\n'\n");
    std::fs::write(
        tmp.path().join("konnect.toml"),
        format!(
            "kicad_cli = {}\nipc_address = 'ipc://missing.sock'\n",
            json!(cli)
        ),
    )
    .unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let startup = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    let cli_health = &startup["prerequisite_health"]["kicad_cli"];
    assert_eq!(cli_health["reason"], "unsupported_version");
    assert_eq!(cli_health["version"], "9.0.8");

    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o600)).unwrap();
    let refreshed = McpProcess::tool_body(&p.call_tool(
        "get_installation_info",
        json!({"refresh_prerequisite_health": true}),
    ));
    assert_eq!(
        refreshed["prerequisite_health"]["kicad_cli"]["reason"],
        "unexecutable"
    );
}

// #527: diagnostics must probe the configured executable, even when it cannot
// launch, instead of substituting a different installed KiCad.
#[test]
fn installation_info_cli_keeps_missing_explicit_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing-kicad-cli");
    for configured in [
        missing.to_string_lossy().into_owned(),
        " ./missing-kicad-cli ".to_string(),
        tmp.path().to_string_lossy().into_owned(),
    ] {
        std::fs::write(
            tmp.path().join("konnect.toml"),
            format!("kicad_cli = {}\n", json!(configured)),
        )
        .unwrap();
        let mut p = McpProcess::spawn_in_dir(Some(tmp.path()));
        let result = p.call_tool("get_installation_info", json!({}));
        assert_ne!(result["isError"], true, "{result}");
        let body = McpProcess::tool_body(&result);
        assert_eq!(body["kicad"]["cli_path"], configured.trim());
        assert_eq!(body["kicad"]["probe_status"], "launch_failed");
        assert!(body["kicad"]["version"].is_null());
    }
}

#[test]
fn installation_info_cli_probes_the_explicit_executable() {
    let tmp = tempfile::tempdir().unwrap();
    // The server binary has a real --version command on every CI platform.
    let executable = env!("CARGO_BIN_EXE_copperpour");
    std::fs::write(
        tmp.path().join("konnect.toml"),
        format!("kicad_cli = {}\n", json!(executable)),
    )
    .unwrap();
    let mut p = McpProcess::spawn_in_dir(Some(tmp.path()));
    let result = p.call_tool("get_installation_info", json!({}));
    assert_ne!(result["isError"], true, "{result}");
    let body = McpProcess::tool_body(&result);
    assert_eq!(body["kicad"]["cli_path"], executable);
    assert_eq!(body["kicad"]["probe_status"], "ok");
    assert_eq!(
        body["kicad"]["version"],
        format!("copperpour {}", env!("CARGO_PKG_VERSION"))
    );
}

#[cfg(unix)]
#[test]
fn cli_failure_reports_signal_termination() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("fake-kicad-cli");
    write_executable_script(
        &cli,
        "#!/bin/sh\nprintf 'interrupted export\\n' >&2\nkill -TERM \"$$\"\n",
    );
    std::fs::write(
        tmp.path().join("copperpour.toml"),
        format!(
            "kicad_cli = {}\nkicad_cli_timeout_seconds = 5\nipc_address = {}\n",
            json!(cli),
            json!(format!(
                "ipc://{}",
                tmp.path().join("unused.sock").display()
            ))
        ),
    )
    .unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let loaded = p.call_tool("load_toolset", json!({"name": "sch_export"}));
    assert_ne!(loaded["isError"], true, "{loaded}");
    let result = p.call_tool(
        "generate_netlist",
        json!({
            "schematic": concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../copperpour-core/tests/fixtures/single_pin_nets.kicad_sch"
            ),
            "output": tmp.path().join("board.net")
        }),
    );
    assert_eq!(result["isError"], true, "{result}");
    let body = McpProcess::tool_body(&result);
    assert_eq!(body["error"]["kind"], "handler_error", "{body}");
    let reason = body["error"]["reason"].as_str().unwrap();
    assert!(reason.starts_with("kicad-cli was terminated"), "{reason}");
    assert!(reason.contains("15 (SIGTERM)"), "{reason}");
    assert!(
        reason.ends_with(":\nstderr:\ninterrupted export"),
        "{reason}"
    );
}

#[test]
fn installation_info_reports_the_serving_process_without_leaking_endpoint_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("konnect.toml"),
        "ipc_address = \"ipc://diagnostic-test.sock?token=secret#fragment\"\n",
    )
    .unwrap();
    let mut p = McpProcess::spawn_in_dir(Some(tmp.path()));

    let list = p.request("tools/list", json!({}));
    assert!(list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "get_installation_info"));

    let result = p.call_tool("get_installation_info", json!({}));
    assert_ne!(result["isError"], json!(true), "{result:#?}");
    let body = McpProcess::tool_body(&result);

    assert_eq!(body["build"]["version"], env!("CARGO_PKG_VERSION"));
    if let Some(commit) = body["build"]["commit"].as_str() {
        assert!((7..=64).contains(&commit.len()));
        assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(body["build"]["commit_source"].is_string());
    } else {
        assert!(body["build"]["commit_source"].is_null());
    }
    assert!(body["runtime"]["executable_path"].is_string());
    assert!(body["runtime"]["working_directory"].is_string());
    assert_eq!(body["installation"]["binary_on_disk"]["probe_status"], "ok");
    assert_eq!(
        body["installation"]["binary_on_disk"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        body["installation"]["binary_on_disk"]["newer_than_running"],
        false
    );
    assert_eq!(body["ipc"]["configured"], true);
    assert_eq!(
        body["ipc"]["endpoint"],
        "ipc://diagnostic-test.sock [query/fragment redacted]"
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("token=secret"), "{body:#?}");
    assert!(!serialized.contains("#fragment"), "{body:#?}");
    assert!(!body["restart_guidance"].as_array().unwrap().is_empty());
}

// These real Unix listeners never speak NNG. Eager automatic discovery
// would wait on them before replying, even for an unrelated MCP request.
#[cfg(target_os = "linux")]
#[test]
fn prerequisite_health_leaves_multiple_unverified_sockets_for_live_operations() {
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let socket_dir = tmp.path().join("kicad");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::write(tmp.path().join("konnect.toml"), "kicad_cli = ''\n").unwrap();
    let primary = socket_dir.join("api.sock");
    let alternate = socket_dir.join(format!("api-{}.sock", std::process::id()));
    let _primary = UnixListener::bind(&primary).unwrap();
    let _alternate = UnixListener::bind(&alternate).unwrap();

    let start = Instant::now();
    let mut p = McpProcess::spawn_with_env_without(
        Some(tmp.path()),
        true,
        &[("TMPDIR", tmp.path().to_str().unwrap())],
        &["KICAD_API_SOCKET"],
    );
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "IPC discovery delayed initialization"
    );

    let start = Instant::now();
    for args in [json!({}), json!({"refresh_prerequisite_health": true})] {
        let info = McpProcess::tool_body(&p.call_tool("get_installation_info", args));
        assert_eq!(info["ipc"]["source"], "ambiguous");
        let ipc = &info["prerequisite_health"]["ipc"];
        assert_eq!(ipc["reason"], "ambiguous_endpoint", "{info:#}");
        assert_eq!(ipc["communication_confirmed"], false);
        assert_eq!(
            ipc["candidates"],
            json!([
                format!("ipc://{}", primary.display()),
                format!("ipc://{}", alternate.display()),
            ])
        );
    }
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "diagnostics probed unverified candidates"
    );
}

/// #565 over real stdio: with nothing to dial at startup, the server looks
/// for KiCad's socket on every call, and `get_installation_info` reports the
/// socket it finds after startup, from the decision the IPC calls make.
/// Linux only: detection there wants a real socket in the temp directory,
/// and macOS also looks in the shared `/tmp/kicad`, where a running KiCad
/// would change the outcome.
#[cfg(target_os = "linux")]
#[test]
fn installation_info_reports_a_socket_found_after_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("tmp");
    std::fs::create_dir_all(temp_dir.join("kicad")).unwrap();
    let mut p = McpProcess::spawn_with_env_without(
        Some(tmp.path()),
        true,
        &[("TMPDIR", temp_dir.to_str().unwrap())],
        &["KICAD_API_SOCKET"],
    );

    let before = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(
        before["ipc"],
        json!({
            "configured": false,
            "source": "unresolved",
            "endpoint": null,
            "candidates": [],
        }),
        "{before:#}"
    );

    assert_eq!(
        before["prerequisite_health"]["ipc"]["reason"],
        "not_configured"
    );
    assert_eq!(
        before["prerequisite_health"]["ipc"]["communication_confirmed"],
        false
    );

    // KiCad starts after the server, and its socket appears.
    let socket = temp_dir.join("kicad").join("api.sock");
    let _kicad = std::os::unix::net::UnixListener::bind(&socket).unwrap();

    let after = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(
        after["ipc"],
        json!({
            "configured": false,
            "source": "detected_at_call_time",
            "endpoint": format!("ipc://{}", socket.display()),
            "candidates": [],
        }),
        "{after:#}"
    );

    assert_eq!(after["prerequisite_health"], before["prerequisite_health"]);
    let refreshed = McpProcess::tool_body(&p.call_tool(
        "get_installation_info",
        json!({"refresh_prerequisite_health": true}),
    ));
    let health = &refreshed["prerequisite_health"]["ipc"];
    assert_eq!(health["endpoint"], after["ipc"]["endpoint"]);
    assert_eq!(health["status"], "unreachable");
    assert_eq!(health["communication_confirmed"], false);
}

#[test]
fn file_based_tool_roundtrip_in_temp_project() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proto_demo");
    let mut p = McpProcess::spawn();

    let created = p.call_tool(
        "create_project",
        json!({"name": "proto_demo", "path": proj.to_string_lossy()}),
    );
    assert_ne!(
        created["isError"],
        json!(true),
        "create_project failed: {created}"
    );
    assert!(proj.join("proto_demo.kicad_sch").exists());

    let info = p.call_tool(
        "get_project_info",
        json!({"path": proj.join("proto_demo.kicad_pro").to_string_lossy()}),
    );
    assert_ne!(
        info["isError"],
        json!(true),
        "get_project_info failed: {info}"
    );
}

#[test]
fn structured_errors_guide_recovery() {
    let mut p = McpProcess::spawn();

    // Known tool in an unloaded toolset → toolset_not_loaded naming the owner.
    let r = p.call_tool("route_trace", json!({}));
    assert_eq!(r["isError"], json!(true));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "toolset_not_loaded");
    assert_eq!(body["error"]["toolset"], "pcb_routing");

    // Unknown tool → unknown_tool.
    let r = p.call_tool("frobnicate_board", json!({}));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "unknown_tool");

    // Missing required argument → invalid_argument naming the field.
    let r = p.call_tool("create_project", json!({"path": "/tmp/x"}));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "name");
}

#[test]
fn unknown_method_is_json_rpc_error_not_crash() {
    let mut p = McpProcess::spawn();
    let resp = p.request("tools/definitely_not_a_method", json!({}));
    assert!(
        resp.get("error").is_some(),
        "expected JSON-RPC error: {resp}"
    );
    // Server must still be alive afterwards.
    let ping = p.request("ping", json!({}));
    assert!(ping.get("result").is_some());
}

/// Regression test for issue #19. After `load_toolset`, the server must emit
/// `notifications/tools/list_changed` **over stdio** — not only over HTTP/SSE.
/// Without it, stdio clients (Claude Code) never re-fetch `tools/list`, so
/// every tool added by `load_toolset` stays uncallable for the session.
#[test]
fn load_toolset_emits_list_changed_over_stdio() {
    let mut p = McpProcess::spawn();
    let lines = p.call_tool_then_fence("load_toolset", json!({"name": "sch_components"}));
    let saw_notification = lines.iter().any(|v| {
        v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
            && v.get("id").is_none()
    });
    assert!(
        saw_notification,
        "expected notifications/tools/list_changed after load_toolset (issue #19); saw: {lines:#?}"
    );
}

/// The same guarantee for `unload_toolset` — removing tools must also tell the
/// client to refresh its tool list.
#[test]
fn unload_toolset_emits_list_changed_over_stdio() {
    let mut p = McpProcess::spawn();
    let _ = p.call_tool_then_fence("load_toolset", json!({"name": "sch_components"}));
    let lines = p.call_tool_then_fence("unload_toolset", json!({"name": "sch_components"}));
    let saw_notification = lines.iter().any(|v| {
        v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
            && v.get("id").is_none()
    });
    assert!(
        saw_notification,
        "expected notifications/tools/list_changed after unload_toolset; saw: {lines:#?}"
    );
}

/// `load_toolset` accepts an array of names in one call: all listed toolsets
/// load, tools_added sums across them, and only one list_changed notification
/// fires for the whole batch.
#[test]
fn load_toolset_batch_form_loads_all_and_notifies_once() {
    let mut p = McpProcess::spawn();
    let lines = p.call_tool_then_fence(
        "load_toolset",
        json!({"name": ["sch_components", "sch_wiring"]}),
    );
    let r = lines
        .iter()
        .find(|v| v.get("result").is_some())
        .expect("expected a tools/call result")["result"]
        .clone();
    let body = McpProcess::tool_body(&r);
    // sch_components (19) + sch_wiring (20).
    assert_eq!(body["tools_added"].as_u64(), Some(39));
    // tools items are {name, description} objects, matching the legacy
    // single-name result shape -- not bare name strings.
    let tools = body["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty());
    for t in tools {
        assert!(t.get("name").and_then(Value::as_str).is_some(), "{t:#?}");
        assert!(
            t.get("description").and_then(Value::as_str).is_some(),
            "{t:#?}"
        );
    }

    let notification_count = lines
        .iter()
        .filter(|v| {
            v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
                && v.get("id").is_none()
        })
        .count();
    assert_eq!(
        notification_count, 1,
        "expected exactly one list_changed notification for the batch; saw: {lines:#?}"
    );

    // Mixed valid/invalid names: partial failure is not isError, but the
    // errors array names the unknown toolset and loaded lists only the real one.
    let lines = p.call_tool_then_fence(
        "load_toolset",
        json!({"name": ["templates", "bogus_toolset"]}),
    );
    let r = lines
        .iter()
        .find(|v| v.get("result").is_some())
        .expect("expected a tools/call result")["result"]
        .clone();
    assert_ne!(r["isError"].as_bool(), Some(true), "{r:#?}");
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["loaded"], json!(["templates"]));
    let errors = body["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].as_str().unwrap().contains("list_toolboxes"),
        "{errors:#?}"
    );
}

/// All names in one `load_toolset` call unknown -> a typed `invalid_argument`
/// error (not a JSON body with a hand-set `isError`), so the observer keeps a
/// real `error_kind` column instead of degrading to `handler_error`.
#[test]
fn load_toolset_batch_total_failure_is_typed_error() {
    let mut p = McpProcess::spawn();
    let r = p.call_tool("load_toolset", json!({"name": ["bogus_one", "bogus_two"]}));
    assert_eq!(r["isError"], json!(true));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "name");
    assert!(
        body["message"].as_str().unwrap().contains("list_toolboxes"),
        "{body:#?}"
    );
}

/// With `auto_load_toolsets = true` in `konnect.toml` (picked up from the
/// server process's cwd), calling a tool from an unloaded toolset auto-loads
/// it and executes in the same call instead of returning `toolset_not_loaded`.
/// Default-off behavior (no config file) is covered by
/// `structured_errors_guide_recovery`.
#[test]
fn auto_load_toolsets_config_loads_and_executes_on_miss() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("konnect.toml"),
        "auto_load_toolsets = true\n",
    )
    .unwrap();
    let mut p = McpProcess::spawn_in_dir(Some(tmp.path()));

    // route_trace is in pcb_routing, not loaded at startup. With auto-load on,
    // the toolset loads, a list_changed notification fires, and the call gets
    // as far as argument validation instead of failing with
    // toolset_not_loaded — which is what this test is about.
    //
    // The field named is `board`, the first entry in route_trace's own
    // `required` list. This used to be `net_name`, whichever argument the
    // handler happened to read first; since #218 the dispatch checks
    // `required` in schema order before the handler runs, which is the order
    // the client was shown.
    let lines = p.call_tool_then_fence("route_trace", json!({}));
    let r = lines
        .iter()
        .find(|v| v.get("result").is_some())
        .expect("expected a tools/call result")["result"]
        .clone();
    assert_eq!(r["isError"], json!(true));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "board");

    let saw_notification = lines.iter().any(|v| {
        v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
            && v.get("id").is_none()
    });
    assert!(
        saw_notification,
        "expected notifications/tools/list_changed after auto-load; saw: {lines:#?}"
    );
}

// ─── Config provenance over the protocol (#419) ───────────────────────────────
//
// These spawn the real binary, so they prove what a client actually receives —
// not what the resolver returns in isolation. Paths are compared after
// `canonicalize` because macOS resolves a temp dir under /var to /private/var,
// and the server reports the resolved form.

fn canonical(path: &std::path::Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn configuration_block(p: &mut McpProcess) -> Value {
    let result = p.call_tool("get_installation_info", json!({}));
    assert_ne!(result["isError"], json!(true), "{result:#?}");
    McpProcess::tool_body(&result)["configuration"].clone()
}

#[test]
fn startup_logs_selected_and_skipped_config_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let selected = tmp.path().join("copperpour.toml");
    let skipped = [
        tmp.path().join("konnect.toml"),
        tmp.path().join("settings.json"),
    ];
    std::fs::write(&selected, "log_level = \"info\"\n").unwrap();
    for path in &skipped {
        std::fs::write(path, "invalid configuration").unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("APPDATA", tmp.path())
        .env("RUST_LOG", "info")
        .output()
        .expect("failed to run copperpour binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "startup logs must stay off stdout"
    );

    let log = stderr
        .lines()
        .find(|line| line.contains("configuration: search_path from"))
        .expect("missing startup configuration log");
    assert!(log.contains(&canonical(&selected)), "{log}");
    for path in skipped {
        let path = path.canonicalize().unwrap();
        assert!(log.contains(&format!("{path:?}")), "{log}");
    }
}

#[test]
fn installation_info_names_the_config_file_that_configured_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let selected = tmp.path().join("konnect.toml");
    std::fs::write(&selected, "log_level = \"debug\"\n").unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let configuration = configuration_block(&mut p);

    assert_eq!(configuration["source"], "search_path");
    assert_eq!(configuration["selected_path"], canonical(&selected));
    assert_eq!(configuration["search_policy"], "first_existing_no_merge");
    assert_eq!(configuration["skipped_existing_paths"], json!([]));
}

#[test]
fn installation_info_reports_a_shadowed_later_candidate() {
    // konnect.toml precedes settings.json, so the second exists but is not read.
    // Naming it is the whole point: "my settings.json is ignored" was
    // indistinguishable from "my settings.json is malformed" (#419).
    let tmp = tempfile::tempdir().unwrap();
    let selected = tmp.path().join("konnect.toml");
    let shadowed = tmp.path().join("settings.json");
    std::fs::write(&selected, "log_level = \"debug\"\n").unwrap();
    std::fs::write(&shadowed, "{\"log_level\": \"trace\"}").unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let configuration = configuration_block(&mut p);

    assert_eq!(configuration["source"], "search_path");
    assert_eq!(configuration["selected_path"], canonical(&selected));
    assert_eq!(
        configuration["skipped_existing_paths"],
        json!([canonical(&shadowed)]),
        "the shadowed file must be named, not merged and not hidden"
    );
}

#[test]
fn installation_info_reports_startup_state_not_a_fresh_search() {
    // Start with only settings.json, then create the higher-priority
    // konnect.toml while the server runs. The answer must not change: the
    // question is what configured this process, not what would configure a new
    // one.
    let tmp = tempfile::tempdir().unwrap();
    let selected = tmp.path().join("settings.json");
    std::fs::write(&selected, "{\"log_level\": \"debug\"}").unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    assert_eq!(
        configuration_block(&mut p)["selected_path"],
        canonical(&selected)
    );

    std::fs::write(tmp.path().join("konnect.toml"), "log_level = \"trace\"\n").unwrap();

    let after = configuration_block(&mut p);
    assert_eq!(
        after["selected_path"],
        canonical(&selected),
        "a file created after launch must not be reported as having configured this process"
    );
    assert_eq!(after["skipped_existing_paths"], json!([]));
}

#[test]
fn installation_info_reports_defaults_when_no_config_file_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);

    let configuration = configuration_block(&mut p);

    assert_eq!(configuration["source"], "defaults");
    assert_eq!(configuration["selected_path"], Value::Null);
    assert_eq!(configuration["skipped_existing_paths"], json!([]));
}

/// Spawn in `dir` with `PATH` = empty, `.`, `bin`. The first two name the
/// working directory too, so a lookup must skip them.
fn spawn_with_path_to(dir: &std::path::Path, bin: &std::path::Path) -> McpProcess {
    let path = std::env::join_paths([std::path::Path::new(""), ".".as_ref(), bin]).unwrap();
    McpProcess::spawn_with_env(Some(dir), true, &[("PATH", path.to_str().unwrap())])
}

/// A `kicad-cli` in the working directory used to end discovery, so `cli_path`
/// reported the relative bare name instead of the binary that runs (#182).
#[test]
fn a_kicad_cli_in_the_working_directory_does_not_stop_discovery() {
    let name = format!("kicad-cli{}", std::env::consts::EXE_SUFFIX);
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(tmp.path().join(&name), "").unwrap();
    std::fs::write(bin.join(&name), "").unwrap();

    let mut p = spawn_with_path_to(tmp.path(), &bin);
    let result = p.call_tool("get_installation_info", json!({}));
    assert_ne!(result["isError"], json!(true), "{result:#?}");
    assert_eq!(
        McpProcess::tool_body(&result)["kicad"]["cli_path"],
        bin.join(name).to_str().unwrap()
    );
}

/// The browser default does not discover or launch a Tauri executable. Verify
/// the default opener, headless reuse and failed opener through real stdio MCP.
#[cfg(unix)]
#[test]
fn browser_viewer_stdio_reports_service_and_opener_separately() {
    browser_viewer_stdio_case(false);
}

#[cfg(unix)]
#[test]
#[ignore = "requires kicad-cli; renders only a disposable fixture without a display session"]
fn browser_viewer_stdio_native_without_display_serves_svg_despite_opener_failure() {
    browser_viewer_stdio_case(true);
}

#[cfg(unix)]
fn browser_viewer_stdio_case(native_cli: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let executable = |name: &str| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
            .unwrap()
    };
    let shell = executable("sh");
    let mkdir = executable("mkdir");
    let cli = bin.join("cli");
    write_executable_script(&cli, format!("#!{}\n{} -p \"$5\"\nprintf '<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><text>saved</text></svg>' > \"$5/root.svg\"\n", shell.display(), mkdir.display()));
    #[cfg(not(target_os = "macos"))]
    let opener = bin.join("xdg-open");
    // macOS uses `open`; Windows uses rundll32 and is tested separately.
    #[cfg(target_os = "macos")]
    let opener = bin.join("open");
    write_executable_script(
        &opener,
        format!(
            "#!{}\nprintf '%s' \"$1\" > \"$0.called\"\nprintf 'garbage-on-stdout\\n'\n",
            shell.display()
        ),
    );
    std::fs::write(
        tmp.path().join("schematic-viewer"),
        "must never be launched",
    )
    .unwrap();
    let schematic = tmp.path().join("root.kicad_sch");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../copperpour-core/tests/fixtures/derived_lib_name.kicad_sch"
        ),
        &schematic,
    )
    .unwrap();
    let kicad = bin.join("kicad");
    write_executable_script(
        &kicad,
        format!(
            "#!{}\nprintf 'display=%s\\n' \"$DISPLAY\" >&2\nexit 255\n",
            shell.display()
        ),
    );
    let cli = if native_cli {
        std::env::var_os("COPPERPOUR_TEST_KICAD_CLI")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| executable("kicad-cli"))
    } else {
        cli
    };
    std::fs::write(
        tmp.path().join("copperpour.toml"),
        format!(
            "kicad_cli = {}\nkicad_binary = {}\n",
            serde_json::to_string(cli.to_str().unwrap()).unwrap(),
            serde_json::to_string(kicad.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    // The MCP client need not inherit a desktop session (#373).
    let mut p = McpProcess::spawn_with_env_without(
        Some(tmp.path()),
        true,
        &[("PATH", path.to_str().unwrap())],
        &["DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"],
    );
    let first = p.call_tool("open_schematic_viewer", json!({"schematic":schematic}));
    assert_ne!(first["isError"], true, "{first}");
    let first = McpProcess::tool_body(&first);
    assert_eq!(first["viewer"], "browser");
    assert_eq!(first["launched"], true);
    assert_eq!(first["browser_launch_requested"], true);
    assert_eq!(first["browser_launch_succeeded"], true);
    assert_eq!(
        std::fs::read_to_string(opener.with_file_name(format!(
            "{}.called",
            opener.file_name().unwrap().to_str().unwrap()
        )))
        .unwrap(),
        first["url"].as_str().unwrap()
    );
    let headless = p.call_tool(
        "open_schematic_viewer",
        json!({"schematic":schematic,"open_browser":false}),
    );
    let headless = McpProcess::tool_body(&headless);
    assert_eq!(headless["url"], first["url"]);
    assert_eq!(headless["reused"], true);
    assert_eq!(headless["browser_launch_requested"], false);
    assert_eq!(headless["browser_launch_succeeded"], false);
    let assert_served = |url: &str| {
        use std::io::Read;
        let (base, token) = url.split_once("/#token=").unwrap();
        let address = base.strip_prefix("http://").unwrap();
        let sheet = format!(
            "/sheet?file={}",
            schematic.canonicalize().unwrap().display()
        );
        for (target, expected) in [("/state", "root.kicad_sch"), (sheet.as_str(), "<svg")] {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.contains(expected), "expected {expected}");
        }
    };
    assert_served(headless["url"].as_str().unwrap());
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let (base, token) = headless["url"]
            .as_str()
            .unwrap()
            .split_once("/#token=")
            .unwrap();
        let address = base.strip_prefix("http://").unwrap();
        let body = json!({"file": schematic.canonicalize().unwrap()}).to_string();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(stream, "POST /open-in-kicad HTTP/1.1\r\nHost: {address}\r\nOrigin: {base}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(response.contains("display=:0"), "{response}");
        assert!(response.contains("fallback DISPLAY=:0"), "{response}");
        assert!(response.contains("XAUTHORITY"), "{response}");
        // A second browser-opener failure below must still report no display:
        // the fallback belongs to the KiCad child, not the server environment.
    }
    write_executable_script(&opener, format!("#!{}\nexit 7\n", shell.display()));
    let failed = p.call_tool("open_schematic_viewer", json!({"schematic":schematic}));
    assert_ne!(failed["isError"], true, "{failed}");
    let failed = McpProcess::tool_body(&failed);
    assert_eq!(failed["url"], first["url"]);
    assert_eq!(failed["launched"], true);
    assert_eq!(failed["browser_launch_requested"], true);
    assert_eq!(failed["browser_launch_succeeded"], false);
    let error = failed["browser_launch_error"].as_str().unwrap();
    assert!(error.contains('7'), "{error}");
    #[cfg(not(target_os = "macos"))]
    assert!(
        error.contains("Neither DISPLAY nor WAYLAND_DISPLAY is set"),
        "{error}"
    );
    assert_served(failed["url"].as_str().unwrap());
}

#[test]
fn env_fallback_applies_without_disturbing_reported_provenance() {
    // Acceptance row: a selected file with a blank ipc_address plus
    // KICAD_API_SOCKET in the environment. The env fallback must still be
    // applied after file resolution (#39), and provenance must still name the
    // file that was selected -- the env var configures a value, not a source.
    let tmp = tempfile::tempdir().unwrap();
    let selected = tmp.path().join("konnect.toml");
    std::fs::write(&selected, "ipc_address = \"\"\n").unwrap();

    let mut p = McpProcess::spawn_with_env(
        Some(tmp.path()),
        true,
        &[("KICAD_API_SOCKET", "ipc://konnect-419-env.sock")],
    );

    let result = p.call_tool("get_installation_info", json!({}));
    assert_ne!(result["isError"], json!(true), "{result:#?}");
    let body = McpProcess::tool_body(&result);

    assert_eq!(body["configuration"]["source"], "search_path");
    assert_eq!(body["configuration"]["selected_path"], canonical(&selected));
    assert_eq!(
        body["ipc"]["configured"],
        json!(true),
        "the blank ipc_address should have been filled from the environment"
    );
}

/// Run the server to completion with the given args, returning (exit ok, stderr).
/// Used for the cases where startup must FAIL: the MCP harness above expects a
/// handshake, and there deliberately is not one.
fn run_expecting_startup_failure(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("APPDATA", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run copperpour binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_malformed_selected_file_stops_startup_instead_of_falling_through() {
    // konnect.toml is selected and is broken; settings.json is valid and later.
    // Falling through would silently run on a configuration the user never
    // pointed at, while their real file went unreported — the failure #419 is
    // about, in its worst form.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("konnect.toml"), "not = valid toml [[[").unwrap();
    std::fs::write(
        tmp.path().join("settings.json"),
        "{\"log_level\":\"trace\"}",
    )
    .unwrap();

    let (ok, stderr) = run_expecting_startup_failure(tmp.path(), &[]);

    assert!(!ok, "a malformed selected file must not start the server");
    assert!(
        !stderr.contains("configuration: search_path"),
        "startup must not report a selection it never completed: {stderr}"
    );
}

#[test]
fn a_malformed_explicit_config_stops_startup_instead_of_falling_back() {
    // --config bypasses discovery, so a valid settings.json sitting next to it
    // must not rescue a broken explicit file either.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.toml"), "not = valid toml [[[").unwrap();
    std::fs::write(
        tmp.path().join("settings.json"),
        "{\"log_level\":\"trace\"}",
    )
    .unwrap();

    let (ok, stderr) = run_expecting_startup_failure(tmp.path(), &["--config", "bad.toml"]);

    assert!(
        !ok,
        "a malformed explicit --config must not start the server"
    );
    assert!(
        !stderr.contains("configuration:"),
        "startup must not report any configuration selection: {stderr}"
    );
}

#[test]
fn cli_explicit_config_flag_errors_refuse_startup_instead_of_falling_back() {
    let tmp = tempfile::tempdir().unwrap();
    // Place a valid search-path file in cwd to prove none of these fall back to discovery.
    std::fs::write(tmp.path().join("copperpour.toml"), "log_level = \"info\"\n").unwrap();
    let valid = tmp.path().join("valid.toml");
    std::fs::write(&valid, "log_level = \"info\"\n").unwrap();
    let valid_str = valid.to_str().unwrap();

    for (args, expected_reason) in [
        (vec!["--config"], "--config requires"),
        (vec!["--config", ""], "must not be empty"),
        (
            vec!["--config", valid_str, "--config", valid_str],
            "only be specified once",
        ),
    ] {
        let (ok, stderr) = run_expecting_startup_failure(tmp.path(), &args);
        assert!(
            !ok,
            "invalid --config invocation {args:?} must refuse startup, stderr={stderr}"
        );
        assert!(
            stderr.contains(expected_reason),
            "expected '{expected_reason}' in stderr for {args:?}, got: {stderr}"
        );
        assert!(
            !stderr.contains("configuration:"),
            "refused --config invocation {args:?} must not report a loaded configuration: {stderr}"
        );
    }
}

/// A server started from an SSH session has no display; `launch_kicad_ui`
/// says so, and `display` aims KiCad at one (#225). `/bin/sh` stands in for
/// KiCad and runs the `project` argument as its script.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn launch_kicad_ui_over_ssh_names_the_missing_display_and_takes_one() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("copperpour.toml"),
        "kicad_binary = \"/bin/sh\"\n",
    )
    .unwrap();
    let kicad = tmp.path().join("kicad.sh");
    std::fs::write(
        &kicad,
        "echo \"DISPLAY=$DISPLAY WAYLAND_DISPLAY=${WAYLAND_DISPLAY-unset} GDK_BACKEND=${GDK_BACKEND-unset}\" >&2; exit 255\n",
    )
    .unwrap();
    let mut p = McpProcess::spawn_with_env_without(
        Some(tmp.path()),
        true,
        &[("WAYLAND_DISPLAY", ""), ("GDK_BACKEND", "wayland")],
        &["DISPLAY"],
    );
    McpProcess::tool_body(&p.call_tool("load_toolset", json!({"name": "verification"})));
    let project = kicad.to_str().unwrap();

    let bare = p.call_tool(
        "launch_kicad_ui",
        json!({"project": project, "wait_ready": false}),
    );
    assert_eq!(bare["isError"], true, "{bare}");
    let text = bare["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("DISPLAY= WAYLAND_DISPLAY= GDK_BACKEND=wayland\n")
            && text.contains("Neither DISPLAY nor WAYLAND_DISPLAY is set")
            && text.contains("Pass display"),
        "{text}"
    );

    let aimed = p.call_tool(
        "launch_kicad_ui",
        json!({"project": project, "wait_ready": false, "display": ":0"}),
    );
    assert_eq!(aimed["isError"], true, "{aimed}");
    let text = aimed["content"][0]["text"].as_str().unwrap();
    assert!(
        text.ends_with("DISPLAY=:0 WAYLAND_DISPLAY=unset GDK_BACKEND=x11"),
        "the display reaches KiCad, and no hint follows: {text}"
    );
}

/// `ipc_failure` (#532) must reach a client over the real stdio protocol path,
/// not only the in-process handler. The endpoint is a socket nothing listens
/// on; the working directory and home are isolated so `KICAD_API_SOCKET` is
/// the address that resolves.
#[test]
fn ipc_failure_kind_and_message_reach_the_client_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let address = format!("ipc://{}", tmp.path().join("no-kicad-here.sock").display());
    let mut p = McpProcess::spawn_with_env(
        Some(tmp.path()),
        true,
        &[("KICAD_API_SOCKET", address.as_str())],
    );
    McpProcess::tool_body(
        &p.call_tool("load_toolset", json!({"name": ["project", "verification"]})),
    );

    for (tool, args) in [
        ("check_kicad_ui", json!({"timeout_seconds": 60})),
        ("open_project", json!({})),
    ] {
        let result = p.call_tool(tool, args);
        assert_ne!(result["isError"], json!(true), "{tool}: {result}");
        let body = McpProcess::tool_body(&result);
        assert_eq!(body["ipc_failure"]["kind"], "no_listener", "{tool}: {body}");
        assert!(
            body["ipc_failure"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Nothing is listening there")),
            "{tool}: {body}"
        );
    }
}

/// Where `user_config_path()` puts the preferences file when `HOME` /
/// `APPDATA` point at `home`.
fn user_config_under(home: &std::path::Path) -> std::path::PathBuf {
    let dir = if cfg!(target_os = "windows") {
        home.join("copperpour")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("copperpour")
    } else {
        home.join(".copperpour")
    };
    dir.join("config.json")
}

/// #580 over real stdio: a user preferences file that exists and cannot be
/// parsed is refused by the load, and the save that used to replace it with
/// the defaults refuses too and leaves it byte-identical.
#[test]
fn a_malformed_user_configuration_is_refused_and_never_overwritten_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let config = user_config_under(tmp.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let original =
        r#"{"kicad_cli": "D:/tools/kicad-cli.exe", "sourcing": {"avl": ["Vishay", "Murata"]},"#;
    std::fs::write(&config, original).unwrap();

    let mut p = McpProcess::spawn_with_env(Some(tmp.path()), true, &[]);
    for (tool, args) in [
        ("load_user_config", json!({})),
        (
            "save_user_config",
            json!({"key_path": "ui.theme", "value": "dark"}),
        ),
        ("get_effective_config", json!({})),
    ] {
        let result = p.call_tool(tool, args);
        assert_eq!(result["isError"], json!(true), "{tool}: {result}");
        let body = McpProcess::tool_body(&result);
        assert_eq!(
            body["error"]["kind"], "invalid_configuration",
            "{tool}: {body}"
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            original,
            "{tool} left the preferences file exactly as it was"
        );
    }
}

/// The other half: with no preferences file the load says it answered with
/// the defaults, leaves them on disk, and the next load reads that file.
#[test]
fn an_absent_user_configuration_reports_defaults_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let config = user_config_under(tmp.path());
    let mut p = McpProcess::spawn_with_env(Some(tmp.path()), true, &[]);

    let first = McpProcess::tool_body(&p.call_tool("load_user_config", json!({})));
    assert_eq!(first["source"], "defaults", "{first}");
    assert_eq!(first["persisted"], true, "{first}");
    assert!(config.is_file(), "the defaults were written to {config:?}");

    let second = McpProcess::tool_body(&p.call_tool("load_user_config", json!({})));
    assert_eq!(second["source"], "file", "{second}");
}

/// The served binary compares installed guidance with its own bundle once per
/// installed version and tells both the model and `get_installation_info`
/// (#728). It writes only its record under `.copperpour`, and a second start
/// with the same install neither rescans nor repeats the notice. The marker is
/// written where a build before the rename put it, `.konnect`, which the first
/// start moves.
/// Unix only: `dirs::home_dir` ignores `HOME` on Windows, so the child would
/// read the real profile there.
#[cfg(unix)]
#[test]
fn a_legacy_guidance_marker_is_reported_on_the_first_start_only() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".konnect")).unwrap();
    std::fs::write(tmp.path().join(".konnect/.installed"), "0.2.2").unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let instructions = p.initialized["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.starts_with("Copperpour operating rules for KiCad work:")
            && instructions.contains("Copperpour Claude guidance (installed by v0.2.2)"),
        "{instructions}"
    );
    let expected_init = format!("`{} init`", env!("CARGO_BIN_EXE_copperpour"));
    assert!(
        instructions.contains(&expected_init) || instructions.contains("`copperpour init`"),
        "{instructions}"
    );
    assert_eq!(instructions.matches("installed by v0.2.2").count(), 1);
    assert!(
        !instructions.contains("Copperpour Codex guidance"),
        "Codex was never installed"
    );

    let result = p.call_tool("get_installation_info", json!({}));
    let body = McpProcess::tool_body(&result);
    let guidance = &body["guidance"];
    assert_eq!(guidance["probe_status"], "ok");
    assert_eq!(guidance["claude"]["state"], "out_of_sync");
    assert_eq!(guidance["claude"]["checked"], "now");
    assert_eq!(guidance["claude"]["marker"]["version"], "0.2.2");
    assert_eq!(guidance["codex"]["state"], "not_installed");
    assert!(guidance["claude"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|file| file["status"] == "missing"));
    drop(p);

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let init = &p.initialized["result"];
    assert_eq!(
        init["instructions"],
        copperpour_core::guidance::SESSION_RULES
    );
    let result = p.call_tool("get_installation_info", json!({}));
    let guidance = &McpProcess::tool_body(&result)["guidance"];
    assert_eq!(guidance["claude"]["state"], "out_of_sync");
    assert_eq!(guidance["claude"]["checked"], "earlier");
    assert!(guidance["claude"].get("files").is_none(), "{guidance}");
    drop(p);

    for dir in [".claude", ".agents"] {
        assert!(!tmp.path().join(dir).exists(), "detection wrote {dir}");
    }
    assert!(
        !tmp.path().join(".konnect").exists(),
        "moved to .copperpour"
    );
    let mut data = std::fs::read_dir(tmp.path().join(".copperpour"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        // The call log and runtime directory every server start keeps.
        .filter(|name| name != "logs" && name != "run")
        .collect::<Vec<_>>();
    data.sort();
    assert_eq!(
        data,
        [
            ".guidance-checked-claude",
            ".guidance-checked-codex",
            ".installed"
        ]
    );
}

/// Regression for #697 over real stdio: a cached `.guidance-checked-claude`
/// record from an older binary (either legacy marker-only or an older
/// `bundle_digest`) must not silence a newer binary whose embedded bundle
/// changed; the upgraded binary rescans once, emits the `initialize` notice,
/// and stays quiet on the next start.
#[cfg(unix)]
#[test]
fn a_stale_guidance_check_record_rechecks_once_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join(".copperpour");
    std::fs::create_dir_all(&data_dir).unwrap();
    let installed_version = env!("CARGO_PKG_VERSION");
    std::fs::write(data_dir.join(".installed-claude"), installed_version).unwrap();
    // Simulate a pre-#697 marker-only cache record from an older binary with
    // the same crate version (the exact state observed on upgraded dev builds).
    std::fs::write(
        data_dir.join(".guidance-checked-claude"),
        serde_json::to_vec(&json!({
            "key": {
                "marker": {
                    "version": installed_version,
                    "legacy": false
                }
            },
            "state": "out_of_sync"
        }))
        .unwrap(),
    )
    .unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let instructions = p.initialized["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(&format!(
            "Copperpour Claude guidance (installed by v{installed_version})"
        )),
        "upgraded binary must not stay silent on a pre-#697 cache record: {instructions}"
    );
    let body = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(body["guidance"]["claude"]["checked"], "now");
    assert_eq!(body["guidance"]["claude"]["state"], "out_of_sync");
    drop(p);

    // Second start of the same binary reuses the updated record and stays quiet.
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    assert_eq!(
        p.initialized["result"]["instructions"],
        copperpour_core::guidance::SESSION_RULES
    );
    let body = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(body["guidance"]["claude"]["checked"], "earlier");
    assert_eq!(body["guidance"]["claude"]["state"], "out_of_sync");
    drop(p);

    // Simulate a record written by an earlier binary whose embedded bundle had
    // a different digest: the newer binary must re-check once and notify again.
    std::fs::write(
        data_dir.join(".guidance-checked-claude"),
        serde_json::to_vec(&json!({
            "key": {
                "marker": {
                    "version": installed_version,
                    "legacy": false
                },
                "bundle_digest": "0000000000000000000000000000000000000000000000000000000000000000"
            },
            "state": "out_of_sync"
        }))
        .unwrap(),
    )
    .unwrap();
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let instructions = p.initialized["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(&format!(
            "Copperpour Claude guidance (installed by v{installed_version})"
        )),
        "upgraded binary must re-check when bundle_digest differs: {instructions}"
    );
    let body = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(body["guidance"]["claude"]["checked"], "now");
    assert_eq!(body["guidance"]["claude"]["state"], "out_of_sync");
}

/// Regression for #722 over real stdio: a session that only runs `initialize`
/// (or `tools/list` and `get_guidance`) without calling another tool must not
/// burn `.guidance-checked-claude`, and the first non-`get_guidance` tool call
/// in a session must carry `guidance_notice` once and record the check.
#[cfg(unix)]
#[test]
fn guidance_drift_notice_survives_idle_session_and_attaches_to_first_tool_call() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join(".copperpour");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join(".installed-claude"), "0.2.2").unwrap();

    // First session: non-KiCad session that only initializes and lists tools.
    let mut idle = McpProcess::spawn_configured(Some(tmp.path()), true);
    let instructions = idle.initialized["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Copperpour Claude guidance (installed by v0.2.2)"),
        "{instructions}"
    );
    let _ = idle.request("tools/list", json!({}));
    drop(idle);
    assert!(
        !data_dir.join(".guidance-checked-claude").exists(),
        "an idle session that called no tools must not record .guidance-checked-claude"
    );

    // Second session: real KiCad session reads get_guidance first, then loads a toolset.
    let mut active = McpProcess::spawn_configured(Some(tmp.path()), true);
    let instructions = active.initialized["result"]["instructions"]
        .as_str()
        .unwrap();
    assert!(
        instructions.contains("Copperpour Claude guidance (installed by v0.2.2)"),
        "second session must still see the notice after an idle session: {instructions}"
    );

    let guidance_doc = McpProcess::tool_body(
        &active.call_tool("get_guidance", json!({"path": "copperpour/SKILL.md"})),
    );
    assert!(
        guidance_doc.get("guidance_notice").is_none(),
        "get_guidance must preserve its resources/read payload without consuming the notice"
    );
    assert!(
        !data_dir.join(".guidance-checked-claude").exists(),
        "get_guidance alone must not consume the one-time drift notice"
    );

    let loaded =
        McpProcess::tool_body(&active.call_tool("load_toolset", json!({"name": "sch_components"})));
    let notice = loaded["guidance_notice"]
        .as_str()
        .expect("first non-get_guidance tool call must carry guidance_notice");
    let expected_init = format!("`{} init`", env!("CARGO_BIN_EXE_copperpour"));
    assert!(
        notice.contains("Copperpour Claude guidance (installed by v0.2.2)")
            && (notice.contains(&expected_init) || notice.contains("`copperpour init`")),
        "{notice}"
    );
    assert!(
        data_dir.join(".guidance-checked-claude").exists(),
        "first non-get_guidance tool call must record .guidance-checked-claude"
    );

    // Later tool calls in the same drifted session keep `guidance_notice` so a
    // multi-tool turn still sees the warning on its final tool call.
    let info = McpProcess::tool_body(&active.call_tool("get_installation_info", json!({})));
    assert_eq!(info["guidance_notice"], notice);
    assert_eq!(info["guidance"]["claude"]["checked"], "now");
    assert_eq!(info["guidance"]["claude"]["state"], "out_of_sync");
    drop(active);

    // Third session with the same install and bundle stays quiet.
    let mut later = McpProcess::spawn_configured(Some(tmp.path()), true);
    assert_eq!(
        later.initialized["result"]["instructions"],
        copperpour_core::guidance::SESSION_RULES
    );
    let loaded_again =
        McpProcess::tool_body(&later.call_tool("load_toolset", json!({"name": "sch_components"})));
    assert!(
        loaded_again.get("guidance_notice").is_none(),
        "later session must stay quiet once the notice was delivered: {loaded_again}"
    );
}

/// Regression for #698 over real stdio: when built with a commit SHA,
/// `copperpour init` writes `<version>+<short_commit>` into the install marker,
/// and both `copperpour status` and the MCP `initialize` drift notice include
/// the commit suffix so dev builds on the same crate version are distinguishable.
#[cfg(unix)]
#[test]
fn dev_build_install_marker_and_drift_notice_include_build_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(["init", "--client", "claude"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "copperpour init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // Edit one installed skill so drift detection triggers.
    let skill = tmp.path().join(".claude/skills/copperpour/SKILL.md");
    std::fs::write(&skill, "local edit").unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let body = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    // A commit read from `.git/HEAD` marks a dev build; a release job passes
    // its commit in and keeps the bare version.
    let expected = match (
        body["build"]["commit_source"].as_str(),
        body["build"]["commit"].as_str(),
    ) {
        (Some("git_head"), Some(commit)) => format!(
            "{}+{}",
            env!("CARGO_PKG_VERSION"),
            commit[..commit.len().min(8)].to_ascii_lowercase()
        ),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    };

    let marker = std::fs::read_to_string(tmp.path().join(".copperpour/.installed-claude")).unwrap();
    assert_eq!(marker.trim(), expected);

    let instructions = p.initialized["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(&format!(
            "Copperpour Claude guidance (installed by v{expected}) does not match this server v{expected}:"
        )),
        "drift notice should include build commit suffix: {instructions}"
    );
    assert_eq!(body["guidance"]["claude"]["marker"]["version"], expected);
    drop(p);

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(["status", "--client", "claude"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(status.status.success());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains(&format!("Copperpour v{expected} — Claude Install Status")),
        "status header should include build commit suffix: {stdout}"
    );
    assert!(
        stdout.contains(&format!("Install marker: v{expected}")),
        "status marker should include build commit suffix: {stdout}"
    );
}

#[cfg(unix)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Regression for #731 over real stdio: when an upgraded server sees an
/// existing install whose files and hooks match the digests recorded in
/// `.installed-manifest-claude.json` (unmodified since the previous install),
/// it updates them in place, updates `.installed-claude`, and emits no
/// `guidance_notice`.
#[cfg(unix)]
#[test]
fn auto_updates_unmodified_guidance_on_upgrade_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(["init", "--client", "claude"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "copperpour init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // Simulate an older release's install by replacing a skill and updating
    // the recorded manifest + marker to match that older version.
    let skill_path = tmp.path().join(".claude/skills/copperpour/SKILL.md");
    let current_skill = std::fs::read_to_string(&skill_path).unwrap();
    let old_skill = "old bundled copperpour skill v0.15.0";
    std::fs::write(&skill_path, old_skill).unwrap();

    let data_dir = tmp.path().join(".copperpour");
    std::fs::write(data_dir.join(".installed-claude"), "0.15.0").unwrap();
    let manifest_path = data_dir.join(".installed-manifest-claude.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["version"] = json!("0.15.0");
    manifest["files"]["skill:copperpour/SKILL.md"] = json!(sha256_hex(old_skill.as_bytes()));
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // Start the upgraded server over stdio.
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    assert_eq!(
        p.initialized["result"]["instructions"],
        copperpour_core::guidance::SESSION_RULES,
        "unmodified guidance should auto-update without an initialize notice"
    );

    let loaded =
        McpProcess::tool_body(&p.call_tool("load_toolset", json!({"name": "sch_components"})));
    assert!(
        loaded.get("guidance_notice").is_none(),
        "tool call must carry no guidance_notice after clean auto-update: {loaded}"
    );

    let info = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(info["guidance"]["claude"]["state"], "current");
    assert_eq!(
        std::fs::read_to_string(&skill_path).unwrap(),
        current_skill,
        "unmodified stale skill should be updated in place"
    );
    assert_ne!(
        std::fs::read_to_string(data_dir.join(".installed-claude"))
            .unwrap()
            .trim(),
        "0.15.0",
        "install marker should advance after clean auto-update"
    );
}

/// Regression for #731 over real stdio: during an upgrade with both an
/// unmodified stale file and a user-modified file, the server updates the
/// unmodified file, leaves the user's edit untouched, and emits
/// `guidance_notice` naming the edited file and the runnable binary path when
/// `copperpour` is not on `PATH`.
#[cfg(unix)]
#[test]
fn mixed_upgrade_preserves_user_edits_and_reports_runnable_exe_path_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(["init", "--client", "claude"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(init.status.success());

    // Stale unmodified file (matches manifest).
    let stale_path = tmp.path().join(".claude/skills/copperpour/SKILL.md");
    let current_stale = std::fs::read_to_string(&stale_path).unwrap();
    let old_stale = "old bundled copperpour skill v0.15.0";
    std::fs::write(&stale_path, old_stale).unwrap();

    // User-modified file (differs from both manifest and current bundle).
    let edited_path = tmp.path().join(".claude/skills/kicad-pcb/SKILL.md");
    std::fs::write(&edited_path, "user custom pcb rules").unwrap();

    let data_dir = tmp.path().join(".copperpour");
    std::fs::write(data_dir.join(".installed-claude"), "0.15.0").unwrap();
    let manifest_path = data_dir.join(".installed-manifest-claude.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["version"] = json!("0.15.0");
    manifest["files"]["skill:copperpour/SKILL.md"] = json!(sha256_hex(old_stale.as_bytes()));
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // Run with an empty PATH so `copperpour` is guaranteed not to be on PATH.
    let mut p = McpProcess::spawn_with_env(Some(tmp.path()), true, &[("PATH", "")]);
    let loaded =
        McpProcess::tool_body(&p.call_tool("load_toolset", json!({"name": "sch_components"})));
    let notice = loaded["guidance_notice"]
        .as_str()
        .expect("user-modified file must surface guidance_notice");
    let expected_cmd = format!("`{} init`", env!("CARGO_BIN_EXE_copperpour"));
    assert!(
        notice.contains("(locally modified: kicad-pcb/SKILL.md)"),
        "notice must name the locally modified file: {notice}"
    );
    assert!(
        notice.contains(&expected_cmd),
        "notice must include the runnable executable path when not on PATH: {notice}"
    );
    assert_eq!(
        std::fs::read_to_string(&stale_path).unwrap(),
        current_stale,
        "unmodified stale file should still be auto-updated"
    );
    assert_eq!(
        std::fs::read_to_string(&edited_path).unwrap(),
        "user custom pcb rules",
        "user-modified file must remain untouched"
    );
}

/// Regression for #737 over real stdio: `copperpour init --client claude`
/// writes `PreToolUse` hook matchers covering `mcp__Copperpour__*` (Claude
/// Desktop `.mcpb` display_name) and agents allowing `mcp__Copperpour__*`, and
/// upgrading from an earlier release's lowercase-only
/// `mcp__(copperpour|konnect)__(...)` matchers auto-updates `settings.json`
/// cleanly on server start.
#[cfg(unix)]
#[test]
fn auto_updates_legacy_lowercase_hook_matcher_and_agent_tools_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_copperpour"))
        .args(["init", "--client", "claude"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "copperpour init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let settings_path = tmp.path().join(".claude/settings.json");
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        4,
        "expected 4 PreToolUse hooks: {entries:#?}"
    );
    for entry in entries {
        let matcher = entry["matcher"].as_str().unwrap();
        assert!(
            matcher.starts_with("mcp__(copperpour|Copperpour|konnect|Konnect)__("),
            "installed hook matcher must include Copperpour/Konnect: {matcher}"
        );
    }
    for agent_name in [
        "kicad-schematic-build-agent.md",
        "kicad-design-review-agent.md",
    ] {
        let agent =
            std::fs::read_to_string(tmp.path().join(".claude/agents").join(agent_name)).unwrap();
        assert!(
            agent.contains("mcp__Copperpour__*"),
            "{agent_name} must allow mcp__Copperpour__*"
        );
        assert!(
            agent.contains("mcp__Konnect__*"),
            "{agent_name} must allow mcp__Konnect__*"
        );
    }

    // Downgrade settings.json + an installed agent + manifest + marker to the
    // pre-#737 lowercase-only form and verify real stdio startup auto-updates
    // both the hook matchers and the bundled agent.
    let mut legacy_settings = settings.clone();
    let data_dir = tmp.path().join(".copperpour");
    std::fs::write(data_dir.join(".installed-claude"), "0.15.0").unwrap();
    let manifest_path = data_dir.join(".installed-manifest-claude.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["version"] = json!("0.15.0");

    let review_agent_path = tmp
        .path()
        .join(".claude/agents/kicad-design-review-agent.md");
    let current_agent = std::fs::read_to_string(&review_agent_path).unwrap();
    let legacy_agent = current_agent
        .replace("  - mcp__Copperpour__*\n", "")
        .replace("  - mcp__Konnect__*\n", "");
    assert_ne!(legacy_agent, current_agent);
    std::fs::write(&review_agent_path, &legacy_agent).unwrap();
    manifest["files"]["agent:kicad-design-review-agent.md"] =
        json!(sha256_hex(legacy_agent.as_bytes()));

    for entry in legacy_settings["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
    {
        let current = entry["matcher"].as_str().unwrap();
        let legacy = current.replacen(
            "mcp__(copperpour|Copperpour|konnect|Konnect)__(",
            "mcp__(copperpour|konnect)__(",
            1,
        );
        assert_ne!(legacy, current);
        entry["matcher"] = json!(legacy);
    }
    for (_, record) in manifest["hooks"].as_object_mut().unwrap() {
        let current = record["matcher"].as_str().unwrap();
        let legacy = current.replacen(
            "mcp__(copperpour|Copperpour|konnect|Konnect)__(",
            "mcp__(copperpour|konnect)__(",
            1,
        );
        assert_ne!(legacy, current);
        record["matcher"] = json!(legacy);
    }
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&legacy_settings).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    assert_eq!(
        p.initialized["result"]["instructions"],
        copperpour_core::guidance::SESSION_RULES,
        "clean matcher/agent upgrade should not append an initialize notice"
    );
    let info = McpProcess::tool_body(&p.call_tool("get_installation_info", json!({})));
    assert_eq!(info["guidance"]["claude"]["state"], "current", "{info}");
    assert!(
        info.get("guidance_notice").is_none(),
        "clean matcher upgrade must not emit guidance_notice: {info}"
    );
    assert_ne!(
        std::fs::read_to_string(data_dir.join(".installed-claude"))
            .unwrap()
            .trim(),
        "0.15.0",
        "install marker should advance after clean matcher/agent auto-update"
    );

    let upgraded: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    for entry in upgraded["hooks"]["PreToolUse"].as_array().unwrap() {
        let matcher = entry["matcher"].as_str().unwrap();
        assert!(
            matcher.starts_with("mcp__(copperpour|Copperpour|konnect|Konnect)__("),
            "upgraded hook matcher must include Copperpour/Konnect: {matcher}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&review_agent_path).unwrap(),
        current_agent,
        "unmodified legacy agent should auto-update to include mcp__Copperpour__*"
    );
}

/// #787 over real stdio: with nothing to find, `check_freerouting` reports
/// every location it searched, in the documented order, from the server's own
/// environment. The environment is redirected so a Freerouting installed on
/// the machine running the test cannot be found.
#[test]
#[cfg(feature = "freerouting")]
fn check_freerouting_reports_every_location_it_searched_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let third_party: Vec<String> = ["3rd-10", "3rd-9", "3rd-8"]
        .iter()
        .map(|name| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir.display().to_string()
        })
        .collect();
    let (bin_s, home_s) = (bin.display().to_string(), tmp.path().display().to_string());
    let mut p = McpProcess::spawn_with_env(
        Some(tmp.path()),
        true,
        &[
            ("PATH", bin_s.as_str()),
            ("USERPROFILE", home_s.as_str()),
            ("KICAD10_3RD_PARTY", third_party[0].as_str()),
            ("KICAD9_3RD_PARTY", third_party[1].as_str()),
            ("KICAD8_3RD_PARTY", third_party[2].as_str()),
        ],
    );
    McpProcess::tool_body(&p.call_tool("load_toolset", json!({"name": "integration"})));

    let result = p.call_tool("check_freerouting", json!({}));
    assert_ne!(result["isError"], json!(true), "{result}");
    let body = McpProcess::tool_body(&result);
    assert_eq!(body["engine_found"], false, "{body}");
    assert_eq!(body["available"], false, "{body}");

    let searched = body["searched_locations"]
        .as_array()
        .expect("searched_locations is reported");
    let kinds: Vec<&str> = searched
        .iter()
        .map(|root| root["kind"].as_str().unwrap())
        .collect();
    // `HOME` and `USERPROFILE` name the same directory here, so on Windows the
    // `Documents` plugin folders are reached twice and searched once.
    let plugin_folders = 6;
    let mut expected = vec!["kicad_3rd_party_variable"; 3];
    expected.extend(vec!["kicad_plugin_folder"; plugin_folders]);
    expected.extend(["working_directory", "system_directory", "system_directory"]);
    expected.push("path_directory");
    assert_eq!(kinds, expected, "{body}");

    for (root, dir) in searched[..3].iter().zip(&third_party) {
        assert_eq!(root["location_path"], json!(dir), "{root}");
        assert_eq!(root["exists"], true, "{root}");
    }
    let working = std::path::PathBuf::from(
        searched[3 + plugin_folders]["location_path"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(working.file_name().unwrap(), "freerouting.jar");
    assert_eq!(
        working.parent().unwrap().canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    assert_eq!(searched[3 + plugin_folders]["exists"], false);
    let last = searched.last().unwrap();
    assert_eq!(last["location_path"], json!(bin_s), "{last}");
    assert_eq!(last["max_depth"], 1, "{last}");
}

/// A relative `HOME` used to put every per-user path under the server's
/// working directory, so the server read and wrote files there (#171). Each
/// such file is planted where that path would land; none may be read, and the
/// server must create nothing beside them.
/// Unix only: Windows reads `APPDATA`, and falls back to the real profile.
#[cfg(unix)]
#[cfg(feature = "jlcpcb")]
#[test]
fn a_relative_home_never_reaches_the_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let table = "(sym_lib_table (version 7) (lib (name \"Planted\")(type \"KiCad\")(uri \"/planted.kicad_sym\")(options \"\")(descr \"\")))\n";
    let toml = "log_level = \"trace\"\n";
    let template = json!({"id": "planted", "name": "Planted", "components": [], "connections": []})
        .to_string();
    let schematic = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../copperpour-core/tests/fixtures/derived_lib_name.kicad_sch"
    ))
    .unwrap();
    let project = tmp.path().join("project");
    // Both the Linux and the macOS layouts, so one list serves either.
    let planted = [
        (user_config_under(&home), "{\"planted\": true}"),
        (home.join(".copperpour/templates/planted.json"), &template),
        (home.join(".config/kicad/10.0/sym-lib-table"), table),
        (
            home.join("Library/Preferences/kicad/10.0/sym-lib-table"),
            table,
        ),
        (home.join(".config/copperpour/config.toml"), toml),
        (
            home.join("Library/Application Support/copperpour/config.toml"),
            toml,
        ),
        (project.join("derived_lib_name.kicad_sch"), &schematic),
        (project.join("derived_lib_name.kicad_pro"), "{}\n"),
    ];
    for (path, content) in &planted {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    // A spawned kicad-cli creates KiCad's own user directories and config files
    // under the `HOME` it inherits; those are KiCad's paths, not Copperpour's,
    // and the directories leading to them are skipped by comparing files only.
    let kicad_data = home.join(".local/share/kicad");
    let kicad_config = [
        home.join(".config/kicad/10.0/kicad.json"),
        home.join(".config/kicad/10.0/kicad_common.json"),
    ];
    let tree = |dir: &std::path::Path| {
        let mut files: Vec<_> = walk(dir)
            .into_iter()
            .filter(|path| !path.starts_with(&kicad_data) && !kicad_config.contains(path))
            .collect();
        files.sort();
        files
    };
    let before = tree(tmp.path());

    // Empty counts as unset, so the lock root comes from `HOME` alone. The
    // Freerouting search is kept off any JAR this machine has.
    let empty = tmp.path().join("empty");
    let empty = empty.to_str().unwrap();
    let mut p = McpProcess::spawn_with_env(
        Some(tmp.path()),
        false,
        &[
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", ""),
            ("XDG_DATA_HOME", ""),
            ("COPPERPOUR_STATE_DIR", ""),
            ("KONNECT_STATE_DIR", ""),
            ("PATH", empty),
            ("KICAD10_3RD_PARTY", empty),
            ("KICAD9_3RD_PARTY", empty),
            ("KICAD8_3RD_PARTY", empty),
        ],
    );
    assert_ne!(
        configuration_block(&mut p)["source"],
        "search_path",
        "the planted config.toml was selected"
    );
    for (tool, args) in [
        ("load_user_config", json!({})),
        (
            "save_user_config",
            json!({"key_path": "planted", "value": false}),
        ),
        ("get_effective_config", json!({})),
        ("list_design_rules", json!({})),
        (
            "add_design_rule",
            json!({"rule": "planted", "scope": "user"}),
        ),
    ] {
        let result = p.call_tool(tool, args);
        assert_eq!(result["isError"], json!(true), "{tool}: {result:#?}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("home directory is unknown"), "{tool}: {text}");
    }

    p.call_tool("load_toolset", json!({"name": "templates"}));
    let template = p.call_tool("get_template", json!({"template_id": "planted"}));
    assert!(
        !template.to_string().contains("Planted"),
        "the planted template was read: {template:#?}"
    );

    p.call_tool("load_toolset", json!({"name": "library"}));
    let global = p.call_tool("list_symbol_libraries", json!({"scope": "global"}));
    assert_eq!(global["isError"], json!(true), "{global:#?}");
    assert!(
        global.to_string().contains("home directory is unknown"),
        "{global:#?}"
    );
    assert!(
        !global.to_string().contains("Planted"),
        "the planted sym-lib-table was read: {global:#?}"
    );

    p.call_tool("load_toolset", json!({"name": "integration"}));
    let download = p.call_tool("download_jlcpcb_database", json!({}));
    assert_eq!(download["isError"], json!(true), "{download:#?}");
    assert!(
        download.to_string().contains("home directory is unknown"),
        "{download:#?}"
    );

    // The document lock lives below the home directory too, so a schematic
    // edit refuses rather than lock the file under the working directory
    // (#179).
    p.call_tool("load_toolset", json!({"name": "sch_wiring"}));
    let wire = p.call_tool(
        "add_wire",
        json!({
            "schematic": project.join("derived_lib_name.kicad_sch"),
            "x1": 10.16, "y1": 10.16, "x2": 20.32, "y2": 10.16
        }),
    );
    assert_eq!(wire["isError"], json!(true), "{wire:#?}");
    assert!(
        wire.to_string().contains("local-data directory"),
        "{wire:#?}"
    );

    let freerouting = McpProcess::tool_body(&p.call_tool("check_freerouting", json!({})));
    assert_eq!(freerouting["engine_found"], false, "{freerouting}");
    // The server reports its canonical working directory (/private/var on macOS).
    let reported_home = std::path::Path::new(&canonical(tmp.path())).join("home");
    let searched = freerouting["searched_locations"].as_array().unwrap();
    assert!(
        !searched.iter().any(|location| location["location_path"]
            .as_str()
            .is_some_and(|path| std::path::Path::new(path).starts_with(&reported_home))),
        "a plugin folder below the relative home was searched: {searched:#?}"
    );

    drop(p);
    assert_eq!(
        tree(tmp.path()),
        before,
        "no call log, run record, lock, config or database was created under the working directory"
    );
    for (path, content) in &planted {
        assert_eq!(std::fs::read_to_string(path).unwrap(), *content, "{path:?}");
    }
}

#[cfg(all(unix, feature = "jlcpcb"))]
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// PCM/MCP registration alone exposes bundled guidance; no native skill install.
#[cfg(unix)]
#[test]
fn server_guidance_works_over_stdio_without_init_or_profile_skill_writes() {
    let profile = tempfile::tempdir().unwrap();
    let mut process = McpProcess::spawn_configured(Some(profile.path()), true);
    let init = &process.initialized["result"];
    assert!(init["instructions"]
        .as_str()
        .unwrap()
        .contains("Never hand-edit"));
    assert_eq!(init["capabilities"]["resources"], json!({}));
    let listed = McpProcess::tool_body(&process.call_tool("get_guidance", json!({})));
    assert_eq!(listed["count"], 17);
    let uri = "copperpour://guidance/copperpour/references/reliability-contract.md";
    let resource = process.request("resources/read", json!({"uri":uri}));
    let tool = McpProcess::tool_body(&process.call_tool(
        "get_guidance",
        json!({"path":"copperpour/references/reliability-contract.md"}),
    ));
    assert_eq!(resource["result"], tool);
    assert_eq!(
        tool["contents"][0]["text"],
        include_str!("../../../docs/RELIABILITY_CONTRACT.md")
    );
    let ts_uri = "copperpour://guidance/copperpour/references/troubleshooting.md";
    let ts_resource = process.request("resources/read", json!({"uri":ts_uri}));
    let ts_tool = McpProcess::tool_body(&process.call_tool(
        "get_guidance",
        json!({"path":"copperpour/references/troubleshooting.md"}),
    ));
    assert_eq!(ts_resource["result"], ts_tool);
    assert_eq!(
        ts_tool["contents"][0]["text"],
        include_str!("../assets/skills/copperpour/references/troubleshooting.md")
    );
    assert!(!profile.path().join(".claude").exists());
    assert!(!profile.path().join(".agents").exists());
}

// ─── call_tool: what the gateway may run (#60) ────────────────────────────────
//
// A client approves tools by name, so these go through the served
// `tools/call`: what they prove is what a client that allowed only `call_tool`
// can and cannot make the server do.

fn list_changed_count(lines: &[Value]) -> usize {
    lines
        .iter()
        .filter(|v| {
            v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
                && v.get("id").is_none()
        })
        .count()
}

/// The `tools/call` result among the lines `call_tool_then_fence` returned.
fn fenced_result(lines: &[Value]) -> Value {
    lines
        .iter()
        .find(|v| v["result"].get("content").is_some())
        .unwrap_or_else(|| panic!("no tools/call result in {lines:#?}"))["result"]
        .clone()
}

fn listed_tools(p: &mut McpProcess) -> Vec<String> {
    let list = p.request("tools/list", json!({}));
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[track_caller]
fn assert_not_permitted(result: &Value, tool: &str, reason: &str) {
    assert_eq!(result["isError"], json!(true), "{result}");
    let body = McpProcess::tool_body(result);
    assert_eq!(body["error"]["kind"], "tool_not_permitted", "{body}");
    assert_eq!(body["error"]["tool"], tool, "{body}");
    assert_eq!(body["error"]["reason"], reason, "{body}");
}

/// By default a writing target is refused before its handler runs, whether
/// its toolset is unloaded, loaded at startup or loaded later. The arguments
/// are ones the handler accepts: the same call made directly writes the file.
#[test]
fn call_tool_refuses_a_writing_target_before_its_handler_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let sheet = tmp.path().join("gateway.kicad_sch");
    let sheet_args = json!({"path": sheet.to_string_lossy()});
    let project = tmp.path().join("gateway_project");
    let mut p = McpProcess::spawn();
    let before = listed_tools(&mut p);
    assert!(before.iter().any(|name| name == "call_tool"));
    assert!(before.iter().any(|name| name == "create_project"));
    assert!(!before.iter().any(|name| name == "create_schematic"));

    // Unloaded toolset.
    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "create_schematic", "arguments": sheet_args}),
    );
    assert_not_permitted(
        &fenced_result(&lines),
        "create_schematic",
        "writes_not_enabled",
    );
    assert_eq!(list_changed_count(&lines), 0, "{lines:#?}");
    assert!(!sheet.exists(), "the refused handler wrote {sheet:?}");

    // Toolset of the starter kit: listed, and still not reachable this way.
    let r = p.call_tool(
        "call_tool",
        json!({
            "name": "create_project",
            "arguments": {"name": "gateway_project", "path": project.to_string_lossy()}
        }),
    );
    assert_not_permitted(&r, "create_project", "writes_not_enabled");
    assert!(!project.exists(), "the refused handler wrote {project:?}");

    // No argument widens the gateway: the setting is the operator's.
    let r = p.call_tool(
        "call_tool",
        json!({"name": "create_schematic", "arguments": sheet_args, "allow_writes": true}),
    );
    assert_eq!(
        McpProcess::tool_body(&r)["error"]["kind"],
        "invalid_argument"
    );
    let r = p.call_tool(
        "call_tool",
        json!({
            "name": "create_schematic",
            "arguments": sheet_args,
            "call_tool_allow_writes": true
        }),
    );
    assert_eq!(
        McpProcess::tool_body(&r)["error"]["kind"],
        "invalid_argument"
    );
    assert!(!sheet.exists());

    // Loading the toolset changes what is listed, not what the gateway runs.
    let loaded = p.call_tool("load_toolset", json!({"name": "sch_hierarchy"}));
    assert_ne!(loaded["isError"], json!(true), "{loaded}");
    let r = p.call_tool(
        "call_tool",
        json!({"name": "create_schematic", "arguments": sheet_args}),
    );
    assert_not_permitted(&r, "create_schematic", "writes_not_enabled");
    assert!(!sheet.exists(), "the refused handler wrote {sheet:?}");

    // The refusal is logged under the target's name.
    let recent = McpProcess::tool_body(&p.call_tool("get_recent_calls", json!({"limit": 1})));
    assert_eq!(recent["calls"][0]["tool"], "create_schematic", "{recent}");
    assert_eq!(
        recent["calls"][0]["error_kind"], "tool_not_permitted",
        "{recent}"
    );

    // Control: the same arguments, called directly, reach the handler.
    let direct = p.call_tool("create_schematic", sheet_args);
    assert_ne!(direct["isError"], json!(true), "{direct}");
    assert!(sheet.exists());
}

/// A read-only target runs from an unloaded toolset, gives what the direct
/// call gives, and is checked as a direct call is: the presence check, the
/// compiled schema's types and its closed set of keys. The catalogue does not
/// change and no notification is sent.
#[test]
fn call_tool_runs_a_read_only_target_with_its_own_argument_validation() {
    let mut p = McpProcess::spawn();
    let before = listed_tools(&mut p);
    assert!(!before.iter().any(|name| name == "get_template"));

    let refused = |p: &mut McpProcess, arguments: Value| {
        let r = p.call_tool(
            "call_tool",
            json!({"name": "get_template", "arguments": arguments}),
        );
        assert_eq!(r["isError"], json!(true), "{r}");
        McpProcess::tool_body(&r)["error"].clone()
    };
    let missing = refused(&mut p, json!({}));
    assert_eq!(missing["kind"], "invalid_argument", "{missing}");
    assert_eq!(missing["field"], "template_id", "{missing}");
    let wrong_type = refused(&mut p, json!({"template_id": 5}));
    assert_eq!(wrong_type["kind"], "invalid_argument", "{wrong_type}");
    assert_eq!(wrong_type["field"], "template_id", "{wrong_type}");
    let unknown_key = refused(
        &mut p,
        json!({"template_id": "usb_c_5v_sink", "no_such_argument": 1}),
    );
    assert_eq!(unknown_key["kind"], "invalid_argument", "{unknown_key}");

    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "get_template", "arguments": {"template_id": "usb_c_5v_sink"}}),
    );
    let via = fenced_result(&lines);
    assert_ne!(via["isError"], json!(true), "{via}");
    assert_eq!(list_changed_count(&lines), 0, "{lines:#?}");
    assert_eq!(listed_tools(&mut p), before, "tools/list changed");
    let active = McpProcess::tool_body(&p.call_tool("get_active_toolsets", json!({})));
    assert!(!active.to_string().contains("templates"), "{active}");

    let loaded = p.call_tool("load_toolset", json!({"name": "templates"}));
    assert_ne!(loaded["isError"], json!(true), "{loaded}");
    let direct = p.call_tool("get_template", json!({"template_id": "usb_c_5v_sink"}));
    assert_eq!(via, direct);
}

/// Meta-tools by default: the ones that only report run; `load_toolset` and
/// `unload_toolset` change the session and are refused, so the catalogue
/// stays and no `tools/list_changed` is sent; `call_tool` and `reload_server`
/// are never run.
#[test]
fn call_tool_applies_its_meta_tool_policy_by_default() {
    let mut p = McpProcess::spawn();
    let before = listed_tools(&mut p);

    let r = p.call_tool("call_tool", json!({"name": "get_active_toolsets"}));
    assert_ne!(r["isError"], json!(true), "{r}");
    let direct = p.call_tool("get_active_toolsets", json!({}));
    assert_eq!(r, direct);

    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "load_toolset", "arguments": {"name": "templates"}}),
    );
    assert_not_permitted(&fenced_result(&lines), "load_toolset", "writes_not_enabled");
    assert_eq!(list_changed_count(&lines), 0, "{lines:#?}");
    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "unload_toolset", "arguments": {"name": "config"}}),
    );
    assert_not_permitted(
        &fenced_result(&lines),
        "unload_toolset",
        "writes_not_enabled",
    );
    assert_eq!(list_changed_count(&lines), 0, "{lines:#?}");
    assert_eq!(listed_tools(&mut p), before, "tools/list changed");

    let r = p.call_tool(
        "call_tool",
        json!({"name": "call_tool", "arguments": {"name": "get_active_toolsets"}}),
    );
    assert_not_permitted(&r, "call_tool", "never_through_call_tool");
    let r = p.call_tool(
        "call_tool",
        json!({"name": "reload_server", "arguments": {"confirm": true}}),
    );
    assert_not_permitted(&r, "reload_server", "never_through_call_tool");
    // The server was not replaced and still answers.
    assert!(p.request("ping", json!({})).get("result").is_some());

    let r = p.call_tool("call_tool", json!({"name": "frobnicate_board"}));
    assert_eq!(McpProcess::tool_body(&r)["error"]["kind"], "unknown_tool");
}

/// With the operator's `call_tool_allow_writes = true` a writing target runs,
/// still under its own argument checks; a wrapped domain call leaves the
/// catalogue alone, a wrapped `load_toolset` changes it and sends one
/// `tools/list_changed` as the direct call does; `call_tool` and
/// `reload_server` stay refused.
#[test]
fn call_tool_allow_writes_is_the_operators_opt_in() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("copperpour.toml"),
        "call_tool_allow_writes = true\n",
    )
    .unwrap();
    let sheet = tmp.path().join("gateway.kicad_sch");
    let mut p = McpProcess::spawn_configured(Some(tmp.path()), true);
    let before = listed_tools(&mut p);
    assert!(!before.iter().any(|name| name == "create_schematic"));

    let r = p.call_tool("call_tool", json!({"name": "create_schematic"}));
    let body = McpProcess::tool_body(&r);
    assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "path", "{body}");
    let r = p.call_tool(
        "call_tool",
        json!({"name": "create_schematic", "arguments": {"path": 7}}),
    );
    assert_eq!(
        McpProcess::tool_body(&r)["error"]["kind"],
        "invalid_argument"
    );
    assert!(!sheet.exists());

    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "create_schematic", "arguments": {"path": sheet.to_string_lossy()}}),
    );
    let created = fenced_result(&lines);
    assert_ne!(created["isError"], json!(true), "{created}");
    assert!(sheet.exists());
    assert_eq!(list_changed_count(&lines), 0, "{lines:#?}");
    assert_eq!(listed_tools(&mut p), before, "tools/list changed");

    let lines = p.call_tool_then_fence(
        "call_tool",
        json!({"name": "load_toolset", "arguments": {"name": "templates"}}),
    );
    let loaded = fenced_result(&lines);
    assert_ne!(loaded["isError"], json!(true), "{loaded}");
    assert_eq!(list_changed_count(&lines), 1, "{lines:#?}");
    assert!(listed_tools(&mut p)
        .iter()
        .any(|name| name == "get_template"));

    let r = p.call_tool(
        "call_tool",
        json!({"name": "call_tool", "arguments": {"name": "get_active_toolsets"}}),
    );
    assert_not_permitted(&r, "call_tool", "never_through_call_tool");
    let r = p.call_tool(
        "call_tool",
        json!({"name": "reload_server", "arguments": {"confirm": true}}),
    );
    assert_not_permitted(&r, "reload_server", "never_through_call_tool");
    assert!(p.request("ping", json!({})).get("result").is_some());
}

/// The description a client shows says what allowing `call_tool` allows.
#[test]
fn call_tool_description_states_the_approval_consequence() {
    let mut p = McpProcess::spawn();
    let list = p.request("tools/list", json!({}));
    let description = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "call_tool")
        .expect("call_tool is listed")["description"]
        .as_str()
        .unwrap()
        .to_string();
    for phrase in [
        "only read-only tools",
        "call_tool_allow_writes",
        "without their usual individual approval prompts",
    ] {
        assert!(description.contains(phrase), "{description}");
    }
}

// ─── get_placement_overview on a KiCad-saved board ───────────────────────────
//
// The expected values are KiCad's, not this tool's: netclasses from pcbnew's
// `GetNetClassName`, airwires from `kicad-cli pcb drc`. The fixture's README
// records both and the script that produced them.

fn placement_overview(p: &mut McpProcess, extra: Value) -> Value {
    let mut args = json!({
        "board": concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../copperpour-core/tests/fixtures/placement_overview_kicad10/placement_overview.kicad_pcb"
        ),
    });
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let result = p.call_tool("get_placement_overview", args);
    assert_ne!(result["isError"], json!(true), "{result}");
    result
}

fn placement_process() -> McpProcess {
    let mut p = McpProcess::spawn();
    let loaded = p.call_tool("load_toolset", json!({"name": "placement"}));
    assert_ne!(loaded["isError"], json!(true), "{loaded}");
    p
}

fn assert_mm(actual: &Value, expected: f64, what: &str) {
    let actual = actual
        .as_f64()
        .unwrap_or_else(|| panic!("{what}: {actual}"));
    assert!(
        (actual - expected).abs() < 0.011,
        "{what}: {actual} != {expected}"
    );
}

#[test]
fn placement_overview_airwires_match_kicads_ratsnest() {
    let mut p = placement_process();
    let result = placement_overview(&mut p, json!({}));
    assert_eq!(result["content"].as_array().unwrap().len(), 1, "{result}");
    let report = McpProcess::tool_body(&result);

    assert_eq!(report["source"], "saved_file", "{report}");
    assert_eq!(report["footprints"], 9, "{report}");
    assert_eq!(report["pads"], 18, "{report}");
    assert_eq!(
        report["outline_bbox"],
        json!({"x_min": 10.0, "y_min": 10.0, "x_max": 70.0, "y_max": 50.0})
    );
    assert_eq!(report["outside_outline_count"], 1, "{report}");
    assert_eq!(report["outside_outline"], json!(["R9"]));

    let airwires = &report["airwires"];
    assert_mm(&airwires["total_length_mm"], 207.08, "total");
    assert_eq!(airwires["count"], 8, "{airwires}");
    assert_eq!(airwires["nets"], 6, "{airwires}");
    assert_eq!(airwires["crossings"], 4, "{airwires}");
    assert_eq!(
        airwires["skipped_nets"],
        json!([{"net": "GND", "pads": 4}]),
        "{airwires}"
    );

    // KiCad's ratsnest lines, longest first: net, the two pads, length.
    let kicad = [
        ("SIG_B", "R3.2", "R9.1", 61.22),
        ("/power/VCC", "R4.2", "R5.2", 30.59),
        ("SIG_B", "R3.2", "R4.1", 26.00),
        ("SIG_A", "R1.1", "R3.1", 21.54),
        ("SIG_A", "R1.1", "R2.1", 20.00),
        ("VCC", "R5.1", "R6.1", 20.00),
        ("/drv/HV_OUT", "R7.2", "R8.1", 15.09),
        ("HV", "R6.2", "R7.1", 12.63),
    ];
    let longest = airwires["longest"].as_array().unwrap();
    assert_eq!(longest.len(), kicad.len(), "{longest:#?}");
    let mut reported: Vec<(String, String, String, f64)> = longest
        .iter()
        .map(|wire| {
            let mut ends = [
                wire["from"].as_str().unwrap().to_string(),
                wire["to"].as_str().unwrap().to_string(),
            ];
            ends.sort();
            let [a, b] = ends;
            (
                wire["net"].as_str().unwrap().to_string(),
                a,
                b,
                wire["length_mm"].as_f64().unwrap(),
            )
        })
        .collect();
    for (net, a, b, length) in kicad {
        let at = reported
            .iter()
            .position(|w| w.0 == net && w.1 == a && w.2 == b)
            .unwrap_or_else(|| panic!("no airwire {net} {a}-{b} in {longest:#?}"));
        let wire = reported.remove(at);
        assert!((wire.3 - length).abs() < 0.011, "{net} {a}-{b}: {wire:?}");
    }
    let lengths: Vec<f64> = longest
        .iter()
        .map(|w| w["length_mm"].as_f64().unwrap())
        .collect();
    assert!(lengths.windows(2).all(|w| w[0] >= w[1]), "{lengths:?}");

    let nets: Vec<(&str, u64, f64)> = vec![
        ("SIG_B", 3, 87.23),
        ("SIG_A", 3, 41.54),
        ("/power/VCC", 2, 30.59),
        ("VCC", 2, 20.00),
        ("/drv/HV_OUT", 2, 15.09),
        ("HV", 2, 12.63),
    ];
    let longest_nets = airwires["longest_nets"].as_array().unwrap();
    assert_eq!(longest_nets.len(), nets.len(), "{longest_nets:#?}");
    for (entry, (net, pads, length)) in longest_nets.iter().zip(nets) {
        assert_eq!(entry["net"], net, "{longest_nets:#?}");
        assert_eq!(entry["pads"], pads, "{entry}");
        assert_mm(&entry["length_mm"], length, net);
    }

    let parts: Vec<(&str, u64, f64)> = vec![
        ("R3", 3, 108.77),
        ("R9", 1, 61.22),
        ("R4", 2, 56.60),
        ("R5", 2, 50.59),
        ("R1", 2, 41.54),
        ("R6", 2, 32.63),
        ("R7", 2, 27.72),
        ("R2", 1, 20.00),
        ("R8", 1, 15.09),
    ];
    let farthest = airwires["farthest_footprints"].as_array().unwrap();
    assert_eq!(farthest.len(), parts.len(), "{farthest:#?}");
    for (entry, (reference, count, length)) in farthest.iter().zip(parts) {
        assert_eq!(entry["reference"], reference, "{farthest:#?}");
        assert_eq!(entry["airwires"], count, "{entry}");
        assert_mm(&entry["length_mm"], length, reference);
        assert_mm(&entry["mean_mm"], length / count as f64, reference);
    }
}

/// The default groups are the project's netclasses, with KiCad's membership:
/// `VCC` and `HV*` do not name the sheet-local `/power/VCC` and
/// `/drv/HV_OUT`, `*VCC` names both VCC nets, and `VCC` is in two classes.
/// A pattern is a regular expression too: `SIG_[AB]`, `S.G_A` and
/// `/drv/HV_(OUT|IN)` name nets, and `IG_A` names none because the whole
/// name has to match.
#[test]
fn placement_overview_netclass_groups_follow_kicads_membership() {
    let mut p = placement_process();
    let report = McpProcess::tool_body(&placement_overview(&mut p, json!({})));
    assert_eq!(report["groups_source"], "netclasses", "{report}");
    let groups = report["groups"].as_array().unwrap();
    let summary: Vec<(&str, &Value, &Value)> = groups
        .iter()
        .map(|g| (g["name"].as_str().unwrap(), &g["nets"], &g["footprints"]))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("AnyChar", &json!(["SIG_A"]), &json!(["R1", "R2", "R3"])),
            ("Drive", &json!(["/drv/HV_OUT"]), &json!(["R7", "R8"])),
            ("HighVoltage", &json!(["HV"]), &json!(["R6", "R7"])),
            ("Power", &json!(["VCC"]), &json!(["R5", "R6"])),
            (
                "Rails",
                &json!(["/power/VCC", "VCC"]),
                &json!(["R4", "R5", "R6"])
            ),
            (
                "Signals",
                &json!(["SIG_A", "SIG_B"]),
                &json!(["R1", "R2", "R3", "R4", "R9"])
            ),
            ("Unanchored", &json!([]), &json!([])),
        ],
        "{groups:#?}"
    );

    // HighVoltage: pads R6.2 (60.8, 40) and R7.1 (49.2, 45).
    assert_eq!(groups[6]["region_bbox"], Value::Null, "{groups:#?}");
    let hv = &groups[2];
    assert_eq!(
        hv["region_bbox"],
        json!({"x_min": 49.2, "y_min": 40.0, "x_max": 60.8, "y_max": 45.0})
    );
    assert_eq!(hv["other_parts_inside_region_count"], 0, "{hv}");
    // 11.6 x 5 mm of the 60 x 40 mm board.
    assert_eq!(hv["region_share_of_board"], 0.02, "{hv}");
    // The nearest pads of other nets on other parts are two pairs at the
    // same 11.18 mm (10 mm across, 5 mm down): R7.1 to R6.1 and R6.2 to R7.2.
    let mut nearest: Vec<(&str, &str, &str)> = hv["closest_other_net_pads"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|c| {
            assert_mm(&c["distance_mm"], 11.18, "closest");
            assert_eq!(c["net"], "HV", "{c}");
            (
                c["pad"].as_str().unwrap(),
                c["other_pad"].as_str().unwrap(),
                c["other_net"].as_str().unwrap(),
            )
        })
        .collect();
    nearest.sort();
    assert_eq!(
        nearest,
        vec![("R6.2", "R7.2", "/drv/HV_OUT"), ("R7.1", "R6.1", "VCC")],
        "{hv}"
    );
}

/// A group the caller names matches a sheet-local net by its short name too,
/// which a netclass pattern does not.
#[test]
fn placement_overview_net_groups_also_match_the_short_name() {
    let mut p = placement_process();
    let report = McpProcess::tool_body(&placement_overview(
        &mut p,
        json!({"net_groups": {"Supply": ["VCC"], "Drive": ["HV_*"]}}),
    ));
    assert_eq!(report["groups_source"], "argument", "{report}");
    let groups = report["groups"].as_array().unwrap();
    let nets: Vec<(&str, &Value)> = groups
        .iter()
        .map(|g| (g["name"].as_str().unwrap(), &g["nets"]))
        .collect();
    assert_eq!(
        nets,
        vec![
            ("Drive", &json!(["/drv/HV_OUT"])),
            ("Supply", &json!(["/power/VCC", "VCC"])),
        ],
        "{groups:#?}"
    );
    // Drive spans R7.2 (50.8, 45) and R8.1 (49.2, 30); R8 and R7 carry it,
    // and no other part's courtyard centre lies in that strip.
    assert_eq!(groups[0]["footprints"], json!(["R7", "R8"]));
    assert_eq!(groups[0]["other_parts_inside_region_count"], 0);
}

#[test]
fn placement_overview_skip_nets_and_max_net_pads_choose_the_airwires() {
    let mut p = placement_process();

    // skip_nets replaces the ground default: GND's 74.14 mm come in, the
    // two SIG nets (41.54 + 87.23 mm) go out.
    let report =
        McpProcess::tool_body(&placement_overview(&mut p, json!({"skip_nets": ["SIG_*"]})));
    let airwires = &report["airwires"];
    assert_mm(
        &airwires["total_length_mm"],
        207.08 + 74.14 - 41.54 - 87.23,
        "total with skip_nets",
    );
    assert_eq!(airwires["count"], 7, "{airwires}");
    assert_eq!(
        airwires["skipped_nets"],
        json!([{"net": "SIG_A", "pads": 3}, {"net": "SIG_B", "pads": 3}]),
        "{airwires}"
    );

    // max_net_pads 2 leaves the four two-pad nets.
    let report = McpProcess::tool_body(&placement_overview(&mut p, json!({"max_net_pads": 2})));
    let airwires = &report["airwires"];
    assert_mm(
        &airwires["total_length_mm"],
        30.59 + 20.00 + 15.09 + 12.63,
        "total with max_net_pads",
    );
    assert_eq!(airwires["count"], 4, "{airwires}");
    assert_eq!(
        airwires["skipped_nets"],
        json!([
            {"net": "GND", "pads": 4},
            {"net": "SIG_A", "pads": 3},
            {"net": "SIG_B", "pads": 3},
        ]),
        "{airwires}"
    );

    // top bounds each ranked list and leaves the totals alone.
    let report = McpProcess::tool_body(&placement_overview(&mut p, json!({"top": 2})));
    let airwires = &report["airwires"];
    assert_eq!(airwires["longest"].as_array().unwrap().len(), 2);
    assert_eq!(airwires["longest_nets"].as_array().unwrap().len(), 2);
    assert_eq!(airwires["farthest_footprints"].as_array().unwrap().len(), 2);
    assert_eq!(airwires["count"], 8, "{airwires}");
}

#[test]
fn placement_overview_image_is_a_png_block_after_the_report() {
    let mut p = placement_process();
    let result = placement_overview(&mut p, json!({"image": true, "width_px": 400}));
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{result}");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(McpProcess::tool_body(&result)["airwires"]["count"], 8);
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    // Base64 of the eight-byte PNG signature.
    let data = content[1]["data"].as_str().unwrap();
    assert!(
        data.starts_with("iVBORw0KGgo"),
        "{}",
        &data[..data.len().min(40)]
    );
}

#[test]
fn placement_overview_refuses_bad_arguments_before_reading_the_board() {
    let mut p = placement_process();
    let board = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../copperpour-core/tests/fixtures/placement_overview_kicad10/placement_overview.kicad_pcb"
    );
    for arguments in [
        json!({}),
        json!({"board": board, "top": 0}),
        json!({"board": board, "max_net_pads": 1}),
        json!({"board": board, "net_groups": {"A": "VCC"}}),
        json!({"board": board, "no_such_argument": true}),
    ] {
        let result = p.call_tool("get_placement_overview", arguments.clone());
        assert_eq!(result["isError"], json!(true), "{arguments}: {result}");
        let error = McpProcess::tool_body(&result)["error"].clone();
        assert_eq!(error["kind"], "invalid_argument", "{arguments}: {error}");
    }
}

#[test]
fn closed_file_move_component_accepts_a_bare_filename() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../copperpour-core/tests/fixtures/specctra_two_resistors.kicad_pcb"
    );
    let board = dir.path().join("m.kicad_pcb");
    std::fs::copy(fixture, &board).unwrap();

    let mut p =
        McpProcess::spawn_with_env_without(Some(dir.path()), true, &[], &["KICAD_API_SOCKET"]);
    let load = p.call_tool("load_toolset", json!({"name": "pcb_components"}));
    assert_ne!(load["isError"], json!(true), "{load}");

    let moved = p.call_tool(
        "move_component",
        json!({"board": "m.kicad_pcb", "reference": "R1", "x": 120, "y": 95}),
    );
    assert_ne!(moved["isError"], json!(true), "{moved}");
    let body = McpProcess::tool_body(&moved);
    assert_eq!(body["moved"], "R1");
    assert_eq!(body["source"], "file");
    assert_eq!(body["x"], json!(120.0));
    assert_eq!(body["y"], json!(95.0));

    let disk = std::fs::read_to_string(&board).unwrap();
    assert!(
        disk.contains("(at 120 95"),
        "expected updated R1 coordinates on disk"
    );
}

#[test]
fn closed_file_sibling_writers_accept_a_bare_filename() {
    let dir = tempfile::tempdir().unwrap();
    let fixture_dir = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../copperpour-core/tests/fixtures/title_block_hierarchy"
    ));
    for name in [
        "complex_hierarchy.kicad_sch",
        "ampli_ht.kicad_sch",
        "complex_hierarchy.kicad_pro",
    ] {
        std::fs::copy(fixture_dir.join(name), dir.path().join(name)).unwrap();
    }

    let mut p =
        McpProcess::spawn_with_env_without(Some(dir.path()), true, &[], &["KICAD_API_SOCKET"]);
    let load_sch = p.call_tool("load_toolset", json!({"name": "sch_hierarchy"}));
    assert_ne!(load_sch["isError"], json!(true), "{load_sch}");
    let load_proj = p.call_tool("load_toolset", json!({"name": "project"}));
    assert_ne!(load_proj["isError"], json!(true), "{load_proj}");

    let tb = p.call_tool(
        "set_schematic_title_block",
        json!({
            "schematic": "complex_hierarchy.kicad_sch",
            "title": "Bare Filename Title",
            "scope": "hierarchy"
        }),
    );
    assert_ne!(tb["isError"], json!(true), "{tb}");
    for name in ["complex_hierarchy.kicad_sch", "ampli_ht.kicad_sch"] {
        let content = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert!(
            content.contains("(title \"Bare Filename Title\")"),
            "{name} missing updated title"
        );
    }

    let self_ref = p.call_tool(
        "add_hierarchical_sheet",
        json!({
            "schematic": "complex_hierarchy.kicad_sch",
            "sheet_name": "Loop",
            "sheet_file": "complex_hierarchy.kicad_sch",
            "x": 100.0,
            "y": 100.0
        }),
    );
    assert_eq!(self_ref["isError"], json!(true), "{self_ref}");
    assert!(
        self_ref["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("cannot reference its parent file"),
        "{self_ref}"
    );

    let rp = p.call_tool(
        "rename_project",
        json!({"project": "complex_hierarchy.kicad_pro", "new_name": "renamed"}),
    );
    assert_ne!(rp["isError"], json!(true), "{rp}");
    assert!(dir.path().join("renamed.kicad_pro").is_file());
    assert!(dir.path().join("renamed.kicad_sch").is_file());
    assert!(!dir.path().join("complex_hierarchy.kicad_pro").exists());
}
