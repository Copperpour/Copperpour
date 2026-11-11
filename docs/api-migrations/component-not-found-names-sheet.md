### A reference on another sheet is refused with that sheet's file (patch release)

`get_component_nets`, `get_pin_connections`, `get_connected_items`,
`get_schematic_component`, `get_schematic_pin_locations` and
`replace_component` read one sheet file. When the reference is not on it but
is on another `.kicad_sch` file in the same directory, the refusal still starts
with `Component '<ref>' not found`, now followed by ` on this sheet (…)` and
the file to pass as `schematic`. A reference recorded only for another
instance of a sheet placed more than once also names the reference that file
saves the symbol as. A reference on no sheet keeps the plain message.
