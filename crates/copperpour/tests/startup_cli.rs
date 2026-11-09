//! Guidance-install invariants for real CLI process startup.
//!
//! Windows is intentionally excluded: `dirs::home_dir()` uses
//! `SHGetKnownFolderPath`, so overriding `HOME`/`USERPROFILE` would still risk
//! modifying the test runner's real client configuration.

#![cfg(not(target_os = "windows"))]

use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn copperpour(home: &Path) -> Command {
    copperpour_at(Path::new(env!("CARGO_BIN_EXE_copperpour")), home)
}

fn copperpour_at(exe: &Path, home: &Path) -> Command {
    let mut command = Command::new(exe);
    command.env("HOME", home);
    command.env("XDG_CONFIG_HOME", home.join(".config"));
    command
}

fn guidance_snapshot(home: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn visit(root: &Path, dir: &Path, entries: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        let mut children: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                entries.push((relative, None));
                visit(root, &path, entries);
            } else {
                entries.push((relative, Some(fs::read(path).unwrap())));
            }
        }
    }

    let mut entries = Vec::new();
    for name in [".claude", ".agents"] {
        let path = home.join(name);
        if path.exists() {
            entries.push((PathBuf::from(name), None));
            visit(home, &path, &mut entries);
        }
    }
    for name in [
        ".copperpour/.installed",
        ".copperpour/.installed-claude",
        ".copperpour/.installed-codex",
    ] {
        let path = home.join(name);
        if path.exists() {
            entries.push((PathBuf::from(name), Some(fs::read(path).unwrap())));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn start_and_initialize_server(home: &Path, client: &str) {
    let mut child = copperpour(home)
        .args(["--client", client])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "startup-test", "version": "0"}
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut response)
        .unwrap();
    assert!(response.contains("\"serverInfo\""), "{response}");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn mcp_start_does_not_install_into_a_clean_home() {
    for client in ["claude", "codex"] {
        let temp = tempfile::tempdir().unwrap();
        let before = guidance_snapshot(temp.path());

        start_and_initialize_server(temp.path(), client);

        assert_eq!(guidance_snapshot(temp.path()), before, "client: {client}");
    }
}

#[test]
fn mcp_start_does_not_reverse_an_explicit_uninstall() {
    for client in ["claude", "codex"] {
        let temp = tempfile::tempdir().unwrap();

        assert!(copperpour(temp.path())
            .args(["init", "--client", client])
            .status()
            .unwrap()
            .success());
        assert!(copperpour(temp.path())
            .args(["uninstall", "--client", client])
            .status()
            .unwrap()
            .success());

        let before = guidance_snapshot(temp.path());
        start_and_initialize_server(temp.path(), client);
        assert_eq!(guidance_snapshot(temp.path()), before, "client: {client}");
    }
}

/// `konnect`, the name before the rename, runs `copperpour` with the same
/// arguments and says so on stderr only.
#[test]
fn the_old_name_runs_copperpour() {
    let output = Command::new(env!("CARGO_BIN_EXE_konnect"))
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("copperpour {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("now called copperpour"));
}

/// #400: `install` run through the multi-platform PCM package's launcher
/// registers `bin/copperpour`, not the server it execs, and replaces the hooks
/// an earlier install wrote for that server.
#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), target_os = "macos"))]
#[test]
fn install_through_the_pcm_launcher_registers_the_launcher() {
    use std::os::unix::fs::PermissionsExt;

    // Beside the binary, so a hard link can stand in for a copy.
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let home = temp.path().join("home");
    // The launcher resolves its directory with `cd -P`.
    let plugins = temp.path().canonicalize().unwrap().join("plugins");
    // `PCM_SERVER_DIR` in install.rs, which this crate's tests cannot import.
    let server_dir = if cfg!(target_os = "macos") {
        "bin-macos"
    } else {
        "bin-linux-x86_64"
    };
    let server = plugins.join(server_dir).join("copperpour");
    let launcher = plugins.join("bin").join("copperpour");
    fs::create_dir_all(server.parent().unwrap()).unwrap();
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    let binary = env!("CARGO_BIN_EXE_copperpour");
    fs::hard_link(binary, &server)
        .or_else(|_| fs::copy(binary, &server).map(drop))
        .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/pcm-launcher.sh"),
        &launcher,
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();

    let commands = || -> Vec<String> {
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap())
                .unwrap();
        let mut commands: Vec<String> = settings["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().unwrap().clone())
            .map(|handler| handler["command"].as_str().unwrap().to_string())
            .collect();
        commands.sort();
        commands
    };
    // What an install through the launcher wrote before the fix.
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/settings.json"),
        json!({"hooks": {"PreToolUse": [{
            "matcher": "x",
            "hooks": [{
                "type": "command",
                "command": format!("'{}' hook 'pre-pcb-ipc'", server.display())
            }]
        }]}})
        .to_string(),
    )
    .unwrap();
    // A test spawning a process while the launcher was being written may hold
    // its fd until exec, so the first exec can fail with ETXTBSY (errno 26).
    let status = (1..=5)
        .find_map(|attempt| {
            match copperpour_at(&launcher, &home)
                .args(["init", "--client", "claude"])
                .stdout(Stdio::null())
                .status()
            {
                Err(error) if error.raw_os_error() == Some(26) && attempt < 5 => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
                result => Some(result),
            }
        })
        .unwrap();
    assert!(status.unwrap().success());

    let launcher_prefix = format!("'{}' hook ", launcher.display());
    let after = commands();
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|c| c.starts_with(&launcher_prefix)),
        "{after:#?}"
    );
    assert!(after.windows(2).all(|w| w[0] != w[1]), "each hook once");
}
