### Placement refuses a numbered designator already in use (minor release)

`add_schematic_component`, `batch_place_components` and `apply_template` wrote a
placement under a numbered designator that another placement of the project
already held, on the same sheet or on another sheet of the hierarchy, and
reported success. eeschema's annotation check reports the pair as duplicate
items, and `kicad-cli sch export netlist` warns that the schematic has
annotation errors.

Such a placement is now refused as `invalid_argument` on `reference`, or on
`ref_start` for `apply_template`, naming the placements that hold the
designator (three, and a count of any more), and nothing is written for it.
Only the other units of one multi-unit part, with one value, share a
designator, as `annotate_schematic` keeps one; this also refuses units of two
different parts with one value and unit count, which eeschema does not report.
An unannotated designator (`R?`, the default) is never refused.
`batch_place_components` refuses only that entry, an earlier entry of the same
batch counting as a holder, and places the others; `apply_template` refuses the
whole call. Omit the reference, or give one no placement holds.
`apply_template` with `ref_start` left out still numbers from one above the
highest number on its own sheet, so on a sheet of a hierarchy it can now be
refused; give a `ref_start` no placement holds.

The other sheets are read as `add_power_symbol` reads them for its numbers; a
sheet no project owns is checked against itself alone. A sheet file that could
not be read is named in `other_sheets_unreadable` in the reply of a call that
places, present only then, and its designators were not checked. A placement
on a sheet placed more than once holds its designator once, though each
instance record carries it, as before. Issue:
[#615](https://github.com/Copperpour/Copperpour/issues/615).
