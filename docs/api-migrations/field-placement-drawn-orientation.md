### `field_placements` entries now report `drawn_orientation` and document symbol-relative rotation (minor release)

`units[].field_placements.<name>` in schematic component responses (such as
`edit_schematic_component` and `batch_edit_schematic_components`) now includes
`"drawn_orientation"`: `"horizontal"` or `"vertical"`. KiCad stores a field's
`rotation` relative to its parent symbol, so on a 90° or 270° symbol
`rotation: 90` draws horizontally and `rotation: 0` draws vertically; the
`field_placements` schema now documents this explicitly.
