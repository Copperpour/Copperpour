//! Where a reference lives when it is not on the sheet a tool was given.
//!
//! The schematic tools read one `.kicad_sch` file. A component on another
//! sheet of the same project is "not found" there, and a bare "not found"
//! reads as "the part does not exist". This module looks through the other
//! sheet files in the same directory and turns the refusal into a pointer:
//! which file holds the reference, and under which reference that file saves
//! the symbol when the reference belongs to another instance of a sheet that
//! is placed more than once.
//!
//! The search is the given sheet's directory, not the project hierarchy: a
//! sheet in a subdirectory is not found, and a sheet of another project in
//! the same directory can be named. That is enough for a hint and is said in
//! the message; it is not a statement about the project.
//!
//! It only ever reads, and only on the not-found path, so it costs nothing on
//! a call that succeeds.

use copperpour_sexp::{parser::parse_sexp, SexpNode};
use std::path::{Path, PathBuf};

/// Sheet files searched at most, so a directory full of schematics cannot
/// turn one refusal into a long scan.
const MAX_SHEETS: usize = 200;

/// The prefix KiCad gives the autosave copy of a sheet it has open.
const AUTOSAVE_PREFIX: &str = "_autosave-";

/// One place a reference was found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReferenceSite {
    pub file: PathBuf,
    /// The symbol's saved `Reference` property. Differs from the reference
    /// asked for when that one is only recorded for another sheet instance.
    pub saved_reference: String,
}

/// Every sheet file in `sch_path`'s directory, other than `sch_path` itself,
/// that places a symbol with `reference`, either as its `Reference` property
/// or as the reference one of its sheet instances records. Unreadable or
/// unparsable files are skipped: this is a hint, not a check.
pub(crate) fn locate_reference(sch_path: &Path, reference: &str) -> Vec<ReferenceSite> {
    let Some(dir) = sch_path.parent() else {
        return Vec::new();
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let own = std::fs::canonicalize(sch_path).ok();
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "kicad_sch"))
        // KiCad's autosave copy of an open sheet is not a sheet of the
        // project; naming it would send the caller to a file KiCad discards.
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(AUTOSAVE_PREFIX))
        })
        .filter(|path| own.is_none() || std::fs::canonicalize(path).ok() != own)
        .collect();
    files.sort();
    files.truncate(MAX_SHEETS);

    let mut sites = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(tree) = parse_sexp(&source) else {
            continue;
        };
        if let Some(saved_reference) = saved_reference_for(&tree, reference) {
            sites.push(ReferenceSite {
                file,
                saved_reference,
            });
        }
    }
    sites
}

/// The `Reference` property of the first placed symbol in `tree` that is
/// `reference`, by property or by a recorded instance reference.
fn saved_reference_for(tree: &SexpNode, reference: &str) -> Option<String> {
    let mut by_instance = None;
    for symbol in tree.find_all("symbol") {
        // Placed symbols carry a lib_id; library definitions do not.
        if symbol.find("lib_id").is_none() {
            continue;
        }
        let saved = symbol
            .find_all("property")
            .into_iter()
            .find(|p| p.get(1).and_then(SexpNode::as_str) == Some("Reference"))
            .and_then(|p| p.get(2))
            .and_then(SexpNode::as_str)
            .map(str::to_string);
        if saved.as_deref() == Some(reference) {
            return saved;
        }
        if by_instance.is_none() && instance_references(symbol).any(|r| r == reference) {
            by_instance = saved;
        }
    }
    by_instance
}

/// The references a placed symbol's `(instances …)` records, one per sheet
/// instance.
fn instance_references(symbol: &SexpNode) -> impl Iterator<Item = &str> {
    symbol
        .find("instances")
        .into_iter()
        .flat_map(|instances| instances.find_all("project"))
        .flat_map(|project| project.find_all("path"))
        .filter_map(|path| path.find("reference"))
        .filter_map(|r| r.get(1).and_then(SexpNode::as_str))
}

/// "Component 'X' not found", followed, when the reference is on another
/// sheet file of the project, by where it is and what to pass instead.
pub(crate) fn component_not_found(sch_path: &Path, reference: &str) -> String {
    let base = format!("Component '{reference}' not found");
    let sites = locate_reference(sch_path, reference);
    if sites.is_empty() {
        return base;
    }
    let described: Vec<String> = sites
        .iter()
        .map(|site| {
            if site.saved_reference == reference {
                format!("{}", site.file.display())
            } else {
                format!(
                    "{} (saved there as '{}': '{reference}' is the reference of another \
                     instance of that sheet; address it as '{}')",
                    site.file.display(),
                    site.saved_reference,
                    site.saved_reference
                )
            }
        })
        .collect();
    format!(
        "{base} on this sheet ({}). A sheet file in the same directory places it: {}. \
         Pass that file as `schematic`.",
        sch_path.display(),
        described.join("; ")
    )
}

#[cfg(test)]
mod tests {
    //! Against the KiCad-saved `annotate_nested_hierarchy` fixture: the root
    //! places `bank.kicad_sch` twice and `bank.kicad_sch` places
    //! `cell.kicad_sch` twice. Expectations come from `kicad-cli` 10.0.7
    //! (`sch export netlist --format kicadxml` on the root): `R7`, `R9`, `R14`
    //! and `R16` are one symbol (tstamp `229a802a-…`) with Sheetfile
    //! `cell.kicad_sch`, saved there with `Reference` `R7`; `R3` and `R10`
    //! are one symbol on `bank.kicad_sch`, saved as `R3`; `R6` is on
    //! `cell.kicad_sch`, saved as `R6`.

    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/annotate_nested_hierarchy"
    );

    /// A temporary copy of the fixture, so tests that add files never touch
    /// the checked-in one.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "nested_pages.kicad_pro",
            "nested_pages.kicad_sch",
            "bank.kicad_sch",
            "cell.kicad_sch",
        ] {
            std::fs::copy(Path::new(FIXTURE).join(name), dir.path().join(name)).unwrap();
        }
        dir
    }

    fn site(dir: &tempfile::TempDir, file: &str, saved: &str) -> ReferenceSite {
        ReferenceSite {
            file: dir.path().join(file),
            saved_reference: saved.into(),
        }
    }

    #[test]
    fn another_instance_reference_on_the_root_names_the_saved_one() {
        let dir = project();
        let root = dir.path().join("nested_pages.kicad_sch");
        assert_eq!(
            locate_reference(&root, "R9"),
            vec![site(&dir, "cell.kicad_sch", "R7")]
        );
        let message = component_not_found(&root, "R9");
        assert!(
            message.starts_with("Component 'R9' not found on this sheet"),
            "{message}"
        );
        assert!(message.contains("cell.kicad_sch"), "{message}");
        assert!(message.contains("address it as 'R7'"), "{message}");
    }

    #[test]
    fn a_reference_saved_on_a_sub_sheet_is_named_as_is() {
        let dir = project();
        let root = dir.path().join("nested_pages.kicad_sch");
        assert_eq!(
            locate_reference(&root, "R3"),
            vec![site(&dir, "bank.kicad_sch", "R3")]
        );
        assert_eq!(
            locate_reference(&root, "R10"),
            vec![site(&dir, "bank.kicad_sch", "R3")]
        );
        let message = component_not_found(&root, "R3");
        assert!(message.contains("bank.kicad_sch"), "{message}");
        assert!(!message.contains("address it as"), "{message}");
    }

    #[test]
    fn a_sub_sheet_points_to_the_sheet_it_places() {
        let dir = project();
        let bank = dir.path().join("bank.kicad_sch");
        assert_eq!(
            locate_reference(&bank, "R6"),
            vec![site(&dir, "cell.kicad_sch", "R6")]
        );
        assert_eq!(
            locate_reference(&bank, "R16"),
            vec![site(&dir, "cell.kicad_sch", "R7")]
        );
    }

    #[test]
    fn the_given_sheet_autosaves_and_library_definitions_are_not_reported() {
        let dir = project();
        // KiCad's autosave copy of an open sheet, and an unparsable file.
        std::fs::copy(
            dir.path().join("cell.kicad_sch"),
            dir.path().join("_autosave-cell.kicad_sch"),
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.kicad_sch"), "(kicad_sch").unwrap();
        let root = dir.path().join("nested_pages.kicad_sch");
        assert_eq!(
            locate_reference(&root, "R9"),
            vec![site(&dir, "cell.kicad_sch", "R7")]
        );
        // `R1` is on the root itself, and every sheet's `lib_symbols` defines
        // a `Device:R` whose Reference property is the bare prefix `R`.
        assert!(locate_reference(&root, "R1").is_empty());
        assert!(locate_reference(&root, "R").is_empty());
        assert_eq!(component_not_found(&root, "R"), "Component 'R' not found");
    }

    #[test]
    fn an_unknown_reference_keeps_the_plain_message() {
        let dir = project();
        let root = dir.path().join("nested_pages.kicad_sch");
        assert_eq!(component_not_found(&root, "U9"), "Component 'U9' not found");
    }
}
