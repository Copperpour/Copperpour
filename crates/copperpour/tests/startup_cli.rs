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
    let mut command = Command::new(env!("CARGO_BIN_EXE_copperpour"));
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
