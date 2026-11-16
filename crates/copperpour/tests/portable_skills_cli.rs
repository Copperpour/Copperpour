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
    let installed = success(
        command()
            .env("COPPERPOUR_SKILLS_DIR", &env_root)
            .arg("init")
            .output()
            .unwrap(),
    );
    assert!(installed.contains("Target from COPPERPOUR_SKILLS_DIR"));
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
    assert!(!status.contains("COPPERPOUR_SKILLS_DIR"));
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

#[test]
fn named_presets_use_portable_ownership_and_honor_overrides() {
    let temp = tempfile::tempdir().unwrap();
    // Directories are named by index: NTFS is case-insensitive, so `GeMiNi`
    // and `gemini` would otherwise collide on Windows.
    for (index, client) in ["gemini", "cursor", "antigravity", "agy", "GeMiNi"]
        .into_iter()
        .enumerate()
    {
        let root = temp.path().join(format!("root-{index}"));
        let home = temp.path().join(format!("home-{index}"));
        fs::create_dir(&home).unwrap();
        let ignored = temp.path().join("ignored-env");
        for action in ["init", "status", "uninstall"] {
            let output = success(
                command()
                    .env("HOME", &home)
                    .env("USERPROFILE", &home)
                    .env("COPPERPOUR_STATE_DIR", home.join("state"))
                    .env("COPPERPOUR_SKILLS_DIR", &ignored)
                    .arg(action)
                    .args(["--client", client, "--skills-dir"])
                    .arg(&root)
                    .output()
                    .unwrap(),
            );
            assert!(output.contains(&root.display().to_string()));
            if action == "init" {
                assert!(root.join(".copperpour-skills.json").is_file());
                fs::write(root.join("copperpour/SKILL.md"), "keep edits").unwrap();
            }
        }
        assert_eq!(
            fs::read_to_string(root.join("copperpour/SKILL.md")).unwrap(),
            "keep edits"
        );
        assert_eq!(fs::read_dir(home).unwrap().count(), 0);
        assert!(!ignored.exists());
    }
}

#[test]
fn preset_arguments_still_refuse_duplicates_unknown_clients_and_bad_paths() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["--client", "gemini", "--client", "cursor"],
        vec!["--client", "cursor", "--client", "codex"],
        vec!["--client", "unknown"],
        vec!["--client"],
        vec!["--client", "agy", "--typo"],
        vec!["--client", "agy", "--skills-dir", ""],
    ] {
        assert!(!command()
            .arg("init")
            .env("COPPERPOUR_SKILLS_DIR", temp.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[cfg(not(windows))]
#[test]
fn presets_install_to_documented_home_directories() {
    // Windows ignores HOME for native home resolution. Deterministic unit tests
    // cover the same resolver there; don't risk its real client directories.
    let temp = tempfile::tempdir().unwrap();
    for (client, relative) in [
        ("gemini", ".gemini/skills"),
        ("cursor", ".cursor/skills"),
        ("antigravity", ".gemini/config/skills"),
        ("agy", ".gemini/config/skills"),
    ] {
        let home = temp.path().join(client);
        fs::create_dir(&home).unwrap();
        let ignored = temp.path().join(format!("ignored-env-{client}"));
        for action in ["init", "status", "uninstall"] {
            success(
                command()
                    .env("HOME", &home)
                    .env("COPPERPOUR_STATE_DIR", home.join("state"))
                    // An explicit --client wins over the environment.
                    .env("COPPERPOUR_SKILLS_DIR", &ignored)
                    .args([action, "--client", client])
                    .output()
                    .unwrap(),
            );
            if action == "init" {
                assert!(home
                    .join(relative)
                    .join(".copperpour-skills.json")
                    .is_file());
            }
        }
        assert!(home.join(relative).is_dir());
        assert!(!ignored.exists());
        assert!(!home.join(".claude").exists());
        assert!(!home.join("state").exists());
    }
}

#[cfg(not(windows))]
#[test]
fn explicit_native_client_ignores_skills_dir_environment() {
    // `status` writes nothing, and HOME keeps the native lookup hermetic.
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let ignored = temp.path().join("cursor-skills");
    for client in ["claude", "codex"] {
        let output = success(
            command()
                .env("HOME", &home)
                .env("COPPERPOUR_STATE_DIR", home.join("state"))
                .env("COPPERPOUR_SKILLS_DIR", &ignored)
                .args(["status", "--client", client])
                .output()
                .unwrap(),
        );
        assert!(!output.contains("Portable skills"), "{output}");
        assert!(!output.contains(&ignored.display().to_string()), "{output}");
    }
    assert!(!ignored.exists());
}

#[test]
fn identical_unowned_files_are_not_adopted() {
    // Bytes equal to the bundle can still belong to another installation, such
    // as a native Codex install in ~/.agents/skills; a portable uninstall
    // must never remove them.
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    success(run("init", &source));
    let root = temp.path().join("other");
    fs::create_dir_all(root.join("copperpour")).unwrap();
    fs::copy(
        source.join("copperpour/SKILL.md"),
        root.join("copperpour/SKILL.md"),
    )
    .unwrap();
    assert!(!run("init", &root).status.success());
    assert!(!root.join(".copperpour-skills.json").exists());
    success(run("uninstall", &root));
    assert!(root.join("copperpour/SKILL.md").is_file());
}

#[test]
fn files_retired_by_a_newer_release_stay_upgradable_and_removable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    success(run("init", root));
    // An earlier release installed `konnect/` (renamed to `copperpour/` in
    // b33babc4) and one reference this binary no longer bundles.
    let marker = root.join(".copperpour-skills.json");
    let mut ownership: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    for (name, content) in [
        ("konnect/SKILL.md", "old konnect skill"),
        ("konnect/references/old.md", "old reference"),
        ("kicad-pcb/references/dropped.md", "dropped reference"),
    ] {
        ownership["files"][name] = content.into();
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fs::write(&marker, serde_json::to_vec(&ownership).unwrap()).unwrap();
    // An edited retired file is preserved, never deleted.
    fs::write(root.join("kicad-pcb/references/dropped.md"), "my edits").unwrap();

    let status = success(run("status", root));
    assert!(status.contains("retired (removed by init or uninstall): konnect/SKILL.md"));
    let init = success(run("init", root));
    assert!(init.contains("Removed retired konnect/SKILL.md"), "{init}");
    assert!(!root.join("konnect").exists());
    assert_eq!(
        fs::read_to_string(root.join("kicad-pcb/references/dropped.md")).unwrap(),
        "my edits"
    );
    let ownership: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert!(ownership["files"].get("konnect/SKILL.md").is_none());
    assert!(ownership["files"]
        .get("kicad-pcb/references/dropped.md")
        .is_some());

    // A marker written before the upgrade is removable as well.
    let mut ownership = ownership;
    ownership["files"]["konnect/SKILL.md"] = "old konnect skill".into();
    fs::create_dir_all(root.join("konnect")).unwrap();
    fs::write(root.join("konnect/SKILL.md"), "old konnect skill").unwrap();
    fs::write(&marker, serde_json::to_vec(&ownership).unwrap()).unwrap();
    success(run("uninstall", root));
    assert!(!root.join("konnect").exists());
    assert!(!root.join("copperpour").exists());
    assert_eq!(
        fs::read_to_string(root.join("kicad-pcb/references/dropped.md")).unwrap(),
        "my edits"
    );
    assert!(!marker.exists());
}
