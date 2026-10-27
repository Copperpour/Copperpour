//! The canonical design-state hash: one identity for "the design as it
//! stands", shared by release gates, evidence bundles, and gate-gated tags.
//!
//! Two rules make it portable, and both exist because Windows is the primary
//! development platform:
//!
//! 1. **Line endings are normalized to LF before hashing.** git's autocrlf
//!    can rewrite CRLF<->LF on checkout, so the same design would otherwise
//!    hash differently across machines — and a gate result recorded on one
//!    machine could never be matched on another (the tag gate would refuse
//!    forever).
//! 2. **Files are hashed in sorted relative-path order**, each contribution
//!    framed as `path\0content\0`, so directory iteration order and file
//!    concatenation ambiguity cannot change the digest.
//!
//! The design set is every file matching the design suffixes, discovered
//! recursively, excluding anything under `.git` and KiCad backup/autosave
//! artifacts. `.copperpour/` state (`.konnect/` before the rename) is
//! deliberately NOT part of the design hash: gate results must be able to
//! reference the design they evaluated without the reference changing the
//! referent.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// File suffixes that constitute the design state.
pub const DESIGN_SUFFIXES: &[&str] = &[".kicad_pro", ".kicad_sch", ".kicad_pcb", ".kicad_dru"];

/// Compute the canonical design-state hash of a project directory.
///
/// Returns the lowercase hex SHA-256 and the sorted list of relative paths
/// it covered — callers persist both, so a later mismatch can name what
/// changed instead of only that something did.
pub fn design_state_hash(project_dir: &Path) -> Result<(String, Vec<String>)> {
    let mut files = collect_design_files(project_dir)?;
    files.sort();

    let mut hasher = Sha256::new();
    for relative in &files {
        let content = std::fs::read(project_dir.join(relative))
            .with_context(|| format!("reading {relative}"))?;
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(normalize_eol(&content));
        hasher.update([0]);
    }
    Ok((format!("{:x}", hasher.finalize()), files))
}

fn collect_design_files(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                // .git is history, .copperpour (.konnect before the rename) is
                // derived state, *-backups is KiCad's own archive directory —
                // none are the design.
                if ![".git", ".copperpour", ".konnect"].contains(&name.as_str())
                    && !name.ends_with("-backups")
                {
                    stack.push(path);
                }
                continue;
            }
            let is_design = DESIGN_SUFFIXES.iter().any(|s| name.ends_with(s));
            // KiCad autosave/lock artifacts are session state, not design.
            let is_artifact = name.starts_with("_autosave-") || name.ends_with(".lck");
            if is_design && !is_artifact {
                out.push(relative_of(root, &path)?);
            }
        }
    }
    Ok(out)
}

fn relative_of(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .context("walked file outside the root")?;
    // Forward slashes so the same tree hashes identically on every OS.
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// CRLF and lone CR both become LF. Applied to every file: the design
/// formats are all text, and a hash that depended on the checkout's line
/// endings would not be an identity.
fn normalize_eol(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    let mut bytes = content.iter().peekable();
    while let Some(&b) = bytes.next() {
        if b == b'\r' {
            if bytes.peek() == Some(&&b'\n') {
                bytes.next();
            }
            out.push(b'\n');
        } else {
            out.push(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &[u8]) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// The CRLF vector: identical content under either line-ending
    /// convention must produce one hash — the property that keeps gate
    /// results matchable across machines and checkouts.
    #[test]
    fn crlf_and_lf_encodings_hash_identically() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write(a.path(), "amp.kicad_sch", b"(kicad_sch\n\t(version 1)\n)\n");
        write(
            b.path(),
            "amp.kicad_sch",
            b"(kicad_sch\r\n\t(version 1)\r\n)\r\n",
        );

        let (hash_a, _) = design_state_hash(a.path()).unwrap();
        let (hash_b, _) = design_state_hash(b.path()).unwrap();
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn content_changes_change_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "amp.kicad_sch", b"(kicad_sch)\n");
        let (before, files) = design_state_hash(dir.path()).unwrap();
        assert_eq!(files, vec!["amp.kicad_sch"]);

        write(dir.path(), "amp.kicad_sch", b"(kicad_sch (junk))\n");
        let (after, _) = design_state_hash(dir.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn discovery_order_cannot_matter_because_paths_are_sorted_and_framed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "b.kicad_sch", b"bb");
        write(dir.path(), "a.kicad_sch", b"aa");
        let (_, files) = design_state_hash(dir.path()).unwrap();
        assert_eq!(files, vec!["a.kicad_sch", "b.kicad_sch"]);

        // Framing: moving a byte across the file boundary changes the hash.
        let x = tempfile::tempdir().unwrap();
        write(x.path(), "a.kicad_sch", b"a");
        write(x.path(), "b.kicad_sch", b"abb");
        let (moved, _) = design_state_hash(x.path()).unwrap();
        let (original, _) = design_state_hash(dir.path()).unwrap();
        assert_ne!(moved, original);
    }

    #[test]
    fn derived_state_and_artifacts_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "amp.kicad_pcb", b"board");
        let (baseline, files) = design_state_hash(dir.path()).unwrap();
        assert_eq!(files, vec!["amp.kicad_pcb"]);

        write(dir.path(), ".copperpour/gates/abc.json", b"{}");
        // A design file is the only kind the exclusion has to keep out.
        write(dir.path(), ".copperpour/state/amp.kicad_pcb", b"copy");
        write(dir.path(), ".konnect/state/amp.kicad_pcb", b"copy");
        write(dir.path(), ".git/config", b"x");
        write(dir.path(), "amp-backups/amp.kicad_pcb", b"old");
        write(dir.path(), "_autosave-amp.kicad_pcb", b"tmp");
        write(dir.path(), "amp.kicad_pcb.lck", b"lock");
        write(dir.path(), "notes.txt", b"not design");
        let (after, files) = design_state_hash(dir.path()).unwrap();
        assert_eq!(files, vec!["amp.kicad_pcb"]);
        assert_eq!(
            baseline, after,
            "non-design files must not shift the identity"
        );
    }

    #[test]
    fn nested_sheets_use_forward_slash_paths() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sub/sheet.kicad_sch", b"s");
        let (_, files) = design_state_hash(dir.path()).unwrap();
        assert_eq!(files, vec!["sub/sheet.kicad_sch"]);
    }
}
