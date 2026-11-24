//! `cargo xtask collect-api-migrations <version>` moves the unreleased notes in
//! `docs/api-migrations/` into `docs/API_MIGRATIONS.md`, under a `## <version>`
//! heading above the previous release.
//!
//! Why this exists: every PR used to add its note at the top of
//! `API_MIGRATIONS.md`, so any two open PRs conflicted there even when their
//! code merged cleanly. Each PR now adds a file of its own instead.

use crate::markdown_links;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const NOTES_DIR: &str = "docs/api-migrations";
pub const MIGRATIONS: &str = "docs/API_MIGRATIONS.md";

pub struct Note {
    pub path: PathBuf,
    pub text: String,
}

/// The notes waiting for a release, by file name. The directory's README is
/// not a note.
pub fn notes(root: &Path) -> Result<Vec<Note>, String> {
    let dir = root.join(NOTES_DIR);
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "md")
                && path.file_name().is_some_and(|name| name != "README.md")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            check(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Note { path, text })
        })
        .collect()
}

/// A note is a `### ` heading on its first line, then text. A `#`, `##` or
/// second `###` heading would split the release section it is folded into.
pub fn check(text: &str) -> Result<(), String> {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.starts_with("### ") && !first[4..].trim().is_empty() => {}
        _ => return Err("must start with a `### ` heading".into()),
    }
    let mut fenced = false;
    let mut body = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced && ["# ", "## ", "### "].iter().any(|h| line.starts_with(h)) {
            return Err(format!(
                "only the first line may be a `###` heading: {line}"
            ));
        }
        body |= !line.trim().is_empty();
    }
    if !body {
        return Err("has a heading but no text".into());
    }
    Ok(())
}

/// `vMAJOR.MINOR.PATCH`, as the release tags are spelled.
pub fn is_version(version: &str) -> bool {
    let Some(numbers) = version.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// `doc` with a `## <version>` section of `notes` inserted above the newest
/// release, or appended when there is none yet. Keeps CRLF if `doc` has it.
pub fn fold(doc: &str, version: &str, notes: &[&str]) -> Result<String, String> {
    let heading = format!("## {version}");
    if doc.lines().any(|line| line.trim_end() == heading) {
        return Err(format!("{MIGRATIONS} already has `{heading}`"));
    }
    let mut lines = vec![heading, String::new()];
    for note in notes {
        lines.extend(note.trim_end().lines().map(str::to_string));
        lines.push(String::new());
    }
    let sep = if doc.contains("\r\n") { "\r\n" } else { "\n" };
    let section = lines.join(sep);
    Ok(match doc.find("\n## ") {
        Some(at) => format!("{}{section}{sep}{}", &doc[..at + 1], &doc[at + 1..]),
        None => format!("{}{sep}{sep}{}{sep}", doc.trim_end(), section.trim_end()),
    })
}

/// Adjusts relative links inside a note that is moving one directory up from
/// `docs/api-migrations/<note>.md` into `docs/API_MIGRATIONS.md`.
///
/// - `../<rest>` becomes `<rest>` when it resolves from `docs/API_MIGRATIONS.md`.
/// - Sibling links to non-removed files in `docs/api-migrations/` (such as
///   `README.md`) become `api-migrations/<target>`.
pub fn rewrite_note_links_for_fold(
    text: &str,
    note_path: &Path,
    migrations_path: &Path,
    removed: &BTreeSet<PathBuf>,
) -> String {
    let mut out = text.to_string();
    let mut links = markdown_links::relative_links(text);
    links.sort_by_key(|b| std::cmp::Reverse(b.span.start));
    for link in links {
        let (wrapped, inner) = match link
            .target
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
        {
            Some(inner) => (true, inner),
            None => (false, link.target.as_str()),
        };
        let replacement = if let (Some(rest_target), Some(rest_path)) =
            (inner.strip_prefix("../"), link.path.strip_prefix("../"))
        {
            markdown_links::resolve(migrations_path, rest_path)
                .exists()
                .then(|| rest_target.to_string())
        } else {
            let orig_resolved = markdown_links::resolve(note_path, &link.path);
            let shifted_target = format!("api-migrations/{inner}");
            let shifted_path = format!("api-migrations/{}", link.path);
            (!removed.contains(&orig_resolved)
                && orig_resolved.exists()
                && markdown_links::resolve(migrations_path, &shifted_path) == orig_resolved)
                .then_some(shifted_target)
        };
        if let Some(new_inner) = replacement {
            let new_target = if wrapped {
                format!("<{new_inner}>")
            } else {
                new_inner
            };
            out.replace_range(link.span, &new_target);
        }
    }
    out
}

/// Checks for links that would break once `notes` are folded into
/// `docs/API_MIGRATIONS.md` and removed from `docs/api-migrations/`.
pub fn find_dangling_links_after_collect(
    root: &Path,
    notes: &[Note],
    rewritten_notes: &[(&Path, String)],
) -> Vec<String> {
    let removed: BTreeSet<PathBuf> = notes
        .iter()
        .map(|n| markdown_links::normalize(&n.path))
        .collect();
    let migrations_path = root.join(MIGRATIONS);
    let mut issues = Vec::new();

    for file in markdown_links::markdown_files(root) {
        let normalized_file = markdown_links::normalize(&file);
        if removed.contains(&normalized_file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel_file = markdown_links::rel_display(root, &file);
        for link in markdown_links::relative_links(&text) {
            let resolved = markdown_links::resolve(&file, &link.path);
            if removed.contains(&resolved) {
                issues.push(format!(
                    "{rel_file}:{}: links to removed note `{}` — point it at `{MIGRATIONS}#<section>` first",
                    link.line, link.target
                ));
            }
        }
    }

    for (note_path, rewritten) in rewritten_notes {
        let rel_note = markdown_links::rel_display(root, note_path);
        for link in markdown_links::relative_links(rewritten) {
            let resolved = markdown_links::resolve(&migrations_path, &link.path);
            if removed.contains(&resolved) || !resolved.exists() {
                issues.push(format!(
                    "{rel_note}:{}: link `{}` would break once folded into `{MIGRATIONS}`",
                    link.line, link.target
                ));
            }
        }
    }

    issues
}

/// Folds unreleased notes under `root` into `docs/API_MIGRATIONS.md`, refusing
/// before modifying any file if a link would be left dangling.
pub fn collect_in(root: &Path, version: &str) -> Result<usize, String> {
    let notes = notes(root)?;
    if notes.is_empty() {
        return Ok(0);
    }
    let path = root.join(MIGRATIONS);
    let removed: BTreeSet<PathBuf> = notes
        .iter()
        .map(|n| markdown_links::normalize(&n.path))
        .collect();
    let rewritten: Vec<(&Path, String)> = notes
        .iter()
        .map(|n| {
            (
                n.path.as_path(),
                rewrite_note_links_for_fold(&n.text, &n.path, &path, &removed),
            )
        })
        .collect();
    let dangling = find_dangling_links_after_collect(root, &notes, &rewritten);
    if !dangling.is_empty() {
        return Err(format!(
            "refusing to collect API migrations; {} link(s) would break:\n  {}",
            dangling.len(),
            dangling.join("\n  ")
        ));
    }
    let doc = std::fs::read_to_string(&path).map_err(|e| format!("{MIGRATIONS}: {e}"))?;
    let texts: Vec<&str> = rewritten.iter().map(|(_, t)| t.as_str()).collect();
    let folded = fold(&doc, version, &texts)?;
    std::fs::write(&path, folded).map_err(|e| format!("{MIGRATIONS}: {e}"))?;
    for note in &notes {
        std::fs::remove_file(&note.path)
            .map_err(|e| format!("remove {}: {e}", note.path.display()))?;
    }
    Ok(notes.len())
}

pub fn collect(version: Option<&str>) {
    let Some(version) = version.filter(|v| is_version(v)) else {
        eprintln!("usage: cargo xtask collect-api-migrations vMAJOR.MINOR.PATCH");
        std::process::exit(2);
    };
    let root = crate::repo_root();
    match collect_in(&root, version) {
        Ok(0) => println!("no unreleased API migrations; nothing to do"),
        Ok(count) => println!("moved {count} note(s) into {MIGRATIONS} under `## {version}`"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "### `run_drc` drops `tests` (minor release)\n\nUse a full run.\n";

    #[test]
    fn a_note_is_one_heading_then_text() {
        assert!(check(NOTE).is_ok());
        assert!(
            check("#### Detail\n\ntext\n").is_err(),
            "first line must be ###"
        );
        assert!(check("text first\n### Heading\n").is_err());
        assert!(check("### Heading\n\n").is_err(), "no text");
        assert!(check("### Heading\n\n## Split\n").is_err());
        assert!(check("### Heading\n\n### Second\n").is_err());
        assert!(check("### Heading\n\n#### Detail\n\ntext\n").is_ok());
        assert!(check("### Heading\n\n```sh\n## a shell comment\n```\n").is_ok());
    }

    #[test]
    fn versions_are_spelled_like_the_tags() {
        assert!(is_version("v0.15.0"));
        assert!(is_version("v1.20.3"));
        for bad in ["0.15.0", "v0.15", "v0.15.0-rc1", "v.1.2", "v0..1"] {
            assert!(!is_version(bad), "{bad}");
        }
    }

    #[test]
    fn a_release_goes_above_the_previous_one() {
        let doc = "# Migrations\n\nIntro.\n\n## v0.15.0\n\n### Old\n\nOld text.\n";
        let notes = ["### A\n\nA text.\n", "### B\n\nB text.\n\n"];
        assert_eq!(
            fold(doc, "v0.16.0", &notes).unwrap(),
            "# Migrations\n\nIntro.\n\n## v0.16.0\n\n### A\n\nA text.\n\n### B\n\nB text.\n\n\
             ## v0.15.0\n\n### Old\n\nOld text.\n"
        );
    }

    #[test]
    fn the_first_release_is_appended() {
        let doc = "# Migrations\n\nIntro.\n";
        assert_eq!(
            fold(doc, "v0.15.0", &["### A\n\nA text.\n"]).unwrap(),
            "# Migrations\n\nIntro.\n\n## v0.15.0\n\n### A\n\nA text.\n"
        );
    }

    #[test]
    fn crlf_documents_stay_crlf() {
        let doc = "# Migrations\r\n\r\n## v0.15.0\r\n\r\n### Old\r\n\r\nOld.\r\n";
        let folded = fold(doc, "v0.16.0", &["### A\n\nA text.\n"]).unwrap();
        assert!(!folded.replace("\r\n", "").contains('\n'), "{folded:?}");
    }

    #[test]
    fn a_release_is_collected_once() {
        let doc = "# Migrations\n\n## v0.15.0\n\n### Old\n\nOld.\n";
        assert!(fold(doc, "v0.15.0", &[NOTE]).is_err());
    }

    #[test]
    fn the_repository_notes_are_well_formed() {
        notes(&crate::repo_root()).unwrap();
    }

    fn temp_repo(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("copperpour-xtask-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(NOTES_DIR)).unwrap();
        std::fs::write(
            dir.join(MIGRATIONS),
            "# Migrations\n\n## v0.17.0\n\n### Old\n\nOld text.\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn collect_refuses_when_another_doc_links_to_a_removed_note() {
        let root = temp_repo("dangling-external");
        let note_path = root.join(NOTES_DIR).join("foo.md");
        std::fs::write(&note_path, NOTE).unwrap();
        std::fs::write(
            root.join("README.md"),
            "See [the migration](docs/api-migrations/foo.md).\n",
        )
        .unwrap();

        let err = collect_in(&root, "v0.18.0").unwrap_err();
        assert!(err.contains("README.md:1"), "{err}");
        assert!(err.contains("docs/api-migrations/foo.md"), "{err}");
        assert!(note_path.exists(), "refused collect must not delete note");
        let migrations = std::fs::read_to_string(root.join(MIGRATIONS)).unwrap();
        assert!(
            !migrations.contains("## v0.18.0"),
            "refused collect must not modify API_MIGRATIONS.md"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_rewrites_parent_and_kept_sibling_links_inside_folded_notes() {
        let root = temp_repo("rewrite-parent");
        std::fs::write(root.join("docs/TROUBLESHOOTING.md"), "# Troubleshooting\n").unwrap();
        std::fs::write(root.join(NOTES_DIR).join("README.md"), "# Notes\n").unwrap();
        let note_path = root.join(NOTES_DIR).join("note.md");
        std::fs::write(
            &note_path,
            "### Change (minor release)\n\nSee [guide](../TROUBLESHOOTING.md#sec), [alt](<../TROUBLESHOOTING.md>), and [format](README.md).\n",
        )
        .unwrap();

        assert_eq!(collect_in(&root, "v0.18.0").unwrap(), 1);
        assert!(!note_path.exists(), "note should be removed after fold");
        let migrations = std::fs::read_to_string(root.join(MIGRATIONS)).unwrap();
        assert!(
            migrations.contains("[guide](TROUBLESHOOTING.md#sec)"),
            "expected rewritten parent link in:\n{migrations}"
        );
        assert!(
            migrations.contains("[alt](<TROUBLESHOOTING.md>)"),
            "expected rewritten angle-bracketed link in:\n{migrations}"
        );
        assert!(
            migrations.contains("[format](api-migrations/README.md)"),
            "expected rewritten kept-sibling link in:\n{migrations}"
        );
        assert!(
            markdown_links::broken_relative_links(&root).is_empty(),
            "folded repo should have no broken links"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_refuses_when_folded_note_links_to_sibling_note() {
        let root = temp_repo("dangling-sibling");
        let a = root.join(NOTES_DIR).join("a.md");
        let b = root.join(NOTES_DIR).join("b.md");
        // `../api-migrations/b.md` is rewritten to `api-migrations/b.md`, which
        // still exists until the fold: only the removed-note check catches it.
        std::fs::write(
            &a,
            "### First (minor release)\n\nSee [second](b.md) and [again](../api-migrations/b.md).\n",
        )
        .unwrap();
        std::fs::write(&b, "### Second (minor release)\n\nDetails.\n").unwrap();

        let err = collect_in(&root, "v0.18.0").unwrap_err();
        assert!(err.contains("2 link(s) would break"), "{err}");
        assert!(err.contains("docs/api-migrations/a.md:3"), "{err}");
        assert!(err.contains("b.md"), "{err}");
        assert!(a.exists() && b.exists(), "refused collect must keep notes");
        let migrations = std::fs::read_to_string(root.join(MIGRATIONS)).unwrap();
        assert!(
            !migrations.contains("## v0.18.0"),
            "refused collect must not modify API_MIGRATIONS.md"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
