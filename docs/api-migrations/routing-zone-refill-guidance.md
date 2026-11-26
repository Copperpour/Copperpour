### Copper routing edits report zone-refill guidance (minor release)

Successful `route_trace`, `route_pad_to_pad`, `add_via`, `delete_trace`,
`modify_trace`, and `route_differential_pair` responses add a `zone_refill`
object to the existing JSON body. It contains only a `remedy` string. The
tools neither inspect nor refill zones, so the field does not say whether
the board has zones or whether any fill is stale. Refusals and errors do
not include this field. All existing response fields are unchanged.

If the edited board has copper zones, await the edit and then run `run_drc`
on that board with `refill_zones: true` and `sync_live_board: true`. Or call
`refill_zones` on the live board first and await it, then save the board
before running DRC against the saved file. `refill_zones` does not save
the board itself.

The Specctra plan and apply responses keep their inventory-based
`zone_refill` fields (`required`, `zones`, `remedy`) unchanged.
