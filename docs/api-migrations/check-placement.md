### Add `check_placement` (minor release)

Load `sch_readability` and call `check_placement` with `schematic` to check one
saved sheet's placement. `overlaps`, `overlap_count`, `bounds_resolved` and
`bounds_unresolved` are the same as `check_schematic_overlaps` returns for that sheet at
its default tolerance. `off_grid_pins` lists each placed symbol pin tip whose X or Y is
not a multiple of 1.27 mm, the nominal KiCad schematic grid, with its reference, symbol
UUID, unit, pin number and position. One KiCad internal unit (0.0001 mm) of drift
counts as off the grid. The project's own connection-grid setting is not read.

Pins come from the placed unit and body style; stacked pins with one number and
position are listed once. A sheet whose symbol pins cannot all be resolved, or whose
wire endpoints or label anchors cannot be read, returns `malformed_file` with its `path`
and `reason` instead of a partial report. A missing file returns `file_not_found`.
The tool does not write, open KiCad, run ERC or follow child sheets.
