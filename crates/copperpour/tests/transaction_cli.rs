use std::process::Command;

fn konnect() -> Command {
    Command::new(env!("CARGO_BIN_EXE_copperpour"))
}

#[test]
fn transaction_help_is_advertised() {
    let output = konnect().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("transaction status <project-dir>"));
    assert!(stdout.contains("transaction recover <project-dir>"));
    assert!(stdout.contains("transaction abandon <project-dir> <id> --force"));
}

#[test]
fn client_scoped_installer_help_is_advertised() {
    let output = konnect().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("init [--client <client>]"));
    assert!(stdout.contains("claude (default)"));
    assert!(stdout.contains("codex"));
    assert!(stdout.contains("~/.agents/skills"));
}

#[test]
fn claude_pretooluse_hook_emits_only_structured_json() {
    let output = konnect().args(["hook", "pre-pcb-ipc"]).output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .is_some_and(|message| message.contains("live-IPC-only")));
    assert_eq!(value.as_object().unwrap().len(), 1);
}

#[test]
fn malformed_journal_requires_force_and_can_be_abandoned() {
    let project = tempfile::tempdir().unwrap();
    let active = project
        .path()
        .join(".konnect-transaction-malformed-fixture.json");
    std::fs::write(&active, "not json").unwrap();

    let refused = konnect()
        .args([
            "transaction",
            "abandon",
            project.path().to_str().unwrap(),
            "malformed-fixture",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(active.exists());

    let abandoned = konnect()
        .args([
            "transaction",
            "abandon",
            project.path().to_str().unwrap(),
            "malformed-fixture",
            "--force",
        ])
        .output()
        .unwrap();

    assert!(abandoned.status.success(), "{abandoned:?}");
    assert!(!active.exists());
    assert!(project
        .path()
        .join(".konnect-transaction-malformed-fixture.abandoned.json")
        .exists());
    let stdout = String::from_utf8(abandoned.stdout).unwrap();
    assert!(stdout.contains("without modifying target files"));
    assert!(stdout.contains("complete before/after schematic images"));
}
