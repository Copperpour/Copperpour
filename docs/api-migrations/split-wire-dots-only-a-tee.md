### `split_wire_at_point` writes a junction only at a T that has none (patch release)

`split_wire_at_point` used to write a junction dot at every split point: a
second dot where a T already had one, a dot where only the wire's two halves
meet, and a dot where another wire crosses with nothing ending there, which
joined that wire to the split one. KiCad keeps such dots through a resave and
its ERC does not report them. Now the point gets a dot only where a wire end,
pin, sheet pin or bus entry there makes three or more distinct wire exits and
none stands, and the response adds `junctions_added_count`, 0 or 1. A caller
that split a wire at a crossing to join the two adds the dot with
`add_junction`. Issue: [#570](https://github.com/Copperpour/Copperpour/issues/570).
