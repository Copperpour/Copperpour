//! Golden-file conformance suite.
//!
//! Oracle: schematics authored by eeschema itself. KiCAD installs a demo
//! corpus (`share/kicad/demos`) full of real, hierarchy-heavy, multi-unit
//! designs — if our parser or editors disagree with anything in there, we
//! disagree with KiCAD.
//!
//! These tests locate an installed KiCAD (or `KICAD_DEMOS` env override) and
//! SKIP silently when none is present, so plain CI stays green while the
//! scheduled real-KiCAD workflow and local dev runs get full coverage.
//! (Same skip pattern the predecessor project used for its kicad-cli tests.)

use copperpour_sexp::{parse_sexp, writer};
use std::path::PathBuf;

fn demo_dirs() -> Option<PathBuf> {
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

fn collect_schematics(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "kicad_sch") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Every schematic eeschema ships must parse. This is the broadest format-
/// coverage test we have: hierarchical sheets, multi-unit symbols, buses,
/// text boxes, images — whatever the demos contain, the parser must accept.
#[test]
fn every_installed_demo_schematic_parses() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found (set KICAD_DEMOS to enable)");
        return;
    };
    let schematics = collect_schematics(&root);
    assert!(
        !schematics.is_empty(),
        "demo dir exists but contains no .kicad_sch files: {}",
        root.display()
    );

    let mut parsed = 0usize;
    let mut failures = Vec::new();
    for sch in &schematics {
        let content = std::fs::read_to_string(sch).unwrap_or_default();
        match parse_sexp(&content) {
            Ok(node) => {
                assert_eq!(
                    node.head(),
                    Some("kicad_sch"),
                    "unexpected root in {}",
                    sch.display()
                );
                parsed += 1;
            }
            Err(e) => failures.push(format!("{}: {}", sch.display(), e)),
        }
    }
    eprintln!("parsed {}/{} demo schematics", parsed, schematics.len());
    assert!(
        failures.is_empty(),
        "parser rejected eeschema-authored files:\n  {}",
        failures.join("\n  ")
    );
}

/// Structural extraction must work on real designs: symbols, wires, and
/// labels come back non-empty for the demo corpus as a whole, and pin
/// transforms compute without panicking for every instance.
#[test]
fn demo_corpus_structural_extraction() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found");
        return;
    };
    use copperpour_sexp::schematic::{extract_symbol_instances, extract_wires};

    let mut total_symbols = 0usize;
    let mut total_wires = 0usize;
    for sch in collect_schematics(&root) {
        let content = std::fs::read_to_string(&sch).unwrap_or_default();
        let Ok(tree) = parse_sexp(&content) else {
            continue; // parse failures are the previous test's job
        };
        let symbols = extract_symbol_instances(&tree);
        for inst in &symbols {
            // Must never panic, whatever rotation/mirror combination ships.
            let t = inst.pin_transform();
            let _ = copperpour_sexp::geometry::transform_pin(1.27, 2.54, t);
        }
        total_symbols += symbols.len();
        total_wires += extract_wires(&tree).len();
    }
    eprintln!("extracted {} symbols, {} wires", total_symbols, total_wires);
    assert!(total_symbols > 100, "suspiciously few symbols extracted");
    assert!(total_wires > 100, "suspiciously few wires extracted");
}

/// Byte-edit safety on real files: applying a no-op edit (insert + delete of
/// the same text) to an eeschema file must leave it byte-identical, and an
/// actual insertion must still re-parse. Guards the predecessor's file-
/// corruption class without needing a full serializer.
#[test]
fn demo_files_survive_edit_cycle() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found");
        return;
    };
    let schematics = collect_schematics(&root);
    // A representative slice keeps this test fast even on huge corpora.
    for sch in schematics.iter().take(10) {
        let original = std::fs::read_to_string(sch).unwrap();

        // No-op: insert marker then delete it again.
        let marker = "(text \"konnect-conformance-probe\")";
        let insert_at = original.rfind(')').unwrap();
        let inserted = writer::apply_edits(
            original.clone(),
            vec![copperpour_sexp::SexpEdit {
                start: insert_at,
                end: insert_at,
                replacement: marker.to_string(),
            }],
        );
        assert!(
            parse_sexp(&inserted).is_ok(),
            "insertion broke parseability of {}",
            sch.display()
        );

        let removed = writer::apply_edits(
            inserted.clone(),
            vec![copperpour_sexp::SexpEdit {
                start: insert_at,
                end: insert_at + marker.len(),
                replacement: String::new(),
            }],
        );
        assert_eq!(
            removed,
            original,
            "edit round-trip not byte-identical for {}",
            sch.display()
        );
    }
}

/// Oracle for [`pin_label_rotation`]: labels eeschema itself anchored on a pin.
///
/// A net label whose text runs *into* the symbol body covers the pin names
/// KiCad draws there. The demos say how eeschema orients them — for every
/// label sitting exactly on one pin endpoint, compare its rotation against the
/// direction leading away from that body.
#[test]
fn pin_anchored_labels_match_eeschema_orientation() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCad demos found");
        return;
    };
    use copperpour_sexp::geometry::points_coincident;
    use copperpour_sexp::schematic::{
        extract_labels, extract_lib_pins_for_unit, extract_symbol_instances, pin_endpoint,
        pin_outward_direction,
    };

    let (mut horizontal, mut vertical) = (0usize, 0usize);
    let (mut disagreements, mut sideways) = (Vec::new(), Vec::new());
    for sch in collect_schematics(&root) {
        let content = std::fs::read_to_string(&sch).unwrap_or_default();
        let Ok(tree) = parse_sexp(&content) else {
            continue;
        };
        let lib_syms = tree
            .find("lib_symbols")
            .map(|n| n.find_all("symbol"))
            .unwrap_or_default();

        // Every pin endpoint on the sheet, with the way it faces.
        let mut pins: Vec<((f64, f64), f64)> = Vec::new();
        for inst in extract_symbol_instances(&tree) {
            let Some(sym) = lib_syms
                .iter()
                .find(|n| n.get(1).and_then(|c| c.as_str()) == Some(&inst.lib_id))
            else {
                continue;
            };
            let t = inst.pin_transform();
            // Unit-aware: superimposing another unit's pins would invent
            // endpoints that no label can legitimately sit on (#35).
            for pin in extract_lib_pins_for_unit(sym, inst.unit) {
                pins.push((pin_endpoint(&pin, t), pin_outward_direction(&pin, t)));
            }
        }

        for label in extract_labels(&tree) {
            let mut hits = pins
                .iter()
                .filter(|(p, _)| points_coincident(p.0, p.1, label.x, label.y, 0.01));
            let Some((_, outward)) = hits.next() else {
                continue; // label on a wire, or free-floating
            };
            if hits.next().is_some() {
                continue; // stacked pins: no single pin owns this anchor
            }
            let rotation = label.rotation.rem_euclid(360.0);
            if *outward == 90.0 || *outward == 270.0 {
                vertical += 1;
            } else {
                horizontal += 1;
                if rotation != *outward {
                    disagreements.push(format!(
                        "{}: '{}' at ({}, {}) is {rotation}°, pin faces {outward}°",
                        sch.display(),
                        label.net,
                        label.x,
                        label.y
                    ));
                }
            }
            // Whichever way the pin faces, eeschema never turns the text
            // sideways — the invariant `horizontal_label_rotation` encodes.
            // Collected, not asserted here: one violation must not abort the
            // sweep before it can report what else disagrees.
            if rotation != 0.0 && rotation != 180.0 {
                sideways.push(format!(
                    "{}: '{}' at ({}, {}) is rotated {rotation}°",
                    sch.display(),
                    label.net,
                    label.x,
                    label.y
                ));
            }
        }
    }

    eprintln!("pin-anchored labels: {horizontal} horizontal, {vertical} vertical");
    // Guard against a matcher that quietly stops matching and passes vacuously.
    assert!(
        horizontal >= 200,
        "suspiciously few pin-anchored labels matched ({horizontal})"
    );
    assert!(
        sideways.is_empty(),
        "no pin-anchored label in the demo corpus should be vertical:\n{}",
        sideways.join("\n")
    );
    assert!(
        disagreements.is_empty(),
        "labels disagree with pin_outward_direction:\n{}",
        disagreements.join("\n")
    );
}
