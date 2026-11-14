//! Explicit, profile-independent installation of portable Markdown skills.
use crate::{install, manifest::SKILLS};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const MARKER: &str = ".copperpour-skills.json";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    // Keep the installed bytes so an upgrade can distinguish old bundled content
    // from local edits without assuming the running binary installed it.
    files: BTreeMap<String, String>,
}

pub fn run(command: &str, args: &[String]) -> Result<()> {
    let mut native_args = Vec::new();
    let mut explicit = None;
    let mut preset = None;
    let mut client_given = false;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--skills-dir" {
            if explicit.is_some() {
                bail!("--skills-dir may only be specified once");
            }
            explicit = Some(
                args.get(index + 1)
                    .context("--skills-dir requires an absolute path")?
                    .clone(),
            );
            index += 2;
        } else if args[index] == "--client" {
            let value = args.get(index + 1).context(
                "--client requires claude, codex, gemini, cursor, or antigravity (alias agy)",
            )?;
            preset = preset_directory(&value.to_ascii_lowercase());
            client_given = true;
            native_args.push(args[index].clone());
            // The legacy parser still validates duplicates and unknown arguments.
            // Presets always resolve a portable target or fail before native dispatch.
            native_args.push(if preset.is_some() {
                "codex".to_owned()
            } else {
                value.clone()
            });
            index += 2;
        } else {
            native_args.push(args[index].clone());
            index += 1;
        }
    }
    let client = install::client_from_args(&native_args).context(
        "installer clients: claude (Claude Code), codex, gemini, cursor, antigravity (alias agy)",
    )?;
    // An explicit flag beats the environment: `--skills-dir` always wins, and an
    // explicit `--client` ignores COPPERPOUR_SKILLS_DIR, so a variable exported
    // for one client never silently redirects an install meant for another.
    let from_env = explicit.is_none() && !client_given;
    let target = match explicit {
        Some(path) => Some(std::ffi::OsString::from(path)),
        None if from_env => std::env::var_os("COPPERPOUR_SKILLS_DIR"),
        None => None,
    };
    let env_selected = from_env && target.is_some();
    if let Some(root) = target_path(target, preset, copperpour_core::app_dirs::home_dir)? {
        if env_selected {
            println!("Target from COPPERPOUR_SKILLS_DIR: {}", root.display());
        }
        return run_at(command, &root);
    }
    match command {
        "init" => install::run_install(client),
        "status" => install::print_status(client),
        "uninstall" => install::run_uninstall(client),
        _ => unreachable!("installer subcommand"),
    }
}

/// Installer-only presets: MCP registration and server client selection are separate.
fn preset_directory(client: &str) -> Option<&'static str> {
    match client {
        "gemini" => Some(".gemini/skills"),
        "cursor" => Some(".cursor/skills"),
        "antigravity" | "agy" => Some(".gemini/config/skills"),
        _ => None,
    }
}

fn target_path(
    overridden: Option<std::ffi::OsString>,
    preset: Option<&str>,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    let target = match overridden {
        Some(path) => PathBuf::from(path),
        None => match preset {
            Some(relative) => home()
                .context(copperpour_core::app_dirs::NO_USER_DIR)?
                .join(relative),
            None => return Ok(None),
        },
    };
    let root: PathBuf = target.components().collect();
    if !root.is_absolute() {
        bail!("skills directory must be a nonempty absolute path");
    }
    Ok(Some(root))
}

fn bundled() -> BTreeMap<String, &'static str> {
    let mut files = BTreeMap::new();
    for skill in SKILLS {
        files.insert(format!("{}/SKILL.md", skill.name), skill.content);
        for (name, content) in skill.references {
            files.insert(format!("{}/references/{name}", skill.name), *content);
        }
    }
    files
}

// Never follow symlinks inside the target, including the ownership marker.
fn check_path(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            path.push(component);
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("refusing symlink {}", path.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Whether a marker key names a skill file this installer could have written:
/// `<skill>/SKILL.md` or `<skill>/references/<file>`, built only from plain names.
/// Keys need not be bundled by this binary: a release may rename or drop a file
/// (b33babc4 renamed `konnect/` to `copperpour/`), and the install it left behind
/// must stay upgradable and removable. Anything else is refused, never trusted
/// as a path to read or delete.
fn is_skill_file_key(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    let plain = |part: &str| {
        !part.is_empty()
            && !part.contains(['\\', ':'])
            && matches!(
                Path::new(part).components().collect::<Vec<_>>().as_slice(),
                [std::path::Component::Normal(only)] if only.to_str() == Some(part)
            )
    };
    parts.iter().all(|part| plain(part))
        && match parts.as_slice() {
            [_, file] => *file == "SKILL.md",
            [_, references, _] => *references == "references",
            _ => false,
        }
}

/// Remove `name` if it still holds exactly the bytes the marker recorded.
/// Returns whether the file is gone (removed or already missing).
fn remove_if_unchanged(root: &Path, name: &str, original: &str) -> Result<bool> {
    let path = root.join(name);
    match read_optional(&path)? {
        None => Ok(true),
        Some(bytes) if bytes == original.as_bytes() => {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            Ok(true)
        }
        Some(_) => Ok(false),
    }
}

/// Remove the now-empty `references/` and skill directories of `names`,
/// deepest first. Never removes the root or a directory that is not empty.
fn remove_empty_skill_dirs<'a>(
    root: &Path,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    let mut directories = std::collections::BTreeSet::new();
    for name in names {
        let mut parts = name.split('/');
        let skill = parts.next().unwrap_or_default();
        directories.insert(root.join(skill));
        if parts.next() == Some("references") {
            directories.insert(root.join(skill).join("references"));
        }
    }
    // Reverse lexical order visits `<skill>/references` before `<skill>`.
    for path in directories.into_iter().rev() {
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                return Err(error).with_context(|| format!("remove empty {}", path.display()))
            }
        }
    }
    Ok(())
}

fn write_marker(root: &Path, ownership: &Ownership) -> Result<()> {
    // One write per run, via rename, so a crash never leaves a torn marker.
    let temporary = root.join(format!("{MARKER}.tmp"));
    check_path(root, Path::new(&format!("{MARKER}.tmp")))?;
    fs::write(&temporary, serde_json::to_vec_pretty(ownership)?)
        .with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, root.join(MARKER))
        .with_context(|| format!("replace {}", root.join(MARKER).display()))
}

fn run_at(command: &str, root: &Path) -> Result<()> {
    let files = bundled();
    check_path(root, Path::new(MARKER))?;
    let mut ownership: Ownership = match read_optional(&root.join(MARKER))? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).context("invalid Copperpour skills ownership marker")?
        }
        None => Ownership::default(),
    };
    // A marker is data, never authority to access arbitrary paths.
    if let Some(name) = ownership.files.keys().find(|name| !is_skill_file_key(name)) {
        bail!(
            "ownership marker lists {name:?}, which is not a skill file path; preserve it and inspect manually"
        );
    }
    for name in ownership.files.keys() {
        check_path(root, Path::new(name))?;
    }
    // Files an earlier release installed that this binary no longer bundles.
    let retired: Vec<String> = ownership
        .files
        .keys()
        .filter(|name| !files.contains_key(*name))
        .cloned()
        .collect();
    // Preflight the complete catalog before mutating any file.
    for (name, content) in &files {
        check_path(root, Path::new(name))?;
        if command == "init" {
            if let Some(bytes) = read_optional(&root.join(name))? {
                // Only a file the marker already owns may be replaced or adopted:
                // identical bytes alone could belong to another installation.
                let owned = match ownership.files.get(name) {
                    Some(recorded) => bytes == content.as_bytes() || bytes == recorded.as_bytes(),
                    None => false,
                };
                if !owned {
                    bail!(
                        "refusing to overwrite locally edited or unowned file {}",
                        root.join(name).display()
                    );
                }
            }
        }
    }
    match command {
        "init" => {
            fs::create_dir_all(root)?;
            // Retire files an earlier release installed, unless edited locally.
            for name in &retired {
                if remove_if_unchanged(root, name, &ownership.files[name])? {
                    ownership.files.remove(name);
                    println!("Removed retired {name}");
                } else {
                    println!("Preserved edited retired file {name}");
                }
            }
            remove_empty_skill_dirs(root, &retired)?;
            // Record ownership before writing: a partial failure never grants
            // permission to remove a pre-existing file with different bytes.
            for (name, content) in &files {
                ownership.files.insert(name.clone(), (*content).to_owned());
            }
            write_marker(root, &ownership)?;
            for (name, content) in &files {
                let path = root.join(name);
                fs::create_dir_all(path.parent().context("skill parent")?)?;
                fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
            }
            println!(
                "Installed {} portable skills in {} (skills only).",
                SKILLS.len(),
                root.display()
            );
        }
        "status" => {
            println!("Portable skills: {} (skills only)", root.display());
            for (name, content) in &files {
                let state = match read_optional(&root.join(name))? {
                    None => "missing",
                    Some(bytes) if bytes == content.as_bytes() => "current",
                    Some(_) => "different",
                };
                println!("{state}: {name}");
            }
            for name in &retired {
                if root.join(name).exists() {
                    println!("retired (removed by init or uninstall): {name}");
                }
            }
            println!(
                "Ownership marker: {}",
                if root.join(MARKER).exists() {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        "uninstall" => {
            for (name, original) in &ownership.files {
                let existed = root.join(name).exists();
                if remove_if_unchanged(root, name, original)? {
                    if existed {
                        println!("Removed {name}");
                    }
                } else {
                    println!("Preserved edited file {name}");
                }
            }
            // Only empty skill directories the marker names may be removed.
            remove_empty_skill_dirs(root, ownership.files.keys())?;
            if root.join(MARKER).exists() {
                fs::remove_file(root.join(MARKER))?;
            }
            println!(
                "Portable uninstall complete; retained target {}.",
                root.display()
            );
        }
        _ => unreachable!("installer subcommand"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_under_home_and_never_fall_back_when_home_is_unknown() {
        let temp = tempfile::tempdir().unwrap();
        for (client, relative) in [
            ("gemini", ".gemini/skills"),
            ("cursor", ".cursor/skills"),
            ("antigravity", ".gemini/config/skills"),
            ("agy", ".gemini/config/skills"),
        ] {
            let preset = preset_directory(client);
            assert!(preset.is_some());
            assert_eq!(
                target_path(None, preset, || Some(temp.path().to_path_buf())).unwrap(),
                Some(temp.path().join(relative))
            );
            assert!(target_path(None, preset, || None).is_err());
            assert_eq!(
                target_path(Some(temp.path().as_os_str().to_owned()), preset, || panic!(
                    "override must not resolve home"
                ))
                .unwrap(),
                Some(temp.path().to_path_buf())
            );
        }
        assert_eq!(
            target_path(None, None, || panic!(
                "native target resolves its own paths"
            ))
            .unwrap(),
            None
        );
        assert!(target_path(Some("".into()), Some(".cursor/skills"), || None).is_err());
    }

    #[test]
    fn marker_keys_must_be_skill_file_paths_but_need_not_be_bundled() {
        for key in [
            "copperpour/SKILL.md",
            "konnect/SKILL.md",
            "kicad-pcb/references/retired.md",
        ] {
            assert!(is_skill_file_key(key), "{key}");
        }
        for key in [
            "../outside",
            "../x/SKILL.md",
            "./SKILL.md",
            "copperpour/../SKILL.md",
            "/etc/SKILL.md",
            "SKILL.md",
            "copperpour/notes.md",
            "copperpour/references/../../x",
            "copperpour/references/",
            "copperpour//SKILL.md",
            "copperpour/scripts/run.sh",
            "copperpour\\SKILL.md",
            "C:/SKILL.md",
            "C:x/SKILL.md",
            ".copperpour-skills.json",
        ] {
            assert!(!is_skill_file_key(key), "{key}");
        }
    }
}
