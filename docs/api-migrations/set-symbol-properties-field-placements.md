### `set_symbol_properties` places library fields (minor release)

[#867](https://github.com/Copperpour/Copperpour/issues/867). No tool could move a
library symbol's field: when a body grew, its Reference and Value overlapped it
until they were moved by hand in KiCad's symbol editor.

`set_symbol_properties` takes `field_placements`, keyed by field name, as
`edit_schematic_component` does for placed symbols. Each entry may set `x`, `y`,
`rotation` and `hide`; an omitted part stays as the file has it.

- **Coordinates** are the symbol's own, as the library file and
  `create_symbol` use them: millimetres from its origin, with y up.
  They are rounded to KiCad's 0.0001 mm, and `rotation` is written in
  [0, 360).
- **Hiding** writes `(hide yes)` before the field's `(effects …)`, as KiCad 10
  writes it. **Showing** takes out every flag that hid the field, the KiCad 6
  to 8 `hide` inside `(effects …)` included.
- **Same call:** fields are named as the call leaves them, so a field set in
  `fields` or renamed in `rename_fields` can be placed (by its new name).
- **Answer:** `field_placements` gives each named field's placement before and
  after. A placement already in place writes nothing.
- **Refused** as `invalid_argument` before writing: an empty object, an entry
  that sets nothing or an unknown key, a non-numeric coordinate, a non-boolean
  `hide`, a `ki_*` name, a name in `remove_fields`, a name `rename_fields`
  renames away, and a name the symbol does not carry after the call's other
  changes.
