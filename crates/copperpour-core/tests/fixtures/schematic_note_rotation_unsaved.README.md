# Note rotation fixture: Copperpour output before any KiCad save

`schematic_note_rotation_unsaved.kicad_sch` is what Copperpour writes today for a
free-text note at 180 or 270 degrees. It is the one sheet among the schematic readability
fixtures that KiCad has never saved, on purpose, and it must stay that way: a KiCad save
rewrites both angles (see below).

## Provenance

Written by tool calls to Copperpour's stdio server, with no other writer and no edit by
hand: `create_schematic` (A4) and two `add_schematic_text` calls with the same text,
position, font and justification as two notes in
`schematic_note_bounds_kicad10.kicad_sch`:

| Text | At | Rotation | Justify | Font | Expected |
|---|---|---|---|---|---|
| `Upside note` | (220, 40) | 180 | left bottom | 1.27 | `unmeasured`, `unsupported_rotation` |
| `Down note\nsecond` | (240, 50) | 270 | left bottom | 1.27 | `unmeasured`, `unsupported_rotation` |

The header is the writer's own: `(generator "copperpour")`, `(version 20250610)`.

## What KiCad does with it

`kicad-cli sch export svg` (KiCad 10.0.8) plots these notes with exactly the ink of the 0
and 90 degree notes that KiCad saves in their place in
`schematic_note_bounds_kicad10.kicad_sch`: `Upside note` x 220.3263 to 231.3643, y 38.2403
to 40.0861; `Down note\nsecond` x 236.4457 to 239.9127, y 39.8371 to 49.4840 (stroke
extents plus half the pen width, mm). `kicad-cli sch upgrade --force` saves them at 0 and
90 with the same justification. So the 180 and 270 cases are measurable by normalising the
angle the way KiCad does on save. Until a reader does that, the expected answer for both
notes is `unmeasured` with `unsupported_rotation`.
