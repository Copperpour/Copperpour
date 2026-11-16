//! `(pts …)` blocks must come back as eeschema wrote them (#20).
//!
//! eeschema keeps a run of `(xy …)` points on one line while that line is shorter
//! than 99 characters, a tab counting one, and starts the next point on a new line
//! once it is not. This crate wrote every point on a line of its own, so a
//! whole-sheet write rewrote every wire, bus and polyline on the sheet, including
//! those the call never touched.
//!
//! The fixture is a sheet eeschema wrote; `tests/fixtures/pts_wrap_kicad10.README.md`
//! records how it was made and which block carries each case.

use copperpour_schematic_editor::Schematic;

const SHEET: &str = include_str!("fixtures/pts_wrap_kicad10.kicad_sch");

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

#[test]
fn a_sheet_eeschema_wrote_round_trips_byte_for_byte() {
    assert_eq!(round_trip(SHEET), SHEET);
}

/// One wire added: the sheet comes back as it was, plus that wire's block.
#[test]
fn adding_a_wire_writes_only_that_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("edit.kicad_sch");
    std::fs::write(&path, SHEET).expect("write fixture");
    let mut sch = Schematic::load(&path).expect("load");
    sch.add_wire(50.8, 101.6, 76.2, 101.6);
    sch.overwrite().expect("save");
    let after = std::fs::read_to_string(&path).expect("read back");

    let start = after
        .find("\t(wire\n\t\t(pts\n\t\t\t(xy 50.8 101.6) (xy 76.2 101.6)\n")
        .expect("the new wire, both points on one line");
    let end = start + after[start..].find("\n\t)\n").expect("its closing paren") + 4;
    assert_eq!(format!("{}{}", &after[..start], &after[end..]), SHEET);
}
