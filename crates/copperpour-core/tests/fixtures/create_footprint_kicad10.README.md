# create_footprint round-trip footprints

Four footprints `create_footprint` wrote, each saved again by KiCad, used to
check that `create_footprint` writes the KiCad 10 format KiCad itself writes
(#711): `kicad-cli fp upgrade --force` of its file gives the file back, UUIDs
and the generator aside.

## Provenance

Each file is `kicad-cli fp upgrade --force` (KiCad 10.0.8 on Linux, format
20260206) of what `create_footprint` wrote, through `tools/call`, for the
arguments below (`output` aside). Nothing was edited by hand. KiCad gives every
item a new UUID on each save and writes `(generator "pcbnew")` where
`create_footprint` writes `(generator "copperpour")`; the tests mask both and
compare everything else byte for byte. KiCad on Linux writes LF line endings.

| Fixture | Arguments | SHA-256 (first 16) |
|---|---|---|
| `create_footprint_smd_body_kicad10.kicad_mod` | `SMD2`: a rect and a roundrect SMD pad given out of order, a body wider than the pads (`body_width` 4, `body_height` 2), a description with quotes, a model with an offset and a rotation | `cdddf82ab88da77e` |
| `create_footprint_tht_kicad10.kicad_mod` | `THT3`: pads `10` (castellated), `2` (oval drill, rotation 90), an unnumbered NPTH and `3` (rotation -90), no body | `be74b546a4af71d3` |
| `create_footprint_large_body_kicad10.kicad_mod` | `BOURNS`: two 2.5 x 3 mm SMD pads at x ±5.715 and a custom pad, body 12.62 x 8.48 mm (the size of a Bourns 70AAJ-5-M0) | `245062f4ec850eaa` |
| `create_footprint_back_kicad10.kicad_mod` | `Back`: two back copper SMD pads and `silkscreen: "none"` | `c748ba100c3fe834` |

The arguments, as JSON:

```json
{"name": "SMD2", "description": "two pads \"quoted\"", "body_width": 4, "body_height": 2,
 "pads": [{"number": "2", "type": "smd", "shape": "roundrect", "x": 1.5, "y": 0, "width": 0.8, "height": 1.2, "roundrect_rratio": 0.25},
          {"number": "1", "type": "smd", "shape": "rect", "x": -1.5, "y": 0, "width": 0.8, "height": 1.2}],
 "model": {"path": "${KICAD10_3DMODEL_DIR}/x.step", "rotate": {"z": 90}, "offset": {"x": 0.1, "y": -0.2}}}

{"name": "THT3",
 "pads": [{"number": "10", "type": "thru_hole", "shape": "rect", "x": 0, "y": 0, "width": 1.7, "height": 1.7, "drill": 1.0, "fabrication_property": "castellated"},
          {"number": "2", "type": "thru_hole", "shape": "oval", "x": 2.54, "y": 0, "width": 1.7, "height": 2.2, "drill": {"shape": "oval", "width": 0.8, "height": 1.2}, "rotation": 90},
          {"number": "", "type": "np_thru_hole", "shape": "circle", "x": 5, "y": 0, "width": 2, "height": 2, "drill": 2},
          {"number": "3", "type": "thru_hole", "shape": "circle", "x": 2.54, "y": 2.54, "width": 1.7, "height": 1.7, "drill": 1.0, "rotation": -90}]}

{"name": "BOURNS", "body_width": 12.62, "body_height": 8.48,
 "pads": [{"number": "1", "type": "smd", "shape": "rect", "x": -5.715, "y": 0, "width": 2.5, "height": 3},
          {"number": "2", "type": "smd", "shape": "rect", "x": 5.715, "y": 0, "width": 2.5, "height": 3},
          {"number": "3", "type": "smd", "shape": "custom", "x": 0, "y": 3, "width": 0.5, "height": 0.5,
           "custom_shape": {"anchor": "circle", "primitives": [{"type": "poly", "points": [{"x": -1, "y": -0.5}, {"x": 1, "y": -0.5}, {"x": 1, "y": 0.5}, {"x": -1, "y": 0.5}], "stroke_width_mm": 0.0, "fill": "solid"}]}}]}

{"name": "Back", "silkscreen": "none",
 "pads": [{"number": "1", "type": "smd", "shape": "rect", "x": -2, "y": 0, "width": 1, "height": 1, "layers": ["B.Cu", "B.Mask"]},
          {"number": "2", "type": "smd", "shape": "rect", "x": 2, "y": 0, "width": 1, "height": 1, "layers": ["B.Cu", "B.Mask"]}]}
```

## What they show

- KiCad's footprint writer (`PCB_IO_KICAD_SEXPR::format( const FOOTPRINT* )`,
  `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp`, 10.0.8) keeps the file as
  written: the order of its children, Reference and Value as properties beside
  hidden empty Datasheet and Description properties,
  `(duplicate_pad_numbers_are_jumpers no)`,
  graphics in `FOOTPRINT::cmp_drawings` order and pads in `cmp_pads` order
  (`pcbnew/footprint.cpp`), `(remove_unused_layers no)` on a plated hole,
  `(embedded_fonts no)` before the model, and the layout of its `Prettify` pass
  (`common/io/kicad/kicad_io_utils.cpp`).
- A pad rotation of -90 is written as 270, as KiCad keeps a pad's orientation
  in [0, 360).
- With a body, the courtyard goes around the pads and the body, and the silk
  runs 0.11 mm outside the body, broken where it would come within 0.2 mm of a
  pad (`BOURNS`: silk at x ±6.42, broken at both side pads).

- KiCad preserves the front and back courtyard rectangles for `THT3`. It
  preserves only the back courtyard for `Back`, where Reference is the only
  item on `F.SilkS` and the fab chamfer is present.
- KiCad preserves the interior pin-1 circle in the other three cases.

## Regenerating

Call `create_footprint` with each argument set above into a `.pretty`
directory with the output filename matching the footprint name. Then run
`kicad-cli fp upgrade --force -o <out>.pretty <that>.pretty`.
