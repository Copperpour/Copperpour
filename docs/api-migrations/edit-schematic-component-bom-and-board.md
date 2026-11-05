### `edit_schematic_component` sets Exclude from BOM and Exclude from board (minor release)

`edit_schematic_component` takes two optional booleans, `in_bom` and `on_board`,
KiCad's native `(in_bom yes|no)` and `(on_board yes|no)` flags: `in_bom: false`
is Exclude from bill of materials and `on_board: false` Exclude from board
([#34](https://github.com/Copperpour/Copperpour/issues/34)). Each is written on
every placed unit, as `dnp` is, and a call that leaves one out keeps the file's
value. `unit` beside either alone is refused, as it is beside `dnp`.

The replies built from the component readback gain `in_bom` and `on_board`, at
the top level and for each unit, read from the saved file:
`edit_schematic_component`, `move_schematic_component`,
`rotate_schematic_component`, `mirror_schematic_component`,
`add_component_annotation`, `replace_component`, and the placement replies of
`add_schematic_component`, `batch_place_components`, `add_power_symbol` and
`batch_add_power_symbol`. Callers needn't change anything.
