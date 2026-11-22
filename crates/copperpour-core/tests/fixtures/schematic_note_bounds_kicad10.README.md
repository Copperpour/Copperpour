# Schematic note bounds fixture

`schematic_note_bounds_kicad10.kicad_sch` holds seventeen free text items and nothing
else, so the proposed note bounds in `get_schematic_layout` can be checked against KiCad's
own plot of each justification, line count and rotation, and against notes it must not
guess.

## Provenance

Drawn by tool calls, not by hand: `create_schematic` (A4, landscape) and one
`add_schematic_text` call per item, in the order below. Then resaved with
`kicad-cli sch upgrade --force` (KiCad 10.0.8), so it carries `(generator "eeschema")` and
`(version 20260306)`. The resave kept every item, UUID, position and justification, and
added `exclude_from_sim` to each text. It also rewrote two angles: `Upside note` was
written at 180 and is saved at 0, and `Down note\nsecond` was written at 270 and is saved
at 90. KiCad plots both the same before and after, so this is how KiCad itself stores
those notes. Copperpour's own output with the 180 and 270 angles is kept in
`schematic_note_rotation_unsaved.kicad_sch`. Plot this sheet with:

```text
kicad-cli sch export svg schematic_note_bounds_kicad10.kicad_sch
```

## Items

| Text | At | Rotation | Justify | Font | Expected |
|---|---|---|---|---|---|
| `Centered\nthree\nlines` | (40, 30) | 0 | centred | 1.27 | measured |
| `Right top A\nRight top B` | (90, 45) | 0 | right top | 1.27 | measured |
| `Left top` | (30, 60) | 0 | left top | 1.27 | measured |
| `Center bottom` | (60, 75) | 0 | bottom | 1.27 | measured |
| `Right bottom` | (90, 90) | 0 | right bottom | 1.27 | measured |
| `Turned left\nbottom` | (130, 70) | 90 | left bottom | 1.27 | measured |
| `Turned centre` | (145, 60) | 90 | centred | 1.27 | measured |
| `Turned right\ntop` | (160, 40) | 90 | right top | 1.27 | measured |
| `Upside note` | (220, 40) | 0 (written as 180) | left bottom | 1.27 | measured |
| `Down note\nsecond` | (240, 50) | 90 (written as 270) | left bottom | 1.27 | measured |
| `Gap above\n\nGap below` | (30, 110) | 0 | left bottom | 1.27 | measured |
| `Big bold` | (30, 135) | 0 | left bottom | 2.54, bold, thickness 0.508 | measured |
| `V_{OUT} and ~{RST}` | (120, 110) | 0 | left bottom | 1.27 | `unsupported_markup` |
| `${TITLE}` | (120, 120) | 0 | left bottom | 1.27 | `unresolved_text_variable` |
| `Italic note` | (120, 130) | 0 | left bottom | 1.27 italic | `unsupported_text_style` |
| `Arrow → here` | (120, 140) | 0 | left bottom | 1.27 | `unsupported_glyph` |
| three spaces | (120, 150) | 0 | left bottom | 1.27 | `no_ink` |

## Expected bounds

Each `<g class="stroked-text">` group in KiCad's SVG is one drawn line. Its bounds are the
path vertices widened by half the plotted stroke width (round caps). A multiline note is
the union of its lines. The SVG writes four decimals; a tolerance of 0.005 mm covers the
rounding.

KiCad 10.0.8's plot of the measured notes (mm, x_min / y_min / x_max / y_max):

| Note | KiCad 10.0.8 SVG |
| --- | --- |
| `Centered\nthree\nlines`, centred | 35.9929 / 26.9387 / 43.9467 / 32.4503 |
| `Right top A\nRight top B` | 78.9985 / 44.7262 / 89.8550 / 48.6166 |
| `Turned left\nbottom`, 90° | 126.4457 / 59.2323 / 129.9127 / 69.6050 |
| `Turned right\ntop`, 90° | 159.9762 / 39.8949 / 163.8666 / 51.4771 |
| `Gap above\n\nGap below` | 30.2659 / 104.1511 / 39.9732 / 110.0861 |
| `Big bold`, 2.54 mm | 30.6849 / 131.6186 / 46.1910 / 135.5133 |
| `Upside note`, saved by KiCad at 0° | 220.3263 / 38.2403 / 231.3643 / 40.0861 |
| `Down note\nsecond`, saved by KiCad at 90° | 236.4457 / 39.8371 / 239.9127 / 49.4840 |

`${TITLE}` resolves to the empty title here and draws nothing. A reader that cannot
resolve text variables should say so rather than guess. The three-space note draws nothing
either.
