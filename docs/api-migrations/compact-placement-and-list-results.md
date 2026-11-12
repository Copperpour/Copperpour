### Placement and component-list results are compact by default (minor release)

`add_schematic_component`, `add_power_symbol` and `batch_place_components` now
return a compact entry per placed part: `reference`, `uuid`, `lib_id`,
`value`, `footprint`, `x`, `y`, `rotation` and `unit`, plus `mirror_x`,
`mirror_y`, `dnp`, `unit_count`, `in_bom` or `on_board` only when they differ
from the default. The single-part tools also keep `schematic`, and their
`junctions_added`, `added_power` and `lcsc_catalog`. The full readback
(`fields`, `units`, `field_placements`, `instance_paths`, `project`, `added`,
`datasheet`) is still verified after every write; pass `verbose: true` to have
it returned as before.

`list_schematic_components` now leaves out of each row's `properties` the
fields the row already carries (`Reference`, `Value`, `Footprint`), empty
fields, and `Datasheet` and `Description`. Pass `verbose: true` for every field,
or read one part with `get_schematic_component`.
