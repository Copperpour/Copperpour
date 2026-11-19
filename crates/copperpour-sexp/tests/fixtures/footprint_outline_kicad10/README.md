# footprint_outline_kicad10

Sixteen boards whose only Edge.Cuts graphics belong to footprints (issue #27),
saved by KiCad 10.0.8's pcbnew. `generate.py` builds each footprint at the
origin, and pcbnew places, turns and flips it, saves the board with
`SaveBoard`, and reads it back with `LoadBoard`; the oracle below is that
second read. Rerun it with `python3 generate.py OUT_DIR` (KiCad 10 `pcbnew`
and `kicad-cli` on PATH). New UUIDs come out of each run, and KiCad orders the
footprints by them, so the files change and the values below do not.

## Boards

- `mech1`: MECH1, built to the issue's figures, at (100, 100), 0°, front. No
  pads; 27 Edge.Cuts primitives: an outer contour of 11 segments and 8 arcs
  (3 mm corner arcs, a 20 x 8 mm notch with 1 mm arcs in the bottom edge, a
  V-notch with sharp corners in the top edge), and a 24.25 x 7.333 mm cut-out
  of 4 segments and 4 arcs. The cut-out's last segment stops 0.005 mm short of
  the arc it meets, inside KiCad's 0.01 mm chaining tolerance. A courtyard
  rectangle on the outline makes MECH1 a part to the overlap check. Beside it:
  SLOT1 at (140, 70), a 6 x 2 mm Edge.Cuts rectangle with a pad outside each
  end; R1 at (60, 80), R2 at (120, 90), a track between them on N1, and J2 at
  (165, 100).
- `mech1_rotated`: MECH1 alone, turned 30°.
- `mech1_back`: MECH1 alone with two 3.2 mm mounting holes (`np_thru_hole`),
  flipped left-right, then turned 150°.
- `open_outline`: MECH1 without its right edge segment, 26 primitives.
- `gap_outline`: MECH1 with the cut-out's gap 0.02 mm, past the tolerance.
- `short_segment`: MECH1 with a 0.004 mm segment cut off its top edge, 28
  primitives.
- `slot_only`: SLOT1 alone at (100, 100).
- `slot_back`: SLOTB alone at (100, 100), flipped left-right, then turned 30°:
  a 6 x 2 mm Edge.Cuts rectangle at x 1..7 beside a pad at x -4, so a mirror
  or a half turn of the pad alone would put it in the slot.
- `two_slots_only`: SLOTS2 alone at (100, 100): a 2 mm Edge.Cuts circle and a
  2 x 2 mm Edge.Cuts polygon, no pads.
- `slot_in_pad`: SLOTPAD alone at (100, 100): a 1.5 x 1 mm Edge.Cuts
  rectangle inside its own 6 x 4 mm pad, clear of the pad's centre and corners.
- `slot_across_pad`: SLOTX alone at (100, 100): a 10 x 0.5 mm Edge.Cuts
  rectangle across its own 6 x 4 mm pad; neither holds a corner of the other.
- `split_outline`: at (100, 100), HALF_A draws the top and right of a
  40 x 30 mm outline and HALF_B its bottom and left, no pads; SLOTN, a padless
  10 x 4 mm Edge.Cuts rectangle with a courtyard, lies inside.
- `curve_outline`: CURVE1 alone at (100, 100): a 20 mm square, no pads, whose
  left side is a Bézier curve.
- `rounded_rect`: RRECT1 alone at (100, 100), turned 90°: one 40 x 30 mm
  Edge.Cuts rectangle with 3 mm corners, no pads.
- `card_edge`: J1, a 30 x 40 mm outline of four segments with five pads
  inside it, at (100, 100), flipped left-right and turned 90°; R1 at
  (100, 100) on the front; and SLOT1 at (140, 100), off the card.
- `card_edge_hole`: `card_edge` with SLOT1 at (100, 92), inside the card.

Neither MECH1 contour is symmetric, so a wrong rotation sense or a mirror
applied twice moves the cut-out away from where KiCad puts it.

## Oracle

- **Contours:** each Edge.Cuts shape's `GetBoundingBox` less half its 0.05 mm
  stroke (`EDA_SHAPE::getBoundingBox` adds it), merged over the shapes the
  script drew into that contour, in board millimetres.
- **Outline:** `BOARD::GetBoardPolygonOutlines`, which chains each
  footprint's Edge.Cuts alone first, cuts those that close, with a pad
  outside them or more than one outline, as holes, and keeps a hole only
  where it lies in an outline. Its polygon bbox approximates arcs, so the
  contour bboxes are the exact values.
- **DRC:** `kicad-cli pcb drc --severity-all`, outline violation types only.

| board | contour | primitives | x_min | y_min | x_max | y_max |
|---|---|---|---|---|---|---|
| mech1 | MECH1 outer | 19 | 30 | 50 | 170 | 150 |
| mech1 | MECH1 cut-out | 8 | 67.875 | 111.3335 | 92.125 | 118.6665 |
| mech1 | SLOT1 | 1 | 137 | 69 | 143 | 71 |
| mech1_rotated | MECH1 outer | 19 | 15.476298 | 22.796806 | 184.523702 | 177.203194 |
| mech1_rotated | MECH1 cut-out | 8 | 78.21171 | 114.118624 | 102.147274 | 131.862137 |
| mech1_back | MECH1 outer | 19 | 15.476298 | 22.796806 | 184.523701 | 177.203193 |
| mech1_back | MECH1 cut-out | 8 | 97.852725 | 114.118624 | 121.788291 | 131.862136 |
| open_outline | MECH1 outer | 18 | 30 | 50 | 170 | 150 |
| open_outline | MECH1 cut-out | 8 | 67.875 | 111.3335 | 92.125 | 118.6665 |
| gap_outline | MECH1 outer | 19 | 30 | 50 | 170 | 150 |
| gap_outline | MECH1 cut-out | 8 | 67.875 | 111.3335 | 92.125 | 118.6665 |
| short_segment | MECH1 outer | 20 | 30 | 50 | 170 | 150 |
| short_segment | MECH1 cut-out | 8 | 67.875 | 111.3335 | 92.125 | 118.6665 |
| slot_only | SLOT1 | 1 | 97 | 99 | 103 | 101 |
| slot_back | SLOTB | 1 | 100.366025 | 95.633975 | 106.562178 | 100.366025 |
| two_slots_only | SLOTS2, both | 2 | 95 | 99 | 105 | 101 |
| slot_in_pad | SLOTPAD | 1 | 101 | 99.5 | 102.5 | 100.5 |
| slot_across_pad | SLOTX | 1 | 95 | 100.5 | 105 | 101 |
| split_outline | HALF_A and HALF_B | 4 | 80 | 85 | 120 | 115 |
| split_outline | SLOTN | 1 | 95 | 98 | 105 | 102 |
| curve_outline | CURVE1 | 4 | 86.251671 | 90 | 110 | 110 |
| rounded_rect | RRECT1 | 1 | 85 | 80 | 115 | 120 |
| card_edge | J1 | 4 | 80 | 85 | 120 | 115 |
| card_edge | SLOT1 | 1 | 137 | 99 | 143 | 101 |
| card_edge_hole | J1 | 4 | 80 | 85 | 120 | 115 |
| card_edge_hole | SLOT1 | 1 | 97 | 91 | 103 | 93 |

| board | outlines KiCad builds | holes | DRC outline errors |
|---|---|---|---|
| mech1 | 1 | 2 | none |
| mech1_rotated | 1 | 1 | none |
| mech1_back | 1 | 1 | none |
| open_outline | none (`GetBoardPolygonOutlines` fails) | - | `invalid_outline` |
| gap_outline | none (`GetBoardPolygonOutlines` fails) | - | `invalid_outline` |
| short_segment | 1 | 1 | none |
| slot_only | 0 | - | none |
| slot_back | 0 | - | none |
| two_slots_only | 0 | - | none |
| slot_in_pad | 1 | 0 | none |
| slot_across_pad | 1 | 0 | none |
| split_outline | 1 | 1 | none |
| curve_outline | 1 | 0 | none |
| rounded_rect | 1 | 0 | none |
| card_edge | 1 | 0 | none |
| card_edge_hole | 1 | 1 | none |

## sha256

```text
80361f091e14371f6b2037bdadc20e9982ab679c2784ea99d2d7dc4d60440baa  mech1.kicad_pcb
a8de75848303edd30fd942706d7db1bafb3a8b8fa00de37f80030468bb8d5a32  mech1_rotated.kicad_pcb
0f6f0456e85dbc23dc30b2e7cfdc56cfd6be0f4b01bd717bd8502f7a63b11658  mech1_back.kicad_pcb
e0754ff6a423a70f6ea85c7d1ad406e3f5b198ed411cc06f883e5901ba732bc2  open_outline.kicad_pcb
c428fd9f98552e154c6e878de2cef44d10bb5335d223cadf00b54da0a31e95d2  gap_outline.kicad_pcb
a43112c5f2e7c4c70559c64b0cb6483b8cfbfeb4de5d47c967bfc9e2d73296dc  short_segment.kicad_pcb
4333b3036dde6c4b860625652c8b902599a958e3a58c99878fdd4827b780ff67  slot_only.kicad_pcb
ea107668975af2b729b22458785df9b2eed61f9d1dbede822b8743bcaa887654  slot_back.kicad_pcb
d5bed7b33bc64288f2ea54386d92bc6e12830e30a8218f13a887f98d62afef8a  two_slots_only.kicad_pcb
caee65f5d5e14ff3f09f4c21c7bfe238d60f531babc07809edf396384a8557d2  slot_in_pad.kicad_pcb
f94724689293c16202c984f3731ec7bde925c574fc5eb9fcf10f8cc20a401695  slot_across_pad.kicad_pcb
bf615e0961985651a534afc4e0f0e15694df6a612f603756e6a870a1e7a0a83a  split_outline.kicad_pcb
4e7ce13e7ff258586c7d81aa97dafcb11f1f6a5a073d4989406b78196760d0c3  curve_outline.kicad_pcb
d889c7b7b493b4f4a78127e95295151a9251e984fcf3db19bbf60bf3340255db  rounded_rect.kicad_pcb
50978f17354cf588f1d0d4aafbb87fa7466fd6b1113deced2228bc4489038b3d  card_edge.kicad_pcb
18ce52db81215e9a0955d42e149a719c3f0ab2c1f936bd74f365db7202aacaef  card_edge_hole.kicad_pcb
```

## Tests using these boards

`crates/copperpour-sexp/tests/board_fixtures.rs`: `footprint_outline` on each
board against the tables above.

`crates/copperpour-core/src/tools/placement.rs`: `score_placement` on
`mech1`, `mech1_back`, `open_outline`, `slot_only`, `split_outline` and
`card_edge`.
