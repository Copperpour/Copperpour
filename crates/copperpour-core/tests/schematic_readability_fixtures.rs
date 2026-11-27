//! The KiCad 10 fixtures shared by the schematic readability work (discussion #638).
//!
//! These tests check only that each fixture is what its README says it is: a sheet the
//! repository's reader parses, saved by the writer the README names, with the README
//! beside it. Behaviour tests come with each change that uses a fixture.
use copperpour_sexp::schematic::{extract_labels, extract_symbol_instances, extract_wires};
use copperpour_sexp::{parse_sexp, SexpNode};
use std::path::{Path, PathBuf};

/// `(schematic, README that describes it, generator in the file header)`, relative to
/// `tests/fixtures`. Every sheet is a KiCad 10.0.8 resave except one: the note-rotation
/// fixture is Copperpour's own output and must never be saved by KiCad.
const SCHEMATICS: &[(&str, &str, &str)] = &[
    (
        "authoring_label_clearance_kicad10/amp_stage.kicad_sch",
        "authoring_label_clearance_kicad10/README.md",
        "eeschema",
    ),
    (
        "block_placement_kicad10/ldo_enable/ldo_enable.kicad_sch",
        "block_placement_kicad10/README.md",
        "eeschema",
    ),
    (
        "block_placement_kicad10/opamp_stage/opamp_stage.kicad_sch",
        "block_placement_kicad10/README.md",
        "eeschema",
    ),
    (
        "block_placement_kicad10/shared_rail/shared_rail.kicad_sch",
        "block_placement_kicad10/README.md",
        "eeschema",
    ),
    (
        "block_placement_kicad10/two_blocks/two_blocks.kicad_sch",
        "block_placement_kicad10/README.md",
        "eeschema",
    ),
    (
        "connector_pin_signals_kicad10.kicad_sch",
        "connector_pin_signals_kicad10.README.md",
        "eeschema",
    ),
    (
        "direct_lanes_kicad10/direct_lanes.kicad_sch",
        "direct_lanes_kicad10/README.md",
        "eeschema",
    ),
    (
        "field_placement_kicad10/field_placement.kicad_sch",
        "field_placement_kicad10/README.md",
        "eeschema",
    ),
    (
        "labels_vs_wires_kicad10/adjacent/adjacent.kicad_sch",
        "labels_vs_wires_kicad10/README.md",
        "eeschema",
    ),
    (
        "labels_vs_wires_kicad10/analog/analog.kicad_sch",
        "labels_vs_wires_kicad10/README.md",
        "eeschema",
    ),
    (
        "labels_vs_wires_kicad10/crossing/crossing.kicad_sch",
        "labels_vs_wires_kicad10/README.md",
        "eeschema",
    ),
    (
        "labels_vs_wires_kicad10/names/names.kicad_sch",
        "labels_vs_wires_kicad10/README.md",
        "eeschema",
    ),
    (
        "pin_contact_kicad10/legacy_route_elbows.kicad_sch",
        "pin_contact_kicad10/legacy_route_elbows.README.md",
        "eeschema",
    ),
    (
        "pin_contact_kicad10/pin_contact.kicad_sch",
        "pin_contact_kicad10/README.md",
        "eeschema",
    ),
    (
        "pin_contact_kicad10/nearby_wire_tap.kicad_sch",
        "pin_contact_kicad10/nearby_wire_tap.README.md",
        "eeschema",
    ),
    (
        "power_orientation_kicad10/power_orientation.kicad_sch",
        "power_orientation_kicad10/README.md",
        "eeschema",
    ),
    (
        "region_cleanup_kicad10/constrained/constrained.kicad_sch",
        "region_cleanup_kicad10/README.md",
        "eeschema",
    ),
    (
        "region_cleanup_kicad10/crowded/crowded.kicad_sch",
        "region_cleanup_kicad10/README.md",
        "eeschema",
    ),
    (
        "region_cleanup_kicad10/sparse/sparse.kicad_sch",
        "region_cleanup_kicad10/README.md",
        "eeschema",
    ),
    (
        "route_elbows_kicad10.kicad_sch",
        "route_elbows_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_note_bounds_kicad10.kicad_sch",
        "schematic_note_bounds_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_note_rotation_unsaved.kicad_sch",
        "schematic_note_rotation_unsaved.README.md",
        "copperpour",
    ),
    (
        "schematic_page_fit_a3_landscape_kicad10.kicad_sch",
        "schematic_page_fit_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_page_fit_coverage_kicad10.kicad_sch",
        "schematic_page_fit_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_page_fit_custom_sheet_kicad10.kicad_sch",
        "schematic_page_fit_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_page_fit_portrait_kicad10.kicad_sch",
        "schematic_page_fit_kicad10.README.md",
        "eeschema",
    ),
    (
        "schematic_wire_runs_kicad10.kicad_sch",
        "schematic_wire_runs_kicad10.README.md",
        "eeschema",
    ),
    (
        "sheet_split_kicad10/split/split.kicad_sch",
        "sheet_split_kicad10/README.md",
        "eeschema",
    ),
    (
        "wire_label_variants_kicad10.kicad_sch",
        "wire_label_variants_kicad10.README.md",
        "eeschema",
    ),
    (
        "wire_over_label_kicad10.kicad_sch",
        "wire_over_label_kicad10.README.md",
        "eeschema",
    ),
    (
        "wire_over_power_text_kicad10.kicad_sch",
        "wire_over_power_text_kicad10.README.md",
        "eeschema",
    ),
    (
        "worked_example_kicad10/after/worked_example.kicad_sch",
        "worked_example_kicad10/README.md",
        "eeschema",
    ),
    (
        "worked_example_kicad10/before/worked_example.kicad_sch",
        "worked_example_kicad10/README.md",
        "eeschema",
    ),
];

/// Project, drawing-sheet and board files that travel with those sheets, and the README
/// that explains why each one is there.
const COMPANIONS: &[(&str, &str)] = &[
    (
        "block_placement_kicad10/ldo_enable/ldo_enable.kicad_pro",
        "block_placement_kicad10/README.md",
    ),
    (
        "block_placement_kicad10/opamp_stage/opamp_stage.kicad_pro",
        "block_placement_kicad10/README.md",
    ),
    (
        "block_placement_kicad10/shared_rail/shared_rail.kicad_pro",
        "block_placement_kicad10/README.md",
    ),
    (
        "block_placement_kicad10/two_blocks/two_blocks.kicad_pro",
        "block_placement_kicad10/README.md",
    ),
    (
        "direct_lanes_kicad10/direct_lanes.kicad_pro",
        "direct_lanes_kicad10/README.md",
    ),
    (
        "field_placement_kicad10/field_placement.kicad_pro",
        "field_placement_kicad10/README.md",
    ),
    (
        "labels_vs_wires_kicad10/adjacent/adjacent.kicad_pro",
        "labels_vs_wires_kicad10/README.md",
    ),
    (
        "labels_vs_wires_kicad10/analog/analog.kicad_pro",
        "labels_vs_wires_kicad10/README.md",
    ),
    (
        "labels_vs_wires_kicad10/crossing/crossing.kicad_pro",
        "labels_vs_wires_kicad10/README.md",
    ),
    (
        "labels_vs_wires_kicad10/names/names.kicad_pro",
        "labels_vs_wires_kicad10/README.md",
    ),
    (
        "page_fit_custom_marked.kicad_wks",
        "schematic_page_fit_kicad10.README.md",
    ),
    (
        "page_fit_custom_unmarked.kicad_wks",
        "schematic_page_fit_kicad10.README.md",
    ),
    (
        "region_cleanup_kicad10/constrained/constrained.kicad_pro",
        "region_cleanup_kicad10/README.md",
    ),
    (
        "region_cleanup_kicad10/crowded/crowded.kicad_pro",
        "region_cleanup_kicad10/README.md",
    ),
    (
        "region_cleanup_kicad10/sparse/sparse.kicad_pro",
        "region_cleanup_kicad10/README.md",
    ),
    (
        "sheet_split_kicad10/split/split.kicad_pro",
        "sheet_split_kicad10/README.md",
    ),
    (
        "sheet_split_kicad10/split_populated.kicad_pcb",
        "sheet_split_kicad10/README.md",
    ),
    (
        "worked_example_kicad10/after/worked_example.kicad_pro",
        "worked_example_kicad10/README.md",
    ),
    (
        "worked_example_kicad10/before/worked_example.kicad_pro",
        "worked_example_kicad10/README.md",
    ),
];

/// Fixture directories that hold only these fixtures, so every KiCad file in them must
/// be listed above.
const DIRECTORIES: &[&str] = &[
    "authoring_label_clearance_kicad10",
    "block_placement_kicad10",
    "direct_lanes_kicad10",
    "field_placement_kicad10",
    "labels_vs_wires_kicad10",
    "pin_contact_kicad10",
    "power_orientation_kicad10",
    "region_cleanup_kicad10",
    "sheet_split_kicad10",
    "worked_example_kicad10",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(relative: &str) -> String {
    let path = fixtures().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn file_name(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap()
}

fn parse(relative: &str) -> SexpNode {
    parse_sexp(&read(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

/// The README exists and names the file, so a reader can find what it describes.
fn assert_described(relative: &str, readme: &str) {
    let text = read(readme);
    assert!(
        text.contains(file_name(relative)),
        "{readme} does not mention {}",
        file_name(relative)
    );
}

#[test]
fn every_sheet_parses_and_names_its_writer() {
    for (sheet, readme, generator) in SCHEMATICS {
        let tree = parse(sheet);
        assert_eq!(tree.head(), Some("kicad_sch"), "{sheet}");
        assert_eq!(tree.find_str("generator"), Some(*generator), "{sheet}");
        let drawn = extract_symbol_instances(&tree).len()
            + extract_wires(&tree).len()
            + extract_labels(&tree).len()
            + tree.find_all("text").len();
        assert!(drawn > 0, "{sheet} draws nothing the reader can see");
        assert_described(sheet, readme);
    }
}

#[test]
fn every_resaved_sheet_has_kicad_10_format() {
    for (sheet, _, generator) in SCHEMATICS {
        if *generator != "eeschema" {
            continue;
        }
        let tree = parse(sheet);
        assert_eq!(tree.find_str("version"), Some("20260306"), "{sheet}");
        assert_eq!(tree.find_str("generator_version"), Some("10.0"), "{sheet}");
    }
}

#[test]
fn every_companion_file_parses_and_is_described() {
    for (file, readme) in COMPANIONS {
        if file.ends_with(".kicad_pro") {
            let project: serde_json::Value =
                serde_json::from_str(&read(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(
                project["meta"]["filename"].as_str(),
                Some(file_name(file)),
                "{file}"
            );
            let sheet = file.replace(".kicad_pro", ".kicad_sch");
            assert!(
                SCHEMATICS.iter().any(|(s, _, _)| *s == sheet),
                "{file} has no sheet beside it"
            );
            // A folder README may cover all its project files at once.
            assert!(
                read(readme).contains(".kicad_pro"),
                "{readme} does not explain the project files"
            );
            continue;
        } else {
            let head = if file.ends_with(".kicad_wks") {
                "kicad_wks"
            } else {
                "kicad_pcb"
            };
            assert_eq!(parse(file).head(), Some(head), "{file}");
        }
        assert_described(file, readme);
    }
}

#[test]
fn every_kicad_file_in_a_fixture_directory_is_listed() {
    let listed: Vec<&str> = SCHEMATICS
        .iter()
        .map(|(s, _, _)| *s)
        .chain(COMPANIONS.iter().map(|(f, _)| *f))
        .collect();
    let mut found = Vec::new();
    for dir in DIRECTORIES {
        collect_kicad_files(&fixtures().join(dir), &mut found);
    }
    assert!(!found.is_empty());
    for path in found {
        let relative = path
            .strip_prefix(fixtures())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            listed.contains(&relative.as_str()),
            "{relative} is not listed in this test"
        );
    }
}

fn collect_kicad_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_kicad_files(&path, found);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.starts_with("kicad_"))
        {
            found.push(path);
        }
    }
}
