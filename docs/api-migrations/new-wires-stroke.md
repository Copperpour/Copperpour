### New schematic wires are written with a stroke (patch release)

`add_wire`, `batch_add_wire`, and the stubs `add_power_symbol`,
`batch_add_power_symbol`, `connect_to_net` and `batch_connect_to_net` draw, wrote
each new wire without `(stroke …)`. KiCad reads such a wire, but tools that expect
every wire to have one, such as KiBot, fail on it
([#899](https://github.com/Copperpour/Copperpour/issues/899)). Each new wire now
gets `(stroke (width 0) (type default))`, the stroke KiCad 10.0.8 gives a new wire
and writes for it. Arguments and responses are unchanged. Wires already in a file
keep what they have: re-save a sheet with KiCad, or run `kicad-cli sch upgrade
--force`, to add the stroke to older stroke-less wires.
