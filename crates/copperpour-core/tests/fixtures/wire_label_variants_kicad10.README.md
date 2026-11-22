# Wire crossings over transformed label text

This synthetic sheet was created with `create_schematic`, `add_schematic_net_label`,
`batch_place_components`, `add_component_annotation`, `edit_schematic_component`,
`batch_add_wire`, `add_wire` and `batch_delete`. It was saved with
`kicad-cli sch upgrade --force`, and plotted with KiCad 10.0.8:

```sh
kicad-cli sch export svg --exclude-drawing-sheet -o . wire_label_variants_kicad10.kicad_sch
```

| Wire centres (mm) | What the plot shows | `wire_label_overlap` |
| --- | --- | --- |
| (48.26, 45.72) to (52.07, 45.72) | Crosses the 90° local label `TURN_UP` | Yes |
| x = 97.79, y = 48.26 to 53.34 | Crosses the 180° global label `FACE_LEFT` | Yes |
| y = 96.52, x = 200.66 to 205.74 | Overlaps the drawn strokes of the 90° global label `VERT_GLOBAL` | Yes |
| y = 54.61, x = 149.86 to 154.94 | Crosses the 270° hierarchical label `TURN_DOWN` | Yes |
| y = 52.07, x = 149.86 to 154.94 | Meets the same label's flag, clear of its text | No |
| (203.2, 50.8) to (215.9, 50.8) | Starts at the `THROUGH` anchor and enters its text | Yes |
| y = 96.52, x = 99.06 to 110.49 | Crosses the mirrored power symbol's visible `+3V3` Value | Yes |
| y = 96.52, x = 148.59 to 156.21 | Runs through the other power symbol's hidden Value position | No |
| y = 106.68, x = 95.25 to 107.95 | Crosses the mirrored power symbol's `RAIL NOTE` field | No |
| y = 97.79 and 101.6, x = 54.61 to 60.96 | Crosses `R1`'s Reference and Value fields | No |
| y = 139.7, x = 50.8 to 63.5 | Runs under `VCC(3V3)` on the baseline; the closest glyph path is 0.029 mm away | No |
| x = 101.6, y = 139.7 to 152.4 | Runs along the baseline of the flipped `V_{gs}`; the closest glyph path is 0.034 mm away | No |
| x = 57.15, y = 139.7 to 144.78 | Joins the `VCC(3V3)` baseline from below | No |
| y = 140.97, x = 101.6 to 106.68 | Joins the `V_{gs}` baseline from the right | No |
| x = 50.8, y = 139.7 to 144.78 | Ends at the `VCC(3V3)` anchor | No |
| x = 55.88, y = 135.89 to 138.43 | Passes into `VCC(3V3)` from above | Yes |

A label flip changes its spin and justification, as in `FACE_LEFT` and `TURN_DOWN`.
KiCad writes a separate mirror axis for the power symbol, not for the labels.

The tests compare the reported boxes with these glyph path extents from the SVG (mm):

| Text | X range | Y range |
| --- | --- | --- |
| `TURN_UP` | 49.02–50.41 | 42.48–50.58 |
| `FACE_LEFT` | 90.58–99.95 | 50.20–51.59 |
| `VERT_GLOBAL` | 202.60–203.99 | 87.73–99.95 |
| `TURN_DOWN` | 151.71–153.10 | 52.66–63.49 |
| `THROUGH` | 204.85–213.26 | 50.20–51.47 |
| `+3V3` | 103.17–107.71 | 95.83–97.10 |
| `VCC(3V3)` | 51.02–59.43 | 137.74–139.67 |

KiCad 10.0.8 exports no nets. ERC reports 11 dangling wires and 7 dangling labels,
among other findings on this intentionally unwired sheet. It does not check text.
