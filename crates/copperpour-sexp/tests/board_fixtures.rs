//! Board-geometry parsing against KiCad's own output.
//!
//! Two oracles, one rule (the fixture rule this repo learned the hard way:
//! a fixture that shares the code's wrong assumption hides the bug):
//!
//! 1. `tests/fixtures/*.kicad_pcb` — verbatim, unmodified copies of boards
//!    from the KiCad demo corpus, so plain CI without KiCad installed still
//!    exercises the parsers on files pcbnew actually wrote.
//! 2. The installed demo corpus itself (`share/kicad/demos`), which SKIPS
//!    silently when KiCad is absent — same pattern as
//!    `copperpour-core/tests/conformance_test.rs`.
//!
//! Fixture provenance (KiCad 10.0 installer, `C:\KiCad\10.0\share\kicad\demos`):
//! - `ecc83-pp.kicad_pcb` — KiCad 9 format (20241229): net table, numeric net
//!   refs, 59 segments, one GND zone, rectangular gr_line outline.
//! - `RoyalBlue54L-NFC-Antenna.kicad_pcb` — KiCad 9 format: 2 vias, and an
//!   Edge.Cuts outline with 10 `gr_arc` corner fillets/tabs whose extrema
//!   extend past the gr_line hull.
//! - `pic_programmer.kicad_pcb` — KiCad 10 format (20260206): no net table,
//!   `(net "NAME")` in place on segments, vias and zones.
//! - `zone_outline_elements.kicad_pcb` — KiCad 10.0.5-saved reduced board
//!   containing two exact arc-only demo zones and one API-authored mixed path;
//!   see `zone_outline_elements.README.md` for hashes and zone UUIDs.
//! - `gr_poly_outline.kicad_pcb` — KiCad 10-saved minimal board holding a
//!   single 12-vertex, non-rectangular `gr_poly` Edge.Cuts outline copied from
//!   a real project board (#593); see `gr_poly_outline.README.md`.
//! - `footprint_outline_kicad10/` — fifteen boards whose only Edge.Cuts graphics
//!   are footprints', built, turned and flipped by KiCad 10.0.8's pcbnew
//!   (#27); see its README.

use copperpour_sexp::board::{
    board_outline_bbox, count_pads, footprint_courtyards, footprint_outline, footprints,
    lossless_zone_outlines, tracks, vias, zones, ContourRole, CourtyardSource, FootprintCourtyard,
    FootprintOutline, OutlineContour, OutlineOwner, OutlineShape, PcbConnectivityIndex, Side,
    ZoneOutlineElement,
};
use copperpour_sexp::parse_sexp;
use std::collections::HashSet;
use std::path::PathBuf;

fn fixture(name: &str) -> copperpour_sexp::SexpNode {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    parse_sexp(&content).unwrap_or_else(|e| panic!("fixture {name} failed to parse: {e}"))
}

fn assert_bbox_eq(got: (f64, f64, f64, f64), expected: (f64, f64, f64, f64), label: &str) {
    let ok = (got.0 - expected.0).abs() < 1e-6
        && (got.1 - expected.1).abs() < 1e-6
        && (got.2 - expected.2).abs() < 1e-6
        && (got.3 - expected.3).abs() < 1e-6;
    assert!(ok, "{label}: got {got:?}, expected {expected:?}");
}

/// ecc83 (KiCad 9): counts taken from the file itself (`grep -c`), the bbox
/// from its four Edge.Cuts gr_lines.
#[test]
fn ecc83_kicad9_board_geometry() {
    let tree = fixture("ecc83-pp.kicad_pcb");

    let t = tracks(&tree);
    assert_eq!(t.skipped, 0, "pcbnew-authored segments must all parse");
    assert_eq!(t.items.len(), 59);
    for track in &t.items {
        assert!(track.width.is_finite() && track.width > 0.0);
        // Numeric refs must have resolved through the net table to names —
        // an id leaking through as "2" would mean the table lookup is dead.
        if let Some(net) = &track.net {
            assert!(
                net.parse::<u64>().is_err(),
                "net {net:?} looks like an unresolved numeric id"
            );
        }
    }
    // GND itself is only poured (the zone), never routed as segments; the
    // grid net Net-(U1A-G) is, on 17 of the 59 segments.
    assert_eq!(
        t.items
            .iter()
            .filter(|tr| tr.net.as_deref() == Some("Net-(U1A-G)"))
            .count(),
        17
    );

    assert_eq!(
        vias(&tree),
        copperpour_sexp::board::Scan {
            items: vec![],
            skipped: 0
        }
    );

    let z = zones(&tree);
    assert_eq!((z.items.len(), z.skipped), (1, 0));
    assert_eq!(z.items[0].net.as_deref(), Some("GND"));
    assert_eq!(z.items[0].layers, vec!["B.Cu"]);
    assert_eq!(z.items[0].points.len(), 4);

    assert_bbox_eq(
        board_outline_bbox(&tree).expect("ecc83 has an Edge.Cuts outline"),
        (121.285, 90.17, 173.355, 136.525),
        "ecc83 outline",
    );
}

/// NFC antenna (KiCad 9): the outline's top tab is closed by gr_arcs, so the
/// true bbox reaches y = 69.058195 — the gr_lines alone stop at 71.060695.
/// Expected values independently computed (Python) from the file's Edge.Cuts
/// primitives with exact arc extrema.
#[test]
fn nfc_antenna_outline_needs_arc_extrema() {
    let tree = fixture("RoyalBlue54L-NFC-Antenna.kicad_pcb");

    let t = tracks(&tree);
    assert_eq!((t.items.len(), t.skipped), (112, 0));

    let v = vias(&tree);
    assert_eq!((v.items.len(), v.skipped), (2, 0));
    for via in &v.items {
        assert_eq!(via.net.as_deref(), Some("/ANT"));
        assert_eq!(via.layers, vec!["F.Cu", "B.Cu"]);
        assert!((via.size - 1.27).abs() < 1e-9);
        assert!((via.drill - 0.7112).abs() < 1e-9);
    }

    let bbox = board_outline_bbox(&tree).expect("outline present");
    assert_bbox_eq(
        bbox,
        (139.94971, 69.058195, 162.94971, 127.108195),
        "NFC antenna outline",
    );
    // The load-bearing part: the gr_line hull alone would report
    // min_y = 71.060695, 2 mm inside the real board edge.
    assert!(
        bbox.1 < 71.0,
        "min_y {} ignores the arc-closed tab at the top of the outline",
        bbox.1
    );
}

/// pic_programmer (KiCad 10): names in place, no net table to lean on.
#[test]
fn pic_programmer_kicad10_board_geometry() {
    let tree = fixture("pic_programmer.kicad_pcb");

    let t = tracks(&tree);
    assert_eq!((t.items.len(), t.skipped), (370, 0));
    assert!(
        t.items.iter().any(|tr| tr.net.as_deref() == Some("VCC")),
        "pic_programmer routes VCC copper"
    );

    let v = vias(&tree);
    assert_eq!((v.items.len(), v.skipped), (6, 0));
    assert!(v.items.iter().all(|via| via.net.is_some()));

    let z = zones(&tree);
    assert_eq!((z.items.len(), z.skipped), (1, 0));
    assert_eq!(z.items[0].net.as_deref(), Some("GND"));

    assert_bbox_eq(
        board_outline_bbox(&tree).expect("outline present"),
        (73.66, 40.64, 233.68, 139.7),
        "pic_programmer outline",
    );
}

/// Exact bbox for a real, concave `gr_poly` Edge.Cuts outline (#593): the
/// vertex hull is exact for a straight-edged polygon, so this must match the
/// min/max hand-computed from the fixture's own 12 `xy` vertices.
#[test]
fn gr_poly_outline_bbox_matches_hand_computed_extrema() {
    let tree = fixture("gr_poly_outline.kicad_pcb");
    assert_bbox_eq(
        board_outline_bbox(&tree).expect("gr_poly outline present"),
        (103.42, 78.96, 154.48, 116.96),
        "gr_poly_outline bbox",
    );
}

/// KiCad rounds each transformed point to a nanometre, and the arc extrema
/// here come from a circumcentre computed in floating point; ten nanometres
/// covers both, as in `board_bounds.rs`.
const KICAD_NM: f64 = 1e-5;

fn assert_bbox_near(got: (f64, f64, f64, f64), expected: (f64, f64, f64, f64), label: &str) {
    let off = (got.0 - expected.0)
        .abs()
        .max((got.1 - expected.1).abs())
        .max((got.2 - expected.2).abs())
        .max((got.3 - expected.3).abs());
    assert!(
        off < KICAD_NM,
        "{label}: got {got:?}, expected {expected:?}"
    );
}

/// One of the boards `footprint_outline_kicad10/generate.py` builds with
/// KiCad 10.0.8's pcbnew; the expected values are KiCad's reading of each, in
/// that directory's README.
fn footprint_outline_fixture(name: &str) -> FootprintOutline {
    footprint_outline(&fixture(&format!(
        "footprint_outline_kicad10/{name}.kicad_pcb"
    )))
}

fn recognized(name: &str) -> (OutlineShape, Vec<OutlineOwner>, Vec<OutlineContour>) {
    match footprint_outline_fixture(name) {
        FootprintOutline::Recognized {
            shape,
            owners,
            contours,
        } => (shape, owners, contours),
        other => panic!("{name}: expected a recognized outline, got {other:?}"),
    }
}

fn owner<'a>(owners: &'a [OutlineOwner], reference: &str) -> &'a OutlineOwner {
    owners
        .iter()
        .find(|o| o.reference.as_deref() == Some(reference))
        .unwrap_or_else(|| panic!("no owner {reference} in {owners:?}"))
}

/// Issue #27's MECH1: a padless footprint at (100, 100), 0°, whose 27
/// Edge.Cuts primitives are the whole outline, beside SLOT1, whose rectangle
/// KiCad cuts as a hole because its pads lie outside it.
#[test]
fn mech1_outline_is_read_from_its_footprint_as_kicad_reads_it() {
    let (shape, owners, contours) = recognized("mech1");

    let mech1 = owner(&owners, "MECH1");
    assert_eq!(
        (
            mech1.pads,
            mech1.primitives,
            mech1.cutouts_only,
            mech1.draws_external
        ),
        (0, 27, false, true)
    );
    let slot = owner(&owners, "SLOT1");
    assert_eq!(
        (slot.pads, slot.primitives, slot.cutouts_only),
        (2, 1, true)
    );

    let roles: Vec<_> = contours.iter().map(|c| (c.role, c.primitives)).collect();
    assert_eq!(
        roles,
        vec![
            (ContourRole::External, 19),
            (ContourRole::Cutout, 8),
            (ContourRole::Cutout, 1),
        ]
    );
    assert_bbox_near(contours[0].bbox, (30.0, 50.0, 170.0, 150.0), "MECH1 outer");
    assert_bbox_near(
        contours[1].bbox,
        (67.875, 111.3335, 92.125, 118.6665),
        "MECH1 cutout",
    );
    let (x0, y0, x1, y1) = contours[1].bbox;
    assert!((x1 - x0 - 24.25).abs() < KICAD_NM && (y1 - y0 - 7.333).abs() < KICAD_NM);
    assert_bbox_near(contours[2].bbox, (137.0, 69.0, 143.0, 71.0), "SLOT1");

    let OutlineShape::Unproven { bbox } = shape else {
        panic!("arcs and cut-outs cannot be a proven rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (30.0, 50.0, 170.0, 150.0), "outline");
}

/// The same MECH1 turned 30°, and flipped to the back and turned 150°, by
/// pcbnew. Neither contour is symmetric, so a wrong rotation sense or a
/// mirror applied twice moves the cut-out away from where KiCad put it. The
/// flipped one holds two mounting holes, which are pads but not electrical.
#[test]
fn a_turned_and_a_flipped_mech1_land_where_kicad_puts_them() {
    let (_, owners, _) = recognized("mech1_back");
    let mech1 = owner(&owners, "MECH1");
    assert_eq!(
        (mech1.pads, mech1.electrical_pads, mech1.cutouts_only),
        (2, 0, false)
    );
    for (name, outer, cutout) in [
        (
            "mech1_rotated",
            (15.476298, 22.796806, 184.523702, 177.203194),
            (78.21171, 114.118624, 102.147274, 131.862137),
        ),
        (
            "mech1_back",
            (15.476298, 22.796806, 184.523701, 177.203193),
            (97.852725, 114.118624, 121.788291, 131.862136),
        ),
    ] {
        let (_, owners, contours) = recognized(name);
        assert_eq!(owner(&owners, "MECH1").primitives, 27, "{name}");
        let roles: Vec<_> = contours.iter().map(|c| (c.role, c.primitives)).collect();
        assert_eq!(
            roles,
            vec![(ContourRole::External, 19), (ContourRole::Cutout, 8)],
            "{name}"
        );
        assert_bbox_near(contours[0].bbox, outer, &format!("{name} outer"));
        assert_bbox_near(contours[1].bbox, cutout, &format!("{name} cutout"));
    }
}

/// MECH1 without its right edge: KiCad builds no outline and its DRC reports
/// `invalid_outline`. The reading refuses, and says where the gap is.
#[test]
fn an_open_footprint_outline_is_refused_with_its_open_end() {
    let FootprintOutline::Refused { owners, reason } = footprint_outline_fixture("open_outline")
    else {
        panic!("an open outline must not be recognized");
    };
    assert_eq!(owner(&owners, "MECH1").primitives, 26);
    assert!(reason.contains("open end at (170.000, "), "{reason}");
}

/// Ends within KiCad's 0.01 mm chaining tolerance meet: every MECH1 board's
/// cut-out has a 0.005 mm gap, and closes. `gap_outline`'s is 0.02 mm, and
/// KiCad builds no outline and reports `invalid_outline`.
#[test]
fn ends_meet_within_kicads_chaining_tolerance_and_not_beyond() {
    let (_, _, contours) = recognized("mech1");
    assert_eq!(
        (contours[1].role, contours[1].primitives),
        (ContourRole::Cutout, 8),
        "a 0.005 mm gap closes"
    );
    let FootprintOutline::Refused { reason, .. } = footprint_outline_fixture("gap_outline") else {
        panic!("a 0.02 mm gap must not close");
    };
    assert!(reason.contains("open end at (67.875, "), "{reason}");
}

/// MECH1 with a 0.004 mm segment cut off its top edge: both of its ends lie
/// within the tolerance of the next piece's, and KiCad chains on to the
/// nearest end, closing the outline. So does this reading.
#[test]
fn a_segment_shorter_than_the_tolerance_still_chains() {
    let (_, owners, contours) = recognized("short_segment");
    assert_eq!(owner(&owners, "MECH1").primitives, 28);
    let roles: Vec<_> = contours.iter().map(|c| (c.role, c.primitives)).collect();
    assert_eq!(
        roles,
        vec![(ContourRole::External, 20), (ContourRole::Cutout, 8)]
    );
    assert_bbox_near(contours[0].bbox, (30.0, 50.0, 170.0, 150.0), "outer");
}

/// SLOT1 alone: KiCad cuts it as a hole and builds no outline, so a slot with
/// pads around it never becomes the board.
#[test]
fn a_slot_whose_pads_lie_outside_it_is_not_an_outline() {
    let FootprintOutline::Refused { owners, reason } = footprint_outline_fixture("slot_only")
    else {
        panic!("a hole alone must not be recognized as an outline");
    };
    assert!(owner(&owners, "SLOT1").cutouts_only);
    assert!(reason.contains("cut-outs only"), "{reason}");
}

/// SLOTPAD alone, a slot inside its own pad: the pad's copper meets the slot,
/// so KiCad keeps the slot in contention, and with nothing else it is the
/// outline. No corner or centre of the pad lies in the slot; the slot's own
/// points lie in the pad.
#[test]
fn a_slot_inside_its_pad_is_not_a_hole() {
    let (shape, owners, _) = recognized("slot_in_pad");
    assert!(!owner(&owners, "SLOTPAD").cutouts_only);
    let OutlineShape::Rectangular { bbox } = shape else {
        panic!("one rectangle is a proven rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (101.0, 99.5, 102.5, 100.5), "slot");
}

/// SLOTX alone, a slot across its own pad: neither holds a corner of the
/// other, yet the slot's edges cross the pad, so KiCad keeps the slot, and
/// it is the outline.
#[test]
fn a_slot_across_its_pad_is_not_a_hole() {
    let (shape, owners, _) = recognized("slot_across_pad");
    assert!(!owner(&owners, "SLOTX").cutouts_only);
    let OutlineShape::Rectangular { bbox } = shape else {
        panic!("one rectangle is a proven rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (95.0, 100.5, 105.0, 101.0), "slot");
}

/// SLOTB alone, flipped and turned 30°: a slot beside its pad, off the
/// footprint's origin. KiCad cuts it as a hole. A pad placed by a mirror or a
/// half turn too many would land in the slot and keep it.
#[test]
fn a_flipped_slot_whose_pad_lies_outside_it_is_not_an_outline() {
    let FootprintOutline::Refused { owners, reason } = footprint_outline_fixture("slot_back")
    else {
        panic!("a hole alone must not be recognized as an outline");
    };
    assert!(owner(&owners, "SLOTB").cutouts_only);
    assert!(reason.contains("cut-outs only"), "{reason}");
}

/// SLOTS2 alone, a circle and a polygon and no pads: more than one outline,
/// which KiCad cuts as holes, building none.
#[test]
fn a_footprint_with_two_outlines_cuts_holes() {
    let FootprintOutline::Refused { owners, reason } = footprint_outline_fixture("two_slots_only")
    else {
        panic!("two slots alone must not be recognized as an outline");
    };
    let slots = owner(&owners, "SLOTS2");
    assert_eq!(
        (slots.pads, slots.primitives, slots.cutouts_only),
        (0, 2, true)
    );
    assert!(reason.contains("cut-outs only"), "{reason}");
}

/// CURVE1, a square with a Bézier side: KiCad builds it as an outline, and
/// this reading, which does not chain curves, refuses it by name.
#[test]
fn a_curve_on_edge_cuts_is_refused_by_name() {
    let FootprintOutline::Refused { reason, .. } = footprint_outline_fixture("curve_outline")
    else {
        panic!("a curve must not be read as a straight edge or skipped");
    };
    assert_eq!(
        reason,
        "CURVE1 draws an fp_curve, which this reading does not chain"
    );
}

/// A card-edge connector whose four Edge.Cuts lines are the card, its fingers
/// inside them, flipped to the back and turned 90°: KiCad's one outline, and a
/// provable rectangle.
#[test]
fn a_card_edge_footprint_with_pads_inside_is_a_rectangular_outline() {
    let (shape, owners, contours) = recognized("card_edge");
    let j1 = owner(&owners, "J1");
    assert_eq!(
        (j1.electrical_pads, j1.primitives, j1.draws_external),
        (5, 4, true)
    );
    // SLOT1 lies off the card: KiCad drops a hole outside every outline.
    let slot = owner(&owners, "SLOT1");
    assert!(slot.cutouts_only && !slot.draws_external);
    assert_eq!(contours.len(), 1);
    let OutlineShape::Rectangular { bbox } = shape else {
        panic!("four sides of a turned rectangle are still a rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (80.0, 85.0, 120.0, 115.0), "card");
}

/// The same card with SLOT1 inside it: KiCad keeps the hole, one outline
/// with one hole. A part over the hole lies inside the card's bbox, so a
/// rectangle with a hole in it is not a proven rectangle.
#[test]
fn a_card_edge_outline_with_a_hole_in_it_is_not_a_proven_rectangle() {
    let (shape, owners, contours) = recognized("card_edge_hole");
    let slot = owner(&owners, "SLOT1");
    assert!(slot.cutouts_only && !slot.draws_external);
    let roles: Vec<_> = contours.iter().map(|c| (c.role, c.primitives)).collect();
    assert_eq!(
        roles,
        vec![(ContourRole::External, 4), (ContourRole::Cutout, 1)]
    );
    assert_bbox_near(contours[1].bbox, (97.0, 91.0, 103.0, 93.0), "SLOT1");
    let OutlineShape::Unproven { bbox } = shape else {
        panic!("a rectangle with a hole in it cannot be a proven rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (80.0, 85.0, 120.0, 115.0), "card");
}

/// HALF_A draws the top and right of a 40 x 30 mm outline and HALF_B its
/// bottom and left; neither closes alone, so KiCad chains them together.
/// SLOTN, a padless slot inside, closes alone and becomes the outline's hole:
/// it draws no part of the board's edge.
#[test]
fn an_outline_drawn_by_two_footprints_chains_across_them() {
    let (_, owners, contours) = recognized("split_outline");
    let index = |reference: &str| {
        owners
            .iter()
            .position(|o| o.reference.as_deref() == Some(reference))
            .unwrap()
    };
    let mut halves = vec![index("HALF_A"), index("HALF_B")];
    halves.sort_unstable();
    assert_eq!(contours.len(), 2);
    assert_eq!(
        (
            contours[0].role,
            contours[0].primitives,
            &contours[0].owners
        ),
        (ContourRole::External, 4, &halves)
    );
    assert_bbox_near(contours[0].bbox, (80.0, 85.0, 120.0, 115.0), "halves");
    assert_eq!(
        (contours[1].role, &contours[1].owners),
        (ContourRole::Cutout, &vec![index("SLOTN")])
    );
    assert!(owner(&owners, "HALF_A").draws_external && owner(&owners, "HALF_B").draws_external);
    let slot = owner(&owners, "SLOTN");
    assert!(!slot.draws_external && !slot.cutouts_only);
}

/// RRECT1, one `fp_rect` with 3 mm corners, turned 90°: KiCad's one outline.
/// Its corners are arcs, so it is not a proven rectangle.
#[test]
fn a_rectangle_with_rounded_corners_is_not_a_proven_rectangle() {
    let (shape, _, contours) = recognized("rounded_rect");
    assert_eq!(contours.len(), 1);
    let OutlineShape::Unproven { bbox } = shape else {
        panic!("rounded corners cannot be a proven rectangle: {shape:?}");
    };
    assert_bbox_near(bbox, (85.0, 80.0, 115.0, 120.0), "rounded rectangle");
}

/// A board that draws its own Edge.Cuts keeps `board_outline_shape`'s
/// reading; the footprint reading does not apply. `board_outline_kicad10`
/// draws its outline and also holds U1, whose Edge.Cuts rectangle closes
/// around its pad, which alone would be read as an outline.
#[test]
fn a_board_with_its_own_edge_cuts_is_not_read_from_footprints() {
    let both = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../copperpour-core/tests/fixtures/board_outline_kicad10.kicad_pcb");
    let both = parse_sexp(&std::fs::read_to_string(&both).unwrap()).unwrap();
    assert_eq!(footprint_outline(&both), FootprintOutline::NotApplicable);
    for name in ["gr_poly_outline.kicad_pcb", "pic_programmer.kicad_pcb"] {
        assert_eq!(
            footprint_outline(&fixture(name)),
            FootprintOutline::NotApplicable,
            "{name}"
        );
    }
}

#[test]
fn zone_outline_elements_are_complete_and_ordered() {
    let tree = fixture("zone_outline_elements.kicad_pcb");
    let scan = lossless_zone_outlines(&tree);
    assert_eq!((scan.items.len(), scan.skipped), (3, 0));

    let mixed = &scan.items[0];
    assert_eq!(mixed.net, None);
    assert_eq!(mixed.layers, vec!["F.Cu"]);
    assert_eq!(
        mixed.elements,
        vec![
            ZoneOutlineElement::Point((145.0, 100.0)),
            ZoneOutlineElement::Point((150.0, 100.0)),
            ZoneOutlineElement::Arc {
                start: (149.28, 105.91),
                mid: (149.206777, 106.086777),
                end: (149.03, 106.16),
            },
            ZoneOutlineElement::Point((145.0, 107.0)),
        ]
    );

    let vsys = &scan.items[1];
    assert_eq!(vsys.net.as_deref(), Some("VSYS"));
    assert_eq!(vsys.layers, vec!["B.Cu"]);
    assert_eq!(
        vsys.elements,
        vec![
            ZoneOutlineElement::Arc {
                start: (149.28, 105.91),
                mid: (149.206777, 106.086777),
                end: (149.03, 106.16),
            },
            ZoneOutlineElement::Arc {
                start: (147.13, 106.16),
                mid: (146.953223, 106.233223),
                end: (146.88, 106.41),
            },
            ZoneOutlineElement::Arc {
                start: (146.88, 108.81),
                mid: (146.953223, 108.986777),
                end: (147.13, 109.06),
            },
            ZoneOutlineElement::Arc {
                start: (149.93, 109.06),
                mid: (150.106777, 108.986777),
                end: (150.18, 108.81),
            },
            ZoneOutlineElement::Arc {
                start: (150.18, 99.41),
                mid: (150.106777, 99.233223),
                end: (149.93, 99.16),
            },
            ZoneOutlineElement::Arc {
                start: (147.83, 99.16),
                mid: (147.653223, 99.233223),
                end: (147.58, 99.41),
            },
            ZoneOutlineElement::Arc {
                start: (147.58, 102.61),
                mid: (147.653223, 102.786777),
                end: (147.83, 102.86),
            },
            ZoneOutlineElement::Arc {
                start: (149.03, 102.86),
                mid: (149.206777, 102.933223),
                end: (149.28, 103.11),
            },
        ]
    );

    let batt = &scan.items[2];
    assert_eq!(batt.net.as_deref(), Some("+BATT"));
    assert_eq!(batt.layers, vec!["In2.Cu"]);
    assert_eq!(
        batt.elements,
        vec![
            ZoneOutlineElement::Arc {
                start: (144.26, 95.23),
                mid: (144.552893, 95.937107),
                end: (145.26, 96.23),
            },
            ZoneOutlineElement::Arc {
                start: (148.545786, 96.23),
                mid: (148.928469, 96.30612),
                end: (149.252893, 96.522893),
            },
            ZoneOutlineElement::Arc {
                start: (150.167107, 97.437107),
                mid: (150.38388, 97.76153),
                end: (150.46, 98.144214),
            },
            ZoneOutlineElement::Arc {
                start: (150.46, 102.13),
                mid: (150.167107, 102.837107),
                end: (149.46, 103.13),
            },
            ZoneOutlineElement::Arc {
                start: (138.36, 103.13),
                mid: (137.652893, 102.837107),
                end: (137.36, 102.13),
            },
            ZoneOutlineElement::Arc {
                start: (137.36, 94.73),
                mid: (137.652893, 94.022893),
                end: (138.36, 93.73),
            },
            ZoneOutlineElement::Arc {
                start: (143.26, 93.73),
                mid: (143.967107, 94.022893),
                end: (144.26, 94.73),
            },
        ]
    );

    // The old point-only scanner retains its exact public shape and projects
    // only the mixed outline's straight vertices. Arc-only zones still enter
    // its historical skip bucket; callers needing them opt into the lossless
    // scanner above.
    let compatibility = zones(&tree);
    assert_eq!((compatibility.items.len(), compatibility.skipped), (1, 2));
    assert_eq!(
        compatibility.items[0].points,
        vec![(145.0, 100.0), (150.0, 100.0), (145.0, 107.0)]
    );
}

/// Totality over the fixture corpus: every fixture parses and every scan runs
/// without panicking, whatever else the files contain.
#[test]
fn fixture_corpus_scans_are_total() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut seen = 0usize;
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "kicad_pcb") {
            continue;
        }
        seen += 1;
        let content = std::fs::read_to_string(&path).expect("readable fixture");
        let tree = parse_sexp(&content)
            .unwrap_or_else(|e| panic!("{} failed to parse: {e}", path.display()));
        for track in &tracks(&tree).items {
            assert!(track.width.is_finite() && track.width > 0.0);
            for c in [track.x1, track.y1, track.x2, track.y2] {
                assert!(c.is_finite());
            }
        }
        let _ = vias(&tree);
        let _ = zones(&tree);
        let _ = lossless_zone_outlines(&tree);
        let _ = board_outline_bbox(&tree);
    }
    assert_eq!(seen, 5, "expected the five committed board fixtures");
}

// ─── Courtyards: hand-computed ground truth ──────────────────────────────────

fn courtyard_of<'a>(scan: &'a [FootprintCourtyard], reference: &str) -> &'a FootprintCourtyard {
    scan.iter()
        .find(|c| c.reference.as_deref() == Some(reference))
        .unwrap_or_else(|| panic!("no courtyard entry for {reference}"))
}

/// ecc83 courtyards against numbers computed by hand from the file text
/// (root `(at …)` plus the F.CrtYd artwork coordinates), never from the code
/// under test.
///
/// C1 (rotated 90°): root `(at 141.605 99.695 90)`; its courtyard is a single
/// `fp_circle` center (2.5, 0), end (7.75, 0) → r = 5.25, local bbox
/// (−2.75, −5.25, 7.75, 5.25). KiCAD's Y-down CCW rotation at 90° maps
/// (x, y) → (y, −x), so the corners land on (∓5.25, ±2.75 / ∓7.75) with hull
/// (−5.25, −7.75, 5.25, 2.75); plus the anchor:
/// (136.355, 91.945, 146.855, 102.445).
///
/// R1 (rotated −90°): root `(at 136.271 107.95 -90)`, local courtyard hull
/// (−1.05, −1.5, 8.67, 1.5) from its four fp_lines. At −90°, (x, y) → (−y, x):
/// hull (−1.5, −1.05, 1.5, 8.67); plus the anchor:
/// (134.771, 106.9, 137.771, 116.62). The same transform puts R1 pad 2
/// (local (7.62, 0)) at (136.271, 115.57) — where the file has a routed
/// segment of that pad's net starting, which is what pins the convention to
/// ground truth rather than to another calculation.
#[test]
fn ecc83_courtyards_match_hand_computed_transforms() {
    let tree = fixture("ecc83-pp.kicad_pcb");
    let scan = footprint_courtyards(&tree);
    assert_eq!(scan.skipped, 0, "pcbnew-authored footprints must all parse");
    assert_eq!(scan.items.len(), 15); // grep -c '(footprint ' = 15
    assert!(scan
        .items
        .iter()
        .all(|c| c.bbox_source == CourtyardSource::Courtyard));

    let c1 = courtyard_of(&scan.items, "C1");
    assert_eq!(c1.rotation_deg, 90.0);
    assert_eq!(c1.layer_side, Side::Front);
    assert_eq!(c1.at, (141.605, 99.695));
    assert_bbox_eq(c1.bbox, (136.355, 91.945, 146.855, 102.445), "C1 rot 90");

    let r1 = courtyard_of(&scan.items, "R1");
    assert_eq!(r1.rotation_deg, -90.0);
    assert_bbox_eq(r1.bbox, (134.771, 106.9, 137.771, 116.62), "R1 rot -90");
}

/// pic_programmer's JP1 is the corpus's back-side footprint: `(layer "B.Cu")`,
/// root `(at 148.082 97.79)`, courtyard hull (−1.65, −1.25, 1.65, 1.25) from
/// four B.CrtYd fp_lines. The file stores back-side children already flipped,
/// so the bbox is the plain translation (146.432, 96.54, 149.732, 99.04) —
/// verified against routed copper: the B.Cu VCC track ends at (147.447,
/// 97.79), inside pad 1 whose anchor this transform puts at (147.357, 97.79);
/// mirroring the footprint would move that pad to 148.807, the wrong side.
#[test]
fn pic_programmer_back_side_footprint_is_stored_preflipped() {
    let tree = fixture("pic_programmer.kicad_pcb");
    let scan = footprint_courtyards(&tree);
    assert_eq!(scan.skipped, 0);
    assert_eq!(scan.items.len(), 63); // grep -c '(footprint ' = 63

    let jp1 = courtyard_of(&scan.items, "JP1");
    assert_eq!(jp1.layer_side, Side::Back);
    assert_eq!(jp1.rotation_deg, 0.0);
    assert_bbox_eq(jp1.bbox, (146.432, 96.54, 149.732, 99.04), "JP1 back side");

    // And the pad the copper pinned: JP1 pad 1 (net VCC) at 148.082 − 0.725.
    let ix = PcbConnectivityIndex::build(&tree);
    let vcc_pad = ix
        .pads_of_net("VCC")
        .iter()
        .find(|p| p.reference == "JP1")
        .expect("JP1 pad 1 is on VCC");
    assert!((vcc_pad.at.0 - 147.357).abs() < 1e-9 && (vcc_pad.at.1 - 97.79).abs() < 1e-9);
    assert_eq!(vcc_pad.layer_side, Side::Back);
}

/// The NFC antenna board is the fallback corpus: neither footprint draws a
/// courtyard. J1 falls back to its pads — two 0.3×1.5 pads at local
/// (±0.5, 0.75), root `(at 151.44971 66.560695)`, so the hull is
/// (−0.65, 0, 0.65, 1.5) translated: (150.79971, 66.560695, 152.09971,
/// 68.060695). The logo footprint has no pads either and degrades to its
/// anchor. Nothing is silently absent: 2 footprints, 2 entries, 0 skipped.
#[test]
fn nfc_antenna_footprints_fall_back_when_courtyards_are_missing() {
    let tree = fixture("RoyalBlue54L-NFC-Antenna.kicad_pcb");
    let scan = footprint_courtyards(&tree);
    assert_eq!((scan.items.len(), scan.skipped), (2, 0));

    let j1 = courtyard_of(&scan.items, "J1");
    assert_eq!(j1.bbox_source, CourtyardSource::PadsFallback);
    assert_bbox_eq(
        j1.bbox,
        (150.79971, 66.560695, 152.09971, 68.060695),
        "J1 pads fallback",
    );

    let logo = courtyard_of(&scan.items, "REF**");
    assert_eq!(logo.bbox_source, CourtyardSource::AnchorOnly);
    assert_bbox_eq(
        logo.bbox,
        (153.1125, 118.976103, 153.1125, 118.976103),
        "logo anchor",
    );
}

// ─── Connectivity index: pinned counts ───────────────────────────────────────
//
// Every expected number below was derived OUTSIDE the code under test — an
// independent Python s-expression walker over the raw fixture text (and
// grep for the footprint counts) — never by running the index and copying
// its output.

/// ecc83: 33 pads on 15 footprints, all netted, 13 distinct nets across pads
/// and segments (independently counted from the file's net references).
#[test]
fn ecc83_index_pinned_counts() {
    let tree = fixture("ecc83-pp.kicad_pcb");
    let ix = PcbConnectivityIndex::build(&tree);
    assert_eq!((ix.skipped_pads(), ix.skipped_tracks()), (0, 0));

    let nets = ix.nets();
    assert_eq!(nets.len(), 13);
    let total: usize = nets.iter().map(|n| ix.pads_of_net(n).len()).sum();
    assert_eq!(total, 33);
    assert_eq!(ix.pads_without_net().len(), 0);

    // Per-net spot checks, counted by hand from the pad (net …) nodes.
    assert_eq!(ix.pads_of_net("GND").len(), 7);
    assert_eq!(ix.net_of_pad("R1", "1"), Some("Net-(U1A-G)"));
    assert_eq!(ix.net_of_pad("R1", "2"), Some("Net-(U1A-K)"));
    // The 17 Net-(U1A-G) segments were pinned in the T0.3a track tests; the
    // index must agree with the same scan, not re-derive it.
    assert_eq!(ix.segments_of_net("Net-(U1A-G)").len(), 17);

    // R1 pad 2 through (136.271, 107.95, −90°): local (7.62, 0) → (136.271,
    // 115.57), where the file has a routed Net-(U1A-K) segment starting.
    let k = ix.pads_of_net("Net-(U1A-K)");
    let p = k.iter().find(|p| p.reference == "R1").expect("R1 pad 2");
    assert!((p.at.0 - 136.271).abs() < 1e-9 && (p.at.1 - 115.57).abs() < 1e-9);
}

/// pic_programmer (KiCad 10): 247 pads on 63 footprints — 236 netted, 11 on
/// no net — over 111 distinct nets; VCC pads 12 of them.
#[test]
fn pic_programmer_index_pinned_counts() {
    let tree = fixture("pic_programmer.kicad_pcb");
    let ix = PcbConnectivityIndex::build(&tree);
    assert_eq!((ix.skipped_pads(), ix.skipped_tracks()), (0, 0));

    let nets = ix.nets();
    assert_eq!(nets.len(), 111);
    let netted: usize = nets.iter().map(|n| ix.pads_of_net(n).len()).sum();
    assert_eq!(netted, 236);
    assert_eq!(ix.pads_without_net().len(), 11);
    assert_eq!(ix.pads_of_net("VCC").len(), 12);
    assert_eq!(
        ix.net_of_pad("JP1", "2"),
        Some("/pic_sockets/VCC_PIC"),
        "JP1 pad 2 net, read straight from the file"
    );
}

/// NFC antenna: 2 pads, both on /ANT — the board's only net.
#[test]
fn nfc_antenna_index_pinned_counts() {
    let tree = fixture("RoyalBlue54L-NFC-Antenna.kicad_pcb");
    let ix = PcbConnectivityIndex::build(&tree);
    assert_eq!((ix.skipped_pads(), ix.skipped_tracks()), (0, 0));
    assert_eq!(ix.nets(), vec!["/ANT"]);
    assert_eq!(ix.pads_of_net("/ANT").len(), 2);
    assert_eq!(ix.pads_without_net().len(), 0);
    // J1 pad 1: local (−0.5, 0.75) + (151.44971, 66.560695), unrotated.
    let p = &ix.pads_of_net("/ANT")[0];
    assert!((p.at.0 - 150.94971).abs() < 1e-9 && (p.at.1 - 67.310695).abs() < 1e-9);
}

// ─── Connectivity index: invariants (proptest over the fixtures) ─────────────

/// The references a board's footprints carry, extracted directly in the test
/// (the index's own extraction is what is under test).
fn reference_set(tree: &copperpour_sexp::SexpNode) -> HashSet<String> {
    let mut out = HashSet::new();
    for fp in footprints(tree) {
        for prop in fp.find_all("property") {
            if prop.get(1).and_then(|n| n.as_str()) == Some("Reference") {
                if let Some(r) = prop.get(2).and_then(|n| n.as_str()) {
                    out.insert(r.to_string());
                }
            }
        }
        for text in fp.find_all("fp_text") {
            if text.get(1).and_then(|n| n.as_str()) == Some("reference") {
                if let Some(r) = text.get(2).and_then(|n| n.as_str()) {
                    out.insert(r.to_string());
                }
            }
        }
    }
    out
}

/// Index invariants that must hold on any board. `strict_net_of_pad` demands
/// `net_of_pad(p) == Some(n)` for every pad of every net — true wherever no
/// two same-numbered pads sit on different nets (all three fixtures; some
/// demo boards break it with per-hole `unconnected-…_N` nets, so the demo
/// oracle uses the membership form instead).
fn assert_index_invariants(tree: &copperpour_sexp::SexpNode, label: &str, strict_net_of_pad: bool) {
    let ix = PcbConnectivityIndex::build(tree);
    let refs = reference_set(tree);

    // 1. Every PadSite's reference exists among the board's footprints.
    let all_sites = || {
        ix.nets()
            .into_iter()
            .flat_map(|n| ix.pads_of_net(n).iter())
            .chain(ix.pads_without_net().iter())
    };
    for site in all_sites() {
        assert!(
            refs.contains(&site.reference),
            "{label}: PadSite references {} which no footprint carries",
            site.reference
        );
        assert!(site.at.0.is_finite() && site.at.1.is_finite());
    }

    // 2. pads_of_net ∪ pads_without_net partitions all readable pads.
    let netted: usize = ix.nets().iter().map(|n| ix.pads_of_net(n).len()).sum();
    assert_eq!(
        netted + ix.pads_without_net().len() + ix.skipped_pads(),
        count_pads(tree),
        "{label}: netted + unconnected + skipped must cover every pad node"
    );
    let key = |p: &copperpour_sexp::board::PadSite| {
        (
            p.reference.clone(),
            p.pad_number.clone(),
            p.at.0.to_bits(),
            p.at.1.to_bits(),
        )
    };
    let with_net: HashSet<_> = ix
        .nets()
        .into_iter()
        .flat_map(|n| ix.pads_of_net(n).iter().map(key))
        .collect();
    assert!(
        !ix.pads_without_net()
            .iter()
            .map(key)
            .any(|k| with_net.contains(&k)),
        "{label}: a pad is indexed both under a net and as unconnected"
    );

    // 3. net_of_pad is consistent with pads_of_net.
    for net in ix.nets() {
        for pad in ix.pads_of_net(net) {
            let got = ix.net_of_pad(&pad.reference, &pad.pad_number);
            if strict_net_of_pad {
                assert_eq!(
                    got,
                    Some(net),
                    "{label}: net_of_pad({}, {}) disagrees with pads_of_net",
                    pad.reference,
                    pad.pad_number
                );
            } else {
                let m = got.unwrap_or_else(|| {
                    panic!(
                        "{label}: pad {}:{} indexed under {net} but net_of_pad knows it not",
                        pad.reference, pad.pad_number
                    )
                });
                assert!(
                    ix.pads_of_net(m)
                        .iter()
                        .any(|q| q.reference == pad.reference && q.pad_number == pad.pad_number),
                    "{label}: net_of_pad answer {m} holds no such pad"
                );
            }
        }
    }
    // …and unconnected pads never get a net answer through some other pad of
    // the same footprint+number (fixtures only — demo jumper footprints may
    // legitimately share a number between netted and unconnected pads).
    if strict_net_of_pad {
        for pad in ix.pads_without_net() {
            assert_eq!(
                ix.net_of_pad(&pad.reference, &pad.pad_number),
                None,
                "{label}: unconnected pad {}:{} reports a net",
                pad.reference,
                pad.pad_number
            );
        }
    }
}

static FIXTURE_TREES: std::sync::OnceLock<Vec<(&'static str, copperpour_sexp::SexpNode)>> =
    std::sync::OnceLock::new();

fn fixture_trees() -> &'static [(&'static str, copperpour_sexp::SexpNode)] {
    FIXTURE_TREES.get_or_init(|| {
        [
            "ecc83-pp.kicad_pcb",
            "pic_programmer.kicad_pcb",
            "RoyalBlue54L-NFC-Antenna.kicad_pcb",
        ]
        .into_iter()
        .map(|name| (name, fixture(name)))
        .collect()
    })
}

proptest::proptest! {
    /// The invariants hold for the whole corpus, and for every net the index
    /// claims: pads_of_net answers are stable, in-corpus, and non-empty
    /// exactly when the net has pads.
    #[test]
    fn index_invariants_hold_across_the_fixture_corpus(
        fixture_ix in 0usize..3,
        net_pick in proptest::num::usize::ANY,
    ) {
        let (name, tree) = &fixture_trees()[fixture_ix];
        assert_index_invariants(tree, name, true);

        let ix = PcbConnectivityIndex::build(tree);
        let nets = ix.nets();
        proptest::prop_assume!(!nets.is_empty());
        let net = nets[net_pick % nets.len()];
        // A net the index reports must have a pad or a segment (that is what
        // being in the index means)…
        proptest::prop_assert!(
            !ix.pads_of_net(net).is_empty() || !ix.segments_of_net(net).is_empty()
        );
        // …and never the pseudo-net.
        proptest::prop_assert!(!net.is_empty());
    }
}

// ─── Installed-demo oracle (skips without KiCad) ─────────────────────────────

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

fn collect_boards(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "kicad_pcb") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Every board pcbnew ships must scan losslessly: `skipped == 0` everywhere,
/// every track sane. This is the conformance oracle for the malformed-item
/// policy itself — if a KiCad-authored file trips the skip path, the *scan*
/// is what's malformed, not the board.
#[test]
fn every_installed_demo_board_scans_losslessly() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found (set KICAD_DEMOS to enable)");
        return;
    };
    let boards = collect_boards(&root);
    assert!(
        !boards.is_empty(),
        "demo dir exists but contains no .kicad_pcb files: {}",
        root.display()
    );

    let (mut n_tracks, mut n_vias, mut n_zones, mut n_bboxes) = (0usize, 0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for board in &boards {
        let content = std::fs::read_to_string(board).unwrap_or_default();
        let tree = match parse_sexp(&content) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{}: parse: {e}", board.display()));
                continue;
            }
        };
        let t = tracks(&tree);
        let v = vias(&tree);
        let z = lossless_zone_outlines(&tree);
        if t.skipped + v.skipped + z.skipped > 0 {
            failures.push(format!(
                "{}: skipped {} segments / {} vias / {} zones from a pcbnew-authored board",
                board.display(),
                t.skipped,
                v.skipped,
                z.skipped
            ));
        }
        for track in &t.items {
            if !(track.width.is_finite() && track.width > 0.0) {
                failures.push(format!(
                    "{}: track with width {}",
                    board.display(),
                    track.width
                ));
            }
        }
        n_tracks += t.items.len();
        n_vias += v.items.len();
        n_zones += z.items.len();
        if let Some((x0, y0, x1, y1)) = board_outline_bbox(&tree) {
            n_bboxes += 1;
            if !(x0 <= x1 && y0 <= y1) {
                failures.push(format!("{}: inverted bbox", board.display()));
            }
        }
    }
    eprintln!(
        "scanned {} demo boards: {n_tracks} tracks, {n_vias} vias, {n_zones} zones, {n_bboxes} outlines",
        boards.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Guard against a scan that quietly stops matching and passes vacuously.
    assert!(n_tracks > 500, "suspiciously few tracks ({n_tracks})");
    assert!(n_vias >= 8, "suspiciously few vias ({n_vias})");
    assert!(n_zones >= 2, "suspiciously few zones ({n_zones})");
    assert!(n_bboxes >= 3, "suspiciously few outlines ({n_bboxes})");
}

/// Courtyards and the connectivity index over every installed demo board:
/// each board indexes without loss, the index invariants hold (membership
/// form of net_of_pad — some demo footprints put same-numbered pads on
/// distinct per-hole `unconnected-…` nets), and every footprint yields a
/// courtyard entry with a well-formed bbox.
#[test]
fn every_installed_demo_board_indexes_consistently() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found (set KICAD_DEMOS to enable)");
        return;
    };
    let boards = collect_boards(&root);
    assert!(!boards.is_empty());

    let (mut n_pads, mut n_nets, mut n_courtyards, mut n_fallbacks) = (0usize, 0, 0, 0);
    let mut failures = Vec::new();
    for board in &boards {
        let content = std::fs::read_to_string(board).unwrap_or_default();
        let Ok(tree) = parse_sexp(&content) else {
            failures.push(format!("{}: parse failure", board.display()));
            continue;
        };
        let label = board.display().to_string();

        let ix = PcbConnectivityIndex::build(&tree);
        if ix.skipped_pads() + ix.skipped_tracks() > 0 {
            failures.push(format!(
                "{label}: skipped {} pads / {} tracks from a pcbnew-authored board",
                ix.skipped_pads(),
                ix.skipped_tracks()
            ));
            continue;
        }
        assert_index_invariants(&tree, &label, false);
        n_pads += ix
            .nets()
            .iter()
            .map(|n| ix.pads_of_net(n).len())
            .sum::<usize>()
            + ix.pads_without_net().len();
        n_nets += ix.nets().len();

        let scan = footprint_courtyards(&tree);
        if scan.skipped > 0 {
            failures.push(format!(
                "{label}: {} footprints yielded no courtyard entry",
                scan.skipped
            ));
        }
        if scan.items.len() != footprints(&tree).len() {
            failures.push(format!("{label}: courtyard entries != footprints"));
        }
        for c in &scan.items {
            let (x0, y0, x1, y1) = c.bbox;
            if !(x0 <= x1 && y0 <= y1 && x0.is_finite() && y1.is_finite()) {
                failures.push(format!("{label}: malformed courtyard bbox {:?}", c.bbox));
            }
            match c.bbox_source {
                CourtyardSource::Courtyard => n_courtyards += 1,
                CourtyardSource::PadsFallback | CourtyardSource::AnchorOnly => n_fallbacks += 1,
            }
        }
    }
    eprintln!(
        "indexed {} demo boards: {n_pads} pads, {n_nets} nets, \
         {n_courtyards} courtyards + {n_fallbacks} fallbacks",
        boards.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Vacuity guards, far below the real corpus numbers.
    assert!(n_pads > 1000, "suspiciously few pads ({n_pads})");
    assert!(n_nets > 100, "suspiciously few nets ({n_nets})");
    assert!(
        n_courtyards > 100,
        "suspiciously few courtyards ({n_courtyards})"
    );
}

// ─── Footprint lock state (#350) ─────────────────────────────────────────────

/// `unlocked` contains `locked`, and KiCad writes far more of the former.
///
/// Every one of ecc83's 15 footprints carries `(unlocked yes)` — nested inside
/// its `(property ...)` items — and the board contains **zero** `(locked yes)`.
/// Two plausible implementations invert the attribute outright here: a text
/// scan for `locked yes`, and a recursive search that matches the tag loosely.
/// Either reports all 15 footprints as locked, so automated placement would
/// refuse to move a board on which nothing is locked at all.
///
/// What makes the real implementation correct is reading the tag as the head of
/// a **direct** child: that makes `unlocked` a different tag rather than a
/// longer spelling of this one, and keeps a property's attribute from being
/// mistaken for the footprint's.
///
/// (The author of this parse fell for exactly this with `grep -c "locked yes"`
/// while writing it, which is why the test exists.)
#[test]
fn unlocked_is_not_a_spelling_of_locked() {
    for (name, min_unlocked) in [
        ("ecc83-pp.kicad_pcb", 30usize),
        ("pic_programmer.kicad_pcb", 12),
        ("RoyalBlue54L-NFC-Antenna.kicad_pcb", 9),
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let content = std::fs::read_to_string(&path).expect("fixture readable");
        assert!(
            content.matches("(unlocked yes)").count() >= min_unlocked,
            "{name}: expected at least {min_unlocked} `(unlocked yes)`; the fixture              changed and this test's premise no longer holds"
        );
        assert_eq!(
            content.matches("(locked yes)").count(),
            0,
            "{name}: fixture now contains a real lock; this test assumes it has none"
        );

        let tree = parse_sexp(&content).expect("fixture parses");
        let scan = footprint_courtyards(&tree);
        let locked: Vec<&str> = scan
            .items
            .iter()
            .filter(|c| c.locked)
            .filter_map(|c| c.reference.as_deref())
            .collect();
        assert!(
            locked.is_empty(),
            "{name}: reported {} locked footprint(s) {:?} on a board whose only lock              attribute is `unlocked`",
            locked.len(),
            locked
        );
    }
}

/// The positive half, against real boards: pcbnew's own demos do contain
/// genuinely locked footprints, and we must see them.
///
/// Pinning an exact total would break on the next KiCad release, so this
/// asserts the shape instead — some board has locked footprints, no board
/// reports more locked than it has, and (the real point) the count is nowhere
/// near the total, which is what a depth-blind implementation would produce.
#[test]
fn installed_demo_boards_report_genuinely_locked_footprints() {
    let Some(root) = demo_dirs() else {
        eprintln!("SKIP: no KiCAD demos found (set KICAD_DEMOS to enable)");
        return;
    };

    let (mut total, mut locked) = (0usize, 0usize);
    let mut boards_with_locks = 0usize;
    for board in collect_boards(&root) {
        let content = std::fs::read_to_string(&board).unwrap_or_default();
        let Ok(tree) = parse_sexp(&content) else {
            continue;
        };
        let scan = footprint_courtyards(&tree);
        let n = scan.items.iter().filter(|c| c.locked).count();
        assert!(
            n <= scan.items.len(),
            "{}: {n} locked of {} footprints",
            board.display(),
            scan.items.len()
        );
        if n > 0 {
            boards_with_locks += 1;
        }
        total += scan.items.len();
        locked += n;
    }

    assert!(
        locked > 0 && boards_with_locks > 0,
        "no locked footprint found across {total} footprints in the installed demos — \
         either the lock parse is blind, or pcbnew stopped shipping locked parts"
    );
    assert!(
        locked * 4 < total,
        "{locked} of {total} demo footprints reported locked; that ratio means the parse \
         is matching pad- or graphic-level locks, not footprint-level ones"
    );
}
