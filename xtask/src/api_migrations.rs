//! `cargo xtask collect-api-migrations <version>` moves the unreleased notes in
//! `docs/api-migrations/` into `docs/API_MIGRATIONS.md`, under a `## <version>`
//! heading above the previous release.
//!
//! Why this exists: every PR used to add its note at the top of
//! `API_MIGRATIONS.md`, so any two open PRs conflicted there even when their
//! code merged cleanly. Each PR now adds a file of its own instead.

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

pub fn collect(version: Option<&str>) {
    let Some(version) = version.filter(|v| is_version(v)) else {
        eprintln!("usage: cargo xtask collect-api-migrations vMAJOR.MINOR.PATCH");
        std::process::exit(2);
    };
    let root = crate::repo_root();
    let fail = |e: String| -> ! {
        eprintln!("{e}");
        std::process::exit(1);
    };
    let notes = notes(&root).unwrap_or_else(|e| fail(e));
    if notes.is_empty() {
        println!("no unreleased API migrations; nothing to do");
        return;
    }
    let path = root.join(MIGRATIONS);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| fail(format!("{MIGRATIONS}: {e}")));
    let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
    let folded = fold(&doc, version, &texts).unwrap_or_else(|e| fail(e));
    std::fs::write(&path, folded).expect("write");
    for note in &notes {
        std::fs::remove_file(&note.path).expect("remove note");
    }
    println!(
        "moved {} note(s) into {MIGRATIONS} under `## {version}`",
        notes.len()
    );
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
}
