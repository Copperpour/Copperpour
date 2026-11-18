### Placed symbols take the library symbol's exclusions (patch release)

`add_schematic_component`, `batch_place_components`, `apply_template`,
`add_power_symbol` and `batch_add_power_symbol` copy the library symbol's
`exclude_from_sim`, `in_bom`, `on_board` and `in_pos_files` onto the symbol
they place, as eeschema does. A `Mechanical:MountingHole`, `(in_bom no)` in
KiCad's library, is now placed out of the BOM; it used to be placed with
`(in_bom yes)`. A library symbol that sets all four, as KiCad 10 writes them,
now also gives its placed symbol `(exclude_from_sim …)` and `(in_pos_files …)`.
A flag the library omits keeps its default. Issue:
[#124](https://github.com/Copperpour/Copperpour/issues/124).
