### `estimate_cost` reports the board's size from its whole Edge.Cuts outline (patch release)

`board.width_mm` and `board.height_mm` were 0 for every board KiCad 10 saved,
because the size was read by scanning the file text for single-line `gr_line`
items, and KiCad writes one field per line. Arc, rectangle, circle and polygon
outlines were never read. The size is now the bounding box of every Edge.Cuts
outline graphic on the board, arcs by their real extent, read the way
placement reads the outline. It is rounded to KiCad's 1 nm grid.
`get_board_extents` is unchanged: it measures every item, not only the outline.

A board without an outline still reports 0 x 0, and so does one with an
outline graphic that cannot be read. Both now add a warning to `warnings`
saying the size was not read. Callers that took 0 to mean "unknown" can keep
doing so, or check `warnings`. No price changes: the PCB price uses only the
layer count.
