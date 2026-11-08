### Wire tools dot only the T's their own wire forms (patch release)

`add_wire`, `batch_add_wire`, and the wire stubs of `connect_to_net` and
`batch_connect_to_net` used to add a junction at every junction-less T on the
sheet, wherever it was. KiCad treats a wire end that lands mid-span without a
junction as a separate net, so any call joined those nets without saying so.
These tools now add a junction only where the wire they draw forms a T or
meets three or more exits, as the routed tools have done since mixelpixx/Konnect#887
([#118](https://github.com/Copperpour/Copperpour/issues/118)).
To join a T that has no junction, call `add_junction` at that point.
