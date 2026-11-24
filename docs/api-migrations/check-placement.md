### Add `check_placement` (minor release)

Load `sch_readability` and call `check_placement` with `schematic` to check one
saved sheet's placement. `overlaps`, `overlap_count`, `bounds_resolved` and
`bounds_unresolved` are the same as `check_schematic_overlaps` returns for that sheet at
its default tolerance. `off_grid_pins` lists each placed symbol pin tip whose X or Y is
not a multiple of 1.27 mm, the nominal KiCad schematic grid, with its reference, symbol
UUID, unit, pin number and position. One KiCad internal unit (0.0001 mm) of drift
counts as off the grid. The project's own connection-grid setting is not read.

To check an expected alignment, pass `alignment_pairs`, an array of
`{a, b, alignment}` records. Each `a` and `b` is a distinct
`{symbol_uuid, pin_number}` selector using the saved placed symbol UUID, not its
reference or a library UUID. `row` compares pin-tip Y; `column` compares pin-tip
X. The check uses the resolved coordinates rounded to six decimal places in mm,
with no extra alignment tolerance. It does not infer pairs from wires or nets.
An omitted, null, or empty list checks zero pairs.

`alignment_pair_count` counts the pairs checked. `misaligned_pairs` lists only
mismatches in request order, with a zero-based `pair_index`, the expected
`alignment`, the absolute perpendicular `offset_mm`, and both resolved pins.
If either selector is missing or ambiguous, the tool refuses the entire call
with `invalid_argument` naming the indexed selector field. It will not choose
between duplicate symbol UUIDs or between different endpoints with the same pin
number. Identical selectors for both ends of a pair are also refused.

Pins come from the placed unit and body style; stacked pins with one number and
position are listed once. A sheet whose symbol pins cannot all be resolved, or whose
wire endpoints or label anchors cannot be read, returns `malformed_file` with its `path`
and `reason` instead of a partial report. A missing file returns `file_not_found`.
The tool does not write, open KiCad, run ERC or follow child sheets.
