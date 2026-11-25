# edit_symbol_pins libraries

A small symbol library saved by KiCad, and KiCad's own saves of the same
library with several pins of one symbol changed, used to check that
`edit_symbol_pins` (#868) leaves the library KiCad saves.

## Provenance

The symbols were drawn for these tests, not copied from KiCad's libraries, so
nothing CC BY-SA is committed. Each library was written as a KiCad 9 draft
(format 20241209) and saved by `kicad-cli sym upgrade --force` (KiCad 10.0.8 on
macOS, format 20251024); a second resave leaves each byte for byte as it is.
KiCad wrote every byte of each fixture.

| Fixture | Content | SHA-256 (first 16) |
|---|---|---|
| `edit_symbol_pins_source_kicad10.kicad_sym` | `BOX`: a body rectangle from (-7.62, 3.81) to (7.62, -3.81) and six pins of length 2.54, one set at each of KiCad's four angles: inputs `1` `IN1` and `2` `IN2` at x -10.16 (angle 0), outputs `3` `OUT1` and `4` `OUT2` at x 10.16 (angle 180), `power_in` `5` `VCC` at y 6.35 (angle 270) and `6` `GND` at y -6.35 (angle 90); `OTHER`, two passive pins | `9a2f1c46b159ec7f` |
| `edit_symbol_pins_kept_kicad10.kicad_sym` | the source with every pin of `BOX` 3.81 long and its end at the body where it was: x -11.43 and 11.43, y 7.62 and -7.62 | `a1916046ebff62d5` |
| `edit_symbol_pins_swapped_kicad10.kicad_sym` | the source with `BOX`'s pins `1` and `2` swapping numbers, `3` named `Q`, `4` of type `tri_state` and `6` hidden | `11ce98c22270a3de` |

## What they show

- KiCad draws each pin from its connection point to the end at the body: the
  connection point plus the length along the pin's direction
  (`SCH_PIN::GetPinRoot`, `eeschema/sch_pin.cpp`, 10.0.8), with the file's y
  up (`parseXY( true )` in `parseSymbolPin`). In `kicad-cli sym export svg
  --symbol BOX` of the kept library, every pin line starts on the rectangle's
  edge and is 3.81 mm long.
- KiCad saves a unit's pins sorted by number, then position (`SCH_PIN::compare`,
  `StrNumCmp`): in the swapped library, the pin now numbered `1` comes first.

## Regenerating

Each draft is one line: `(kicad_symbol_lib (version 20241209) (generator
"kicad_symbol_editor") (generator_version "9.0") …)` holding the symbols above,
each with Reference, Value and hidden empty Footprint, Datasheet and
Description properties, its rectangle background-filled in `NAME_0_1`, and its
pins in `NAME_1_1` with names and numbers in 1.27 mm fonts. Save each with
`kicad-cli sym upgrade --force -o <fixture> <draft>`.
