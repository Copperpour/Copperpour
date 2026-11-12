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
        } else {
            native_args.push(args[index].clone());
            index += 1;
        }
    }
    let client = install::client_from_args(&native_args)?;
    let target = explicit
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("COPPERPOUR_SKILLS_DIR"));
    if let Some(target) = target {
        let root: PathBuf = PathBuf::from(target).components().collect();
        if !root.is_absolute() {
            bail!("skills directory must be a nonempty absolute path");
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
    if ownership.files.keys().any(|name| !files.contains_key(name)) {
        bail!("ownership marker contains unknown files; preserve it and inspect manually");
    }
    // Preflight the complete catalog before mutating any file.
    for (name, content) in &files {
        check_path(root, Path::new(name))?;
        if command == "init" {
            if let Some(bytes) = read_optional(&root.join(name))? {
                let expected = ownership.files.get(name).map(String::as_bytes);
                if bytes != content.as_bytes() && expected != Some(bytes.as_slice()) {
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
            for (name, content) in &files {
                let path = root.join(name);
                fs::create_dir_all(path.parent().context("skill parent")?)?;
                // Record ownership before writing: a partial failure never grants
                // permission to remove a pre-existing file with different bytes.
                ownership.files.insert(name.clone(), (*content).to_owned());
                fs::write(root.join(MARKER), serde_json::to_vec_pretty(&ownership)?)?;
                fs::write(path, content)?;
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
                let path = root.join(name);
                if read_optional(&path)?.as_deref() == Some(original.as_bytes()) {
                    fs::remove_file(&path)?;
                    println!("Removed {name}");
                } else if path.exists() {
                    println!("Preserved edited file {name}");
                }
            }
            // Only empty known directories may be removed. Never remove root.
            for skill in SKILLS {
                if !ownership
                    .files
                    .keys()
                    .any(|name| name.starts_with(&format!("{}/", skill.name)))
                {
                    continue;
                }
                let mut directories = vec![root.join(skill.name)];
                if ownership
                    .files
                    .keys()
                    .any(|name| name.starts_with(&format!("{}/references/", skill.name)))
                {
                    directories.insert(0, root.join(skill.name).join("references"));
                }
                for path in directories {
                    match fs::remove_dir(&path) {
                        Ok(()) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::DirectoryNotEmpty
                            ) => {}
                        Err(error) => {
                            return Err(error)
                                .with_context(|| format!("remove empty {}", path.display()))
                        }
                    }
                }
            }
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
