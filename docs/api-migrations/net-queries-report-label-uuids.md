### Net queries report each label's uuid (patch release)

`get_net_connections`, `get_net_connectivity`, `get_connected_items` and
`trace_from_point` now add `uuid` to each net, global or hierarchical label
they return, the UUID `list_schematic_labels` lists and `batch_delete` takes.
An entry of type `PowerSymbol` has no `uuid`: it stands for a power symbol's
pin, and the symbol's own UUID would delete the symbol. `get_net_connectivity`'s
wires also carry `uuid`, as `get_connected_items`' wires and
`list_schematic_wires` do. Every existing field is unchanged. Issue:
[#93](https://github.com/Copperpour/Copperpour/issues/93).
