//! Labels must survive a load/save round-trip with every child KiCad wrote (#695).
//!
//! `Label`, `GlobalLabel` and `HierarchicalLabel` rebuilt each block from a fixed
//! set of fields and kept nothing else, so every whole-file write deleted
//! `(fields_autoplaced …)` from every label that carried it, including labels the
//! call never touched.
//!
//! KiCad writes the token in two forms: the bare `(fields_autoplaced)` in KiCad 7
//! files (format `20230819`) and `(fields_autoplaced yes)` from KiCad 8 on. The
//! fixtures are label blocks copied byte for byte out of KiCad 10.0.5's own demo
//! schematics; `tests/fixtures/labels_kicad.README.md` records where each came
//! from.

use copperpour_schematic_editor::sexp::parser::parse;
use copperpour_schematic_editor::sexp::SexpNode;
use copperpour_schematic_editor::Schematic;
use std::path::{Path, PathBuf};

const KICAD7: &str = include_str!("fixtures/labels_kicad7.kicad_sch");
const KICAD9: &str = include_str!("fixtures/labels_kicad9.kicad_sch");

const LABEL_KINDS: [&str; 3] = ["label", "global_label", "hierarchical_label"];

fn round_trip(src: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = dir.path().join("in.kicad_sch");
    let output = dir.path().join("out.kicad_sch");
    std::fs::write(&input, src).expect("write fixture");
    Schematic::load(&input)
        .expect("load")
        .save(&output)
        .expect("save");
    std::fs::read_to_string(&output).expect("read back")
}

/// The source text of the parenthesised block starting at byte `start`.
fn balanced(text: &str, start: usize) -> &str {
    let bytes = text.as_bytes();
    let (mut depth, mut i) = (0usize, start);
    loop {
        match bytes[i] {
            b'"' => {
                i += 1;
                while bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[start..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Every top-level label block in `text`, as `(uuid, source text)`.
fn label_blocks(text: &str) -> Vec<(String, String)> {
    let root = parse(text).expect("parse");
    let mut out = Vec::new();
    for kind in LABEL_KINDS {
        let needle = format!("({kind} \"");
        let mut from = 0;
        while let Some(off) = text[from..].find(&needle) {
            let start = from + off;
            let block = balanced(text, start);
            let node = parse(block).expect("label block parses");
            let uuid = node.get_value("uuid").expect("label uuid").to_owned();
            out.push((uuid, block.to_owned()));
            from = start + block.len();
        }
    }
    // Every block found must be a direct child of the sheet, not text inside one.
    let top_level = root
        .args()
        .iter()
        .filter(|n| n.tag().is_some_and(|t| LABEL_KINDS.contains(&t)))
        .count();
    assert_eq!(out.len(), top_level, "label scan disagrees with the parser");
    out
}

fn block_by_uuid(text: &str, uuid: &str) -> String {
    label_blocks(text)
        .into_iter()
        .find(|(u, _)| u == uuid)
        .unwrap_or_else(|| panic!("label {uuid} missing from:\n{text}"))
        .1
}

/// A block with its line endings folded to LF. The demos are CRLF, as KiCad on
/// Windows saves them, and Konnect writes the whole file with LF; line endings
/// are a property of the file, not of a label, so byte comparisons ignore them.
fn lf(block: String) -> String {
    block.replace("\r\n", "\n")
}

/// A label's children by tag, in order, with each child rendered as its text so
/// that a quoted and an unquoted UUID compare equal. KiCad 7 writes compact,
/// unquoted blocks that Konnect's writer re-lays out (#210), so for those files
/// this is the comparison that isolates which children survived and where.
fn children(block: &str) -> Vec<String> {
    fn render(n: &SexpNode) -> String {
        match n {
            SexpNode::List(c) => {
                let inner: Vec<String> = c.iter().map(render).collect();
                format!("({})", inner.join(" "))
            }
            SexpNode::Atom(s) | SexpNode::Str(s) => s.clone(),
        }
    }
    parse(block)
        .expect("label block parses")
        .args()
        .iter()
        .map(render)
        .collect()
}

const OUT6: &str = "0062fe1f-cf41-406e-b703-2262a1b26c41"; // global_label, `yes` form
const CC2: &str = "2304f992-0bb1-483c-b141-e6c210b6d83d"; // label, no token
const TRIGOUT6: &str = "023cf3af-4ede-4443-8185-42863f2b4a2f"; // label, bare form
const TRIGIO_IN: &str = "4f8cf29d-9167-4ba7-a0ed-759e3dacdd1c"; // hierarchical_label, bare form

#[test]
fn a_kicad_written_global_label_round_trips_byte_identically() {
    let out = round_trip(KICAD9);
    assert_eq!(
        lf(block_by_uuid(&out, OUT6)),
        lf(block_by_uuid(KICAD9, OUT6))
    );
}

#[test]
fn a_label_without_the_token_round_trips_and_gains_none() {
    let out = round_trip(KICAD9);
    let block = block_by_uuid(&out, CC2);
    assert_eq!(lf(block.clone()), lf(block_by_uuid(KICAD9, CC2)));
    assert!(
        !block.contains("fields_autoplaced"),
        "token invented:\n{block}"
    );
}

#[test]
fn kicad7_bare_tokens_keep_their_form_and_position() {
    let out = round_trip(KICAD7);
    for uuid in [TRIGOUT6, TRIGIO_IN] {
        let before = children(&block_by_uuid(KICAD7, uuid));
        let after = children(&block_by_uuid(&out, uuid));
        assert_eq!(after, before, "children of {uuid} changed");
        let at = after
            .iter()
            .position(|c| c.starts_with("(at "))
            .expect("at");
        assert_eq!(after[at + 1], "(fields_autoplaced)", "{uuid}: {after:?}");
    }
    assert!(
        !out.contains("fields_autoplaced yes"),
        "form rewritten:\n{out}"
    );
}

/// The defect was structural: anything the model did not know about was
/// discarded. A future KiCad attribute must survive on every label type.
#[test]
fn unmodelled_children_survive_on_every_label_type() {
    for (src, uuid) in [(KICAD9, OUT6), (KICAD9, CC2), (KICAD7, TRIGIO_IN)] {
        let original = block_by_uuid(src, uuid);
        let close = original.rfind(')').expect("closing paren");
        let mut edited = original.clone();
        edited.insert_str(close, "(some_future_attribute yes)");
        let out = round_trip(&src.replace(&original, &edited));
        let block = block_by_uuid(&out, uuid);
        assert!(
            block.contains("(some_future_attribute yes)"),
            "unmodelled child dropped from {uuid}:\n{block}"
        );
        assert_eq!(
            block
                .matches(&format!(
                    "\"{}\"",
                    parse(&original).unwrap().value().unwrap()
                ))
                .count(),
            1,
            "label text written twice:\n{block}"
        );
    }
}

#[test]
fn labels_konnect_creates_carry_no_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = dir.path().join("in.kicad_sch");
    let output = dir.path().join("out.kicad_sch");
    std::fs::write(&input, KICAD9).expect("write fixture");
    let mut sch = Schematic::load(&input).expect("load");
    sch.add_label("NEW_LOCAL", 10.16, 10.16);
    sch.add_global_label("NEW_GLOBAL", "input", 20.32, 10.16);
    sch.add_hierarchical_label("NEW_HIER", "output", 30.48, 10.16);
    sch.save(&output).expect("save");
    let out = std::fs::read_to_string(&output).expect("read back");
    for (_, block) in label_blocks(&out)
        .into_iter()
        .filter(|(_, b)| b.contains("\"NEW_"))
    {
        assert!(
            !block.contains("fields_autoplaced"),
            "token invented:\n{block}"
        );
    }
    // The KiCad-written label still has its token after the edit.
    assert!(block_by_uuid(&out, OUT6).contains("(fields_autoplaced yes)"));
}

// ---- oracle: every label KiCad ships ----------------------------------------

fn demo_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KICAD_DEMOS") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\KiCad\10.0\share\kicad\demos",
            r"C:\Program Files\KiCad\10.0\share\kicad\demos",
        ]
    } else if cfg!(target_os = "macos") {
        &["/Applications/KiCad/KiCad.app/Contents/SharedSupport/demos"]
    } else {
        &["/usr/share/kicad/demos", "/usr/local/share/kicad/demos"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

fn schematics_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "kicad_sch") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Every label in every schematic of an installed KiCad keeps every child, in
/// KiCad's order, through a load/save round-trip. Skips silently without KiCad,
/// like the conformance suite.
#[test]
fn every_label_kicad_ships_keeps_its_children() {
    let Some(root) = demo_dir() else {
        eprintln!("no KiCad demo corpus found; skipping");
        return;
    };
    let (mut labels, mut autoplaced) = (0usize, 0usize);
    let mut failures = Vec::new();
    for path in schematics_under(&root) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let out = round_trip(&src);
        let after: std::collections::HashMap<_, _> = label_blocks(&out).into_iter().collect();
        for (uuid, block) in label_blocks(&src) {
            labels += 1;
            autoplaced += usize::from(block.contains("(fields_autoplaced"));
            let kept = after.get(&uuid).map(|b| children(b));
            if kept.as_ref() != Some(&children(&block)) {
                failures.push(format!("{} {uuid}", path.display()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {labels} labels changed, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
    eprintln!("{labels} labels, {autoplaced} with fields_autoplaced, all intact");
}
