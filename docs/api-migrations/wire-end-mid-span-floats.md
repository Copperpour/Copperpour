### `validate_wire_connections` and `find_orphan_items` report a wire end lying mid-span without a junction (patch release)

Both tools counted a wire end that lies mid-span on another wire as connected,
with or without a junction there. eeschema joins the two only through a
junction or a label at the point, so KiCad's netlist keeps them apart and its
ERC reports the end as `unconnected_wire_endpoint`. Both tools now report it
too: `validate_wire_connections` among `floating_endpoints`, and
`find_orphan_items` as a `dangling_wire_end`
([#347](https://github.com/Copperpour/Copperpour/issues/347)). A junction or a
label at the point still joins them. To join such an end, add a junction there
with `add_junction`.
