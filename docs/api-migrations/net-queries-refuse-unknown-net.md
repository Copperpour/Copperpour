### Net queries refuse a name nothing on the sheet spells (patch release)

`get_net_connections`, `get_net_connectivity` and `get_net_components` now
return a structured `invalid_argument` error on `net` when no label or power
symbol on the sheet spells the name asked for. Previously they answered it as a
net with nothing on it: no pins, labels, wires or components, and no error.
Where a name on the sheet differs only in case, or by the sheet path KiCad's
netlist puts before a local label's name (`/SDA` for `SDA`), the reason offers
it. A net no label or power symbol names has no name these tools take; reach it
with `get_connected_items` or `trace_from_point`. Arguments, and the responses
for a name on the sheet, are unchanged. Issue:
[#101](https://github.com/Copperpour/Copperpour/issues/101).
