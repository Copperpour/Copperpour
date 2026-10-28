//! The renderer against a real kicad-cli 10.0.5 schematic export — the
//! fixture is verbatim `kicad-cli sch export svg` output (stroke-font paths
//! plus opacity-0 searchability text), which is exactly the input the
//! visual-feedback tools feed it.

use copperpour_render::{diff_pngs, svg_to_png};

const KICAD_SVG: &[u8] = include_bytes!("fixtures/placement_fixture.svg");

#[test]
fn a_real_kicad_cli_export_renders() {
    let out = svg_to_png(KICAD_SVG, 1600).expect("real exports must render");
    assert_eq!(out.width_px, 1600);
    assert!(out.height_px > 0);
    // A schematic render is not a blank page: some pixels differ from the
    // white ground.
    let img = image::load_from_memory(&out.png).unwrap().to_rgba8();
    let non_white = img.pixels().filter(|p| p.0 != [255, 255, 255, 255]).count();
    assert!(non_white > 1000, "only {non_white} non-white pixels");
}

#[test]
fn rendering_twice_is_byte_identical() {
    // Same-machine smoke test; the cross-OS CI hash comparison is the real
    // determinism gate.
    let a = svg_to_png(KICAD_SVG, 800).unwrap();
    let b = svg_to_png(KICAD_SVG, 800).unwrap();
    assert_eq!(a.png, b.png);
    let diff = diff_pngs(&a.png, &b.png).unwrap();
    assert_eq!(diff.changed_pixels, 0);
}
