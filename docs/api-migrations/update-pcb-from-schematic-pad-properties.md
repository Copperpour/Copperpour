### `update_pcb_from_schematic` places pads with their fabrication properties (minor release)

A footprint the sync added lost every pad's fabrication property,
`(property pad_prop_heatsink)`, `pad_prop_bga`, `pad_prop_mechanical` and the
rest, without saying so, and KiCad's DRC then reported
`lib_footprint_mismatch` for it
([#149](https://github.com/Copperpour/Copperpour/issues/149)). On KiCad
10.0.7 and later each added pad now gets its library property, and the
post-apply readback reports a pad property the board lost as
`board_readback_differs`.

KiCad before 10.0.7 can't take the property over its API. There the footprint
is still placed without it, and the dry run and the apply carry a new
non-blocking diagnostic, `pad_properties_not_carried`. It names the footprint in
`footprint_id`, the parts in `references`, and the pads, their properties and
the KiCad version in `message`. KiCad's Tools > Update Footprints from Library
restores the properties. The plan's `status` stays `ready`. Its
`plan_revision` includes whether the properties are carried, so a plan made on
one side of KiCad 10.0.7 is stale on the other.
