# Text overlap fixture

`text_overlap_kicad10.kicad_sch` puts label and field text into other text and
across a symbol's body, beside controls that must stay clean, so
`check_schematic_overlaps`' `text_overlap` entries (#111) can be checked.
`text_overlap_kicad10.svg` is KiCad's plot of it, the oracle for where each
string is drawn.

## Provenance

Built through Copperpour with `create_schematic`, `add_schematic_net_label`,
`add_power_symbol`, `add_schematic_component`, `delete_schematic_component` and
`rename_schematic_net_label`, against KiCad 10.0.6's stock `Device`, `power`
and `Amplifier_Operational` libraries, then parsed and force-resaved by KiCad
10.0.6 with no project file beside it, so each symbol's instance reads
`(project "")`; resaved beside a `.kicad_pro`, those lines name the project:

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
| `NRST_CH348` / `X{slash}I` | the same pair, `X{slash}I` at `(64.77, 63.5)`, drawn `X/I` | x 51.13–62.71 and 64.98–67.79, 2.27 mm apart | clean |
| `#PWR01` Value / `VBUS_SENSE` | `+3V3` at `(88.9, 50.8)`, the label at `(85.09, 48.26)` | x 86.59–91.27 and 85.24–96.94, y 46.48–47.90 and 46.40–47.94 | `text_overlap` |
| `LONG_NET_NAME_ACROSS` / `R3` | the label at `(170.18, 63.5)`, through `R3` at `(177.8, 63.5)` | across `R3`'s body and both its fields | three `text_overlap`s: the body, the Value, the Reference |
| `SCL_PULLUP` / `SDA` | the label at `(35.56, 88.9)` into the input global label at `(50.8, 88.9)`, turned 180 | x 35.83–47.23 and 45.87–49.23, y 87.04–88.58 and 88.22–89.65 | `text_overlap` with the global label |
| `CLK_SRC` / `CLK_OUT` | the net label at `(60.96, 114.3)`, turned 180, over the output hierarchical label at `(50.8, 114.3)` | x 52.74–60.69 and 52.53–60.48, y 112.44–113.98 and 113.53–115.08 | `text_overlap` |
| `R4` | stock `Device:R` at `(101.6, 88.9)`, fields where the library puts them | its Value inside its own body | clean |
| `R10`–`R15` | `Device:R` at 0, 90, 180 and 270, at 0 mirrored `x`, and at 270 mirrored `x` | each field upright beside or inside its own body | clean |
| `G_*`, `H_*`, `N_UP`, `N_DOWN`, `~{RESET}` | global labels `input` at 0, `output` at 90, `bidirectional` at 180, `tri_state` at 270 and `passive` at 0; hierarchical `input` at 90, `output` at 180, `bidirectional` at 270 and `passive` at 0; net labels standing up and down; an overbar | apart | clean |
| `G_STACK_A` / `G_STACK_B` | `input` global labels at `(127, 88.9)` and `(127, 91.44)`, on the 2.54 mm grid | outlines drawn at y 87.6296–90.1704 and 90.1696–92.7104 | clean |
| `G_CORNER` / `R5` | a `passive` global label at `(164.846, 101.6)`, `Device:R` at `(163.83, 97.79)` | the outline's corner drawn at `(164.846, 100.3296)`, the body's at `(164.846, 100.33)` | clean |
| `ONE_A` / `ONE_B` | net labels at one anchor, `(127, 114.3)` | one over the other | one `label_overlap`, no `text_overlap` |
| `NEAR_A` / `NEAR_B` | net labels of two nets at `(25.4, 152.4)` and `(25.7, 152.4)`, within the 0.5 mm tolerance | x 25.73–32.35 and 26.03–32.71, one row | one `label_overlap`, no `text_overlap` |
| `SAME` / `SAME` | a net label and an `input` global label of one net at one anchor, `(76.2, 152.4)` | x 76.47–81.03 and 77.89–82.46, y 150.54–151.96 and 151.72–153.15 | `text_overlap` |
| `BBBB1` / `BBBB2` | net labels at `(139.7, 127)` and `(139.7, 128.3)`, 1.3 mm apart | y 125.14–126.56 and 126.44–127.86, the strokes crossing by 0.12 mm | `text_overlap` |
| `END_A` / `END_B` | net labels end to end at `(50.8, 152.4)` and `(56.43, 152.4)` | x 51.13–56.66 and 56.76–62.35, 0.095 mm apart | clean |
| `␣LEAD`, `TRAIL␣` | net labels with a leading and a trailing space at `(25.4, 182.88)` and `(50.8, 182.88)` | x 26.69–30.84 and 50.94–55.81, the space drawing nothing | clean |
| `A~{BC}D`, `~{X_{1}}`, `a^{2}g`, `4kΩ` | net labels with a partial overbar, an overbar over a subscript, a superscript inside the text and the ohm sign, on the same row | x 76.34–81.03 for `A~{BC}D`, 101.87–104.74 for `a^{2}g`, 127.27–130.62 for `4kΩ` | clean |
| `±1% 25°C` | a net label with the plus-minus and degree signs, at `(177.8, 182.88)` | x 178.13–187.59 | clean |
| `V_{OUT}`, `X^{2}` | an `output` and an `input` global label ending in a subscript and a superscript, at `(25.4, 190.5)` and `(63.5, 190.5)` | text x 26.02–29.95 and 65.13–67.09 | clean |
| `R6`, `TH1`, `AE1`, `AE2` | `Device:R_Photo` with its top-justified Value as `~{EN}`, `Device:Thermistor` with its bottom-justified Value as `X^{2}`, and `Device:Antenna` with its right-justified Value as `V_{OUT}` turned 90 and as `10µF` mirrored `y` | Values x 103.03–105.41, x 122.81–124.35, y 200.18–204.11, x 179.98–184.55 | clean |
| `U1` | `LM358` unit 1 mirrored `y`, unit 2 turned 90 and mirrored `x` | References drawn as `U1A` and `U1B`, fields left-justified | clean |

An outline's figures are its drawn line. `SCH_GLOBALLABEL::CreateGraphicShape`
draws it 0.0004 mm further out on each side than the check measures it: the 3
internal units it adds, and a fourth from rounding its margin and pen to whole
units. So KiCad's stacked outlines cross by 0.0008 mm, and at the corner its
line is 0.0004 mm into the body's in y and flush with it in x; the check's
lines meet.

The test holds every label and visible field to the plot: the same string, its
ink along it and across it to 0.005 mm. The plot's
other strings are `U1`'s pin names and numbers. All the text is 1.27 mm, in
the default stroke font and weight; `␣` above stands for a space.
