# Text overlap fixture

`text_overlap_kicad10.kicad_sch` puts label and field text into other text and
across a symbol's body, beside controls that must stay clean, so
`check_schematic_overlaps`' `text_overlap` entries (#111) can be checked.
`text_overlap_kicad10.svg` is KiCad's plot of it, the oracle for where each
string is drawn.

## Provenance

Built through Copperpour with `create_schematic`, `add_schematic_net_label`,
`add_power_symbol` and `add_schematic_component`, against KiCad 10.0.6's stock
`Device`, `power` and `Amplifier_Operational` libraries, then parsed and
force-resaved by KiCad 10.0.6:

```text
kicad-cli sch upgrade --force text_overlap_kicad10.kicad_sch
kicad-cli sch export svg --exclude-drawing-sheet -o . text_overlap_kicad10.kicad_sch
```

What is committed is that Eeschema serialization, `(generator "eeschema")` and
`(version 20260306)`, and that plot, its `<title>`'s export timestamp removed.
Every string is 1.27 mm.

## Cases

KiCad's own figures below are read from the SVG: the ink of each string's
drawn strokes (`<g class="stroked-text">`, widened by half the 0.1524 mm
stroke), in mm.

| Items | Placement | KiCad's ink | Expected |
|---|---|---|---|
| `NRST_CH347` / `XO` | net labels on one row, `XO` at `(60.96, 50.8)` | x 51.13–62.77 and 61.17–63.43, one row | `text_overlap`, the issue's case |
| `NRST_CH348` / `XI` | the same pair, `XI` at `(64.77, 63.5)` | x 51.13–62.71 and 64.98–66.46, 2.27 mm apart | clean |
| `#PWR01` Value / `VBUS_SENSE` | `+3V3` at `(88.9, 50.8)`, the label at `(85.09, 48.26)` | x 86.59–91.27 and 85.24–96.94, y 46.48–47.90 and 46.40–47.94 | `text_overlap` |
| `LONG_NET_NAME_ACROSS` / `R3` | the label at `(170.18, 63.5)`, through `R3` at `(177.8, 63.5)` | across `R3`'s body and both its fields | three `text_overlap`s: the body, the Value, the Reference |
| `SCL_PULLUP` / `SDA` | the label at `(35.56, 88.9)` into the input global label at `(50.8, 88.9)`, turned 180 | x 35.83–47.23 and 45.87–49.23, y 87.04–88.58 and 88.22–89.65 | `text_overlap` with the global label |
| `CLK_SRC` / `CLK_OUT` | the net label at `(60.96, 114.3)`, turned 180, over the output hierarchical label at `(50.8, 114.3)` | x 52.74–60.69 and 52.53–60.48, y 112.44–113.98 and 113.53–115.08 | `text_overlap` |
| `R4` | stock `Device:R` at `(101.6, 88.9)`, fields where the library puts them | its Value inside its own body | clean |
| `R10`–`R15` | `Device:R` at 0, 90, 180 and 270, at 0 mirrored `x`, and at 270 mirrored `x` | each field upright beside or inside its own body | clean |
| `G_*`, `H_*`, `N_UP`, `N_DOWN`, `~{RESET}` | global labels `input` at 0, `output` at 90, `bidirectional` at 180, `tri_state` at 270 and `passive` at 0; hierarchical `input` at 90, `output` at 180, `bidirectional` at 270 and `passive` at 0; net labels standing up and down; an overbar | apart | clean |
| `U1` | `LM358` units 1 and 2 | References drawn as `U1A` and `U1B` | clean |

The test holds every label and visible field to the plot: the same string,
across it the ink to 0.005 mm, along it both ends to 0.002 mm. The plot's
other strings are `U1`'s pin names and numbers. All the text is 1.27 mm, in
the default stroke font and weight; no field is top- or bottom-justified.
