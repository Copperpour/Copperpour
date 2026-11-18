### A pin on a net no label names reports KiCad's name for that net (patch release)

`get_component_nets`, `get_pin_connections` and `get_pin_net_name` answered
`net: null` for a pin on a net that no label, power symbol or sheet pin names,
the answer they give a pin on nothing. They now give the name KiCad's netlist
gives that net after one of its pins, such as `Net-(R1-Pad2)`, `Net-(U1-VIN)`
or `Net-(U2A-+)`, `unconnected-(…)` when a no-connect flag is on it, and a
hidden `power_in` pin's own name, as KiCad 10.0.8 does, whose unannotated
pins drive their nets. `null` is left for a pin KiCad nets alone, for a pin
whose net a sheet pin carries to the sheet below, and for a pin on a net KiCad
names after an unannotated symbol's pin, whose name carries a hash of that
symbol's UUID. A name a label
gives is unchanged, and the tools that take a net by name still take only
those. Issue: [#618](https://github.com/Copperpour/Copperpour/issues/618).
