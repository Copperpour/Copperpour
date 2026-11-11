### The Specctra round trip keeps copper zones as planes (minor release)

`export_specctra_dsn` no longer refuses a board with copper zones
([#430](https://github.com/Copperpour/Copperpour/issues/430), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Each zone is
written as its outline, one DSN `plane` of the zone's net on every copper
layer it covers, as KiCad's own exporter writes it; its fill is not
exported. Freerouting then reaches a plane through vias instead of routing
its net. Still refused, each with its own message: a rule area (keep-outs stay
with [#81](https://github.com/Copperpour/Copperpour/issues/81)), a zone
without a net, an outline with a cutout, an outline point that is not a
straight-edge corner, a zone on a layer that is not one of the board's copper
layers, a zone inside a footprint, and a zone whose net has no pad.

A copper layer the board's layer table marks `power` is now a DSN `power`
layer, where Freerouting lays no track, as KiCad's exporter writes it; it was
always `signal`. `signal`, `mixed` and `jumper` layers stay `signal`. With
the KiCad 10 native bridge, KiCad's planes must be the same outlines on the
same layers and nets, in any order, and its layer types the same, or the
native DSN is refused.

The reverse manifest moves to schema 3. `supported_profile.copper_zones` is
`true` when the board has zones, and the new `zones` list counts them per
copper layer and net (`{"layer", "net", "count"}`); a board without zones
omits it. `apply_specctra_ses` and `plan_specctra_ses_import` refuse a
manifest whose inventory differs from the live board's, and still read a
schema 1 or 2 manifest of a board without zones.

The import never creates, changes, deletes or refills a zone. Both tools'
responses have a new `zone_refill` object: `required` (whether the board has
zones), `zones` (the manifest's inventory) and `remedy` (what to run, or
`null`). It is a declared consequence of the import, not an observation of
the fills ([#71](https://github.com/Copperpour/Copperpour/issues/71)): the
candidate DRC of `apply_specctra_ses` runs on the fills as exported, so
refill them (`refill_zones`, or `run_drc` with `refill_zones` and
`sync_live_board`) before judging it.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes`,
and `capabilities.copper_zones` is new: the exported board's zone count.
