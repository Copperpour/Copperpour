### `move_schematic_component` accepts `unit` (minor release)

`move_schematic_component` moved the lowest-numbered unit of a component to
`x`, `y` and translated every other placed unit by the same delta, so one unit
of a multi-unit symbol could not be repositioned alone
([#44](https://github.com/Copperpour/Copperpour/issues/44)). The new optional
`unit` moves only that placed unit to `x`, `y`; every other unit stays where the
file has it, and the saved-file readback holds each of them to that. A `unit`
the component has no placement for is refused as `invalid_argument` with the
placed units named, before anything is written. A unit carried by more than one
placement is refused as `ambiguous_target`, as every component edit already
refuses it.

Calls without `unit` behave as before. With `unit`, `moved_units` is 1; the
moved unit's new position is its entry in `placements`, which still lists every
unit. As in every component tool, the top-level `x` and `y` describe the
lowest-numbered unit.
