### `replace_board_outline` (minor release)

A new `pcb_board` tool replaces the board outline with closed contours of
lines and arcs ([#47](https://github.com/Copperpour/Copperpour/issues/47)):
`replace_board_outline(board, contours, dry_run?, expected_plan_revision?)`.
Each contour is an ordered list of `{type: "line", start, end}` and
`{type: "arc", start, mid, end}`, points as `{x, y}` in mm, the line and arc
`set_footprint_graphics` takes. Each primitive ends where the next starts, and
the last where the first starts. A contour inside an outline is a cut-out, and
one inside a cut-out is another outline, as KiCad reads them.

`dry_run` defaults to true: it checks the contours, lists under `replaced` the
board-owned Edge.Cuts shapes it would delete, and returns `plan_revision`. An
apply takes `dry_run: false` and that revision as `expected_plan_revision`. If
the board's Edge.Cuts shapes, the contours or the source have changed since,
the apply is refused as `stale_target`, as `delete_zone` refuses a stale
revision. Edge.Cuts shapes that footprints own are left alone and counted in
`footprint_edge_cuts_count`; the checks below do not include them. Text and
dimensions on Edge.Cuts are not shapes and stay.

Before writing, it refuses with `invalid_argument`, naming the primitive as
`contours[i][j]`: a joint whose two points differ at all; a line of 0.001 mm or
less, or an arc of 0.001 mm or less measured start to mid to end, as KiCad
measures them; an arc whose three points lie on a line, or one so nearly
straight that its centre lies beyond the ±2147.483637 mm KiCad keeps, as does
any coordinate beyond it; and primitives that cross or touch anywhere but at
their shared joint. Arcs are checked as chords within 0.001 mm of them.

On a board KiCad holds, the delete and the draw are one commit and one undo
step; otherwise the saved file is edited. Either way the outline is read back,
and a read-back that differs returns `mutation_outcome_uncertain`. KiCad may
store an arc reversed, with its own midpoint as `mid`, so the read-back
compares arcs by their end points and circle. The response gives each
contour's `role`, the line and arc counts, the box as `get_board_extents` names
it (`x_min` to `height`), `replaced`, `drawn` and `source`.

No existing tool changes. `pcb_board` rises from 17 to 18 tools.
