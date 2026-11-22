### `get_board_extents` and `update_pcb_from_schematic` measure live board tables on KiCad 10.0.7+ (patch release)

KiCad 10.0.7 added `kiapi.board.types.Table` and began listing board tables via
`GetItems`. `get_board_extents` and `update_pcb_from_schematic`'s live staging
bounds now decode and measure listed tables instead of reporting them as
`undecodable` in `unavailable_item_classes` ([#757](https://github.com/Copperpour/Copperpour/issues/757)).

On KiCad 10.0.7+, live `get_board_extents` answers include tables in
`measured_item_counts.tables` and in the returned bounding box, and omit
`"tables"` from `unavailable_item_classes` (leaving only `"generators"` on
KiCad 10.0.8). On KiCad releases before 10.0.7, where KiCad refuses `GetItems`
for tables with `AS_BAD_REQUEST`, `"tables"` is still reported as `refused` in
`unavailable_item_classes`. Saved-file reads are unchanged and still report
tables in `unmeasured_item_counts`.
