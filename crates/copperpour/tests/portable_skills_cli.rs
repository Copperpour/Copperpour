//! Custom targets are hermetic on Windows too: no home-directory spoofing needed.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_copperpour"));
    command.env_remove("COPPERPOUR_SKILLS_DIR");
    command
}
fn run(action: &str, root: &Path) -> Output {
    command()
        .arg(action)
        .arg("--skills-dir")
        .arg(root)
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn portable_lifecycle_preserves_edits_unrelated_content_and_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills with spaces");
    success(run("init", &root));
    assert!(root.join("copperpour/SKILL.md").is_file());
    assert!(success(run("status", &root)).contains("current: copperpour/SKILL.md"));
    let edited = root.join("kicad-pcb/SKILL.md");
    fs::write(&edited, "local edits").unwrap();
    fs::write(root.join("copperpour/custom.md"), "mine").unwrap();
    fs::create_dir(root.join("other-client")).unwrap();
    fs::create_dir(root.join("kicad-library/references")).unwrap();
    assert!(success(run("status", &root)).contains("different: kicad-pcb/SKILL.md"));
    assert!(!run("init", &root).status.success());
    success(run("uninstall", &root));
    assert_eq!(fs::read_to_string(edited).unwrap(), "local edits");
    assert_eq!(
        fs::read_to_string(root.join("copperpour/custom.md")).unwrap(),
        "mine"
    );
    assert!(root.join("other-client").is_dir());
    assert!(root.join("kicad-library/references").is_dir());
    assert!(!root.join("copperpour/SKILL.md").exists());
    assert!(!root.join(".copperpour-skills.json").exists());
    assert!(root.is_dir());
    success(run("uninstall", &root));
}

#[test]
fn unmarked_and_malformed_targets_are_not_deleted_or_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("copperpour")).unwrap();
    let path = root.join("copperpour/SKILL.md");
    fs::write(&path, "mine").unwrap();
    assert!(!run("init", root).status.success());
    success(run("uninstall", root));
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    fs::write(
        root.join(".copperpour-skills.json"),
        r#"{"files":{"../outside":"mine"}}"#,
    )
    .unwrap();
    assert!(!run("uninstall", root).status.success());
    assert_eq!(fs::read_to_string(path).unwrap(), "mine");
}

#[test]
fn flag_precedes_env_and_status_never_creates_target() {
    let temp = tempfile::tempdir().unwrap();
    let env_root = temp.path().join("environment");
    let flag_root = temp.path().join("flag");
    success(
        command()
            .env("COPPERPOUR_SKILLS_DIR", &env_root)
            .arg("init")
            .output()
            .unwrap(),
    );
    assert!(env_root.join("copperpour/SKILL.md").exists());
    let status = success(
        command()
            .env("COPPERPOUR_SKILLS_DIR", &env_root)
            .args(["status", "--skills-dir"])
            .arg(&flag_root)
            .output()
            .unwrap(),
    );
    assert!(status.contains(&flag_root.display().to_string()));
    assert!(!flag_root.exists());
    success(
        command()
            .env("COPPERPOUR_SKILLS_DIR", &env_root)
            .arg("uninstall")
            .output()
            .unwrap(),
    );
    assert!(env_root.is_dir());
    assert_eq!(fs::read_dir(env_root).unwrap().count(), 0);
}

#[test]
fn invalid_explicit_targets_refuse_without_native_fallback() {
    let temp = tempfile::tempdir().unwrap();
    for action in ["init", "status", "uninstall"] {
        for args in [
            vec!["--skills-dir"],
            vec!["--skills-dir", ""],
            vec!["--skills-dir", "relative"],
            vec!["--skills-dir", "--client"],
            vec!["--typo"],
        ] {
            assert!(!command()
                .current_dir(temp.path())
                .arg(action)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        assert!(!command()
            .current_dir(temp.path())
            .arg(action)
            .args(["--skills-dir"])
            .arg(temp.path())
            .args(["--skills-dir"])
            .arg(temp.path())
            .output()
            .unwrap()
            .status
            .success());
        for value in ["", "relative"] {
            assert!(!command()
                .current_dir(temp.path())
                .env("COPPERPOUR_SKILLS_DIR", value)
                .arg(action)
                .output()
                .unwrap()
                .status
                .success());
        }
    }
    assert!(success(
        command()
            .current_dir(temp.path())
            .args(["init", "--skills-dir", "", "--help"])
            .output()
            .unwrap()
    )
    .contains("--skills-dir"));
}

#[test]
fn custom_target_does_not_write_client_profile() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("portable");
    // Native Windows home lookup ignores these; the explicit target must avoid
    // looking up a profile at all. Linux additionally verifies an empty fake home.
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    for action in ["init", "status", "uninstall"] {
        success(
            command()
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("COPPERPOUR_STATE_DIR", home.join("state"))
                .arg(action)
                .arg("--skills-dir")
                .arg(&root)
                .output()
                .unwrap(),
        );
    }
    assert_eq!(fs::read_dir(home).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_redirect_portable_operations() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    let external = temp.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("SKILL.md"), "precious").unwrap();
    fs::create_dir(&root).unwrap();
    symlink(&external, root.join("copperpour")).unwrap();
    for action in ["init", "status", "uninstall"] {
        assert!(!run(action, &root).status.success());
    }
    assert_eq!(
        fs::read_to_string(external.join("SKILL.md")).unwrap(),
        "precious"
    );
    fs::remove_file(root.join("copperpour")).unwrap();
    symlink(
        external.join("SKILL.md"),
        root.join(".copperpour-skills.json"),
    )
    .unwrap();
    assert!(!run("init", &root).status.success());
    assert_eq!(
        fs::read_to_string(external.join("SKILL.md")).unwrap(),
        "precious"
    );
}

#[test]
fn upgrades_recognize_previous_contents_and_unmarked_directories_remain() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("copperpour")).unwrap();
    success(run("uninstall", root));
    assert!(root.join("copperpour").is_dir());
    success(run("init", root));
    let marker = root.join(".copperpour-skills.json");
    let mut ownership: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    ownership["files"]["copperpour/SKILL.md"] = "previous version".into();
    fs::write(&marker, serde_json::to_vec(&ownership).unwrap()).unwrap();
    fs::write(root.join("copperpour/SKILL.md"), "previous version").unwrap();
    success(run("init", root));
    assert!(success(run("status", root)).contains("current: copperpour/SKILL.md"));
}

#[cfg(unix)]
#[test]
fn symlink_root_with_trailing_separator_is_refused() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let external = temp.path().join("external");
    let root = temp.path().join("link");
    fs::create_dir(&external).unwrap();
    symlink(&external, &root).unwrap();
    let trailing = format!("{}/", root.display());
    for action in ["init", "status", "uninstall"] {
        assert!(!run(action, Path::new(&trailing)).status.success());
    }
    assert_eq!(fs::read_dir(external).unwrap().count(), 0);
}
