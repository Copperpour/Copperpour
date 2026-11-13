### `add_schematic_net_label` refuses a label shape KiCad cannot load or draw (patch release)

`shape` is now one of `input`, `output`, `bidirectional`, `tri_state` and
`passive`, the shapes the sheet-pin tools take. Any other value is refused as
`invalid_argument` on `shape`, through the schema and in the handler, and
nothing is written. On a global or hierarchical label it used to be written as
given: KiCad 10 then could not load the sheet, and a parent sheet's netlist left
its parts out, or, for `dot` or `round` on a hierarchical label, kicad-cli
crashed plotting it. On a `net_label` it was ignored; it is refused
there too.

Omitted, `shape` is still `input`, and a `net_label` given one of the five
still writes none, so a call that sends the schema's default with a plain label
is unchanged. Issue: [#479](https://github.com/Copperpour/Copperpour/issues/479).
