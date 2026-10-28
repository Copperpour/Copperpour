//! Project-scoped git checkpointing on pure-Rust gix.
//!
//! This crate exists so the vcs toolset's safety policy has one home:
//! operations are scoped to KiCAD design files, destructive operations
//! re-verify the repository root immediately before acting, and restores go
//! through a recovery branch so nothing is ever lost. The full policy lands
//! with the toolset (port plan T2.1); this scaffold proves the gix
//! dependency links and carries the first API the spike will grow.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Design-file globs a Konnect repository tracks. Everything else — backups,
/// fab output, cache libraries — is ignored by the vcs_init .gitignore.
pub const DESIGN_FILE_SUFFIXES: &[&str] = &[".kicad_pro", ".kicad_sch", ".kicad_pcb", ".kicad_dru"];

/// Open an existing repository whose work-dir root IS `project_dir`.
///
/// Refuses a repository discovered above the project directory: operating in
/// a parent repo would scope commits to someone else's history.
pub fn open_project_repo(project_dir: &Path) -> Result<gix::Repository> {
    let repo = gix::discover(project_dir)
        .with_context(|| format!("no git repository at {}", project_dir.display()))?;
    let workdir = repo
        .workdir()
        .context("repository has no working directory (bare?)")?
        .to_path_buf();
    let canonical_project = canonical(project_dir)?;
    let canonical_workdir = canonical(&workdir)?;
    if canonical_project != canonical_workdir {
        anyhow::bail!(
            "repository root {} is not the project directory {} — refusing to \
             operate in an enclosing repository",
            canonical_workdir.display(),
            canonical_project.display()
        );
    }
    Ok(repo)
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("cannot canonicalize {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_an_enclosing_repository() {
        let outer = tempfile::tempdir().unwrap();
        gix::init(outer.path()).unwrap();
        let project = outer.path().join("boards").join("amp");
        std::fs::create_dir_all(&project).unwrap();

        let err = open_project_repo(&project).unwrap_err().to_string();
        assert!(
            err.contains("enclosing repository"),
            "a parent repo must be refused, got: {err}"
        );
    }

    #[test]
    fn opens_a_repo_rooted_at_the_project() {
        let dir = tempfile::tempdir().unwrap();
        gix::init(dir.path()).unwrap();
        let repo = open_project_repo(dir.path()).unwrap();
        assert!(repo.workdir().is_some());
    }

    #[test]
    fn missing_repo_is_an_error_not_a_creation() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            open_project_repo(dir.path()).is_err(),
            "open must never create — reads never create sidecars"
        );
        assert!(
            !dir.path().join(".git").exists(),
            "no .git may appear from an open"
        );
    }
}
