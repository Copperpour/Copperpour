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
        "{base} on this sheet ({}). It is on another sheet of the project: {}. \
         Pass that file as `schematic`.",
        sch_path.display(),
        described.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(reference: &str, instance_refs: &[&str]) -> String {
        let paths: String = instance_refs
            .iter()
            .enumerate()
            .map(|(i, r)| format!("(path \"/root/s{i}\" (reference \"{r}\") (unit 1))"))
            .collect();
        format!(
            "(symbol (lib_id \"Device:R\") (at 10 10 0) (unit 1) \
             (property \"Reference\" \"{reference}\" (at 0 0 0)) \
             (instances (project \"p\" {paths})))"
        )
    }

    fn sheet(symbols: &[String]) -> String {
        format!(
            "(kicad_sch (version 20250114) (uuid \"u\") (lib_symbols (symbol \"Device:R\")) {})",
            symbols.join(" ")
        )
    }

    fn project(files: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn a_reference_on_a_sub_sheet_is_located() {
        let dir = project(&[
            ("root.kicad_sch", sheet(&[symbol("R1", &["R1"])])),
            ("sub.kicad_sch", sheet(&[symbol("Q7", &["Q7"])])),
            ("_autosave-sub.kicad_sch", sheet(&[symbol("Q7", &["Q7"])])),
        ]);
        let root = dir.path().join("root.kicad_sch");
        let sites = locate_reference(&root, "Q7");
        assert_eq!(
            sites,
            vec![ReferenceSite {
                file: dir.path().join("sub.kicad_sch"),
                saved_reference: "Q7".into()
            }]
        );
        let message = component_not_found(&root, "Q7");
        assert!(message.starts_with("Component 'Q7' not found on this sheet"));
        assert!(message.contains("sub.kicad_sch"), "{message}");
    }

    #[test]
    fn a_second_instance_reference_names_the_saved_one() {
        let dir = project(&[
            ("root.kicad_sch", sheet(&[])),
            ("ch.kicad_sch", sheet(&[symbol("R103", &["R103", "R203"])])),
        ]);
        let root = dir.path().join("root.kicad_sch");
        let sites = locate_reference(&root, "R203");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].saved_reference, "R103");
        let message = component_not_found(&root, "R203");
        assert!(message.contains("address it as 'R103'"), "{message}");
    }

    #[test]
    fn the_given_sheet_and_library_definitions_are_not_reported() {
        // A library definition named like the reference is no placed symbol.
        let lib_only = "(kicad_sch (version 20250114) (uuid \"u\") \
                        (lib_symbols (symbol \"Q7\" (property \"Reference\" \"Q7\"))))";
        let dir = project(&[
            ("root.kicad_sch", sheet(&[symbol("Q7", &["Q7"])])),
            ("lib.kicad_sch", lib_only.to_string()),
            ("broken.kicad_sch", "(kicad_sch".to_string()),
        ]);
        let root = dir.path().join("root.kicad_sch");
        assert!(locate_reference(&root, "Q7").is_empty());
        assert_eq!(component_not_found(&root, "Q7"), "Component 'Q7' not found");
    }

    #[test]
    fn an_unknown_reference_keeps_the_plain_message() {
        let dir = project(&[("root.kicad_sch", sheet(&[]))]);
        let root = dir.path().join("root.kicad_sch");
        assert_eq!(component_not_found(&root, "U9"), "Component 'U9' not found");
    }
}
