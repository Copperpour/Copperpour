### Add `compare_schematic_layout` (minor release)

Load `sch_readability` to score a saved `schematic` sheet. Pass `baseline` to compare
two saved copies of the same sheet. The tool reads neither unsaved editor changes
nor other sheets. It writes nothing and does not run KiCad.

A missing or unreadable sheet returns `file_not_found` with its path. Invalid
schematic content returns `malformed_file` with the path and reason.

`score` is a vector, not a weighted total. With a baseline, `baseline_score`
reports the same fields and `delta` gives current minus baseline for each scored
term. `bounds_unresolved`, `wire_segments` and `power_symbols` are diagnostics,
not ranking terms. Unavailable terms are `null` and appear in `not_compared`.
`pin_contact` and `short_pin_wire` follow `find_pin_contacts` with its default
2.54 mm minimum. Without a baseline, `baseline_score`, `delta` and `verdict`
are `null`.

`blocked` means the comparison could not establish that the two sheets preserve
pin nets and symbol identity. See `reasons`. For an intentional power-symbol
removal, pass its placed UUID in `removed_power_symbol_uuids`. Remaining pins
must keep their nets, and each named net must retain its name and scoped aliases.
The comparison does not check the ERC effect of removing a power symbol.

Otherwise `improved` means the first tier with a different sum went down and
no individual term in the first two tiers went up. The tiers, in order, are
connectivity-related findings, text and pin visibility, wire and power-symbol
clutter, then wire length in mm. `wire_field_overlap` counts the same crossings as
`check_schematic_overlaps` in the text and pin visibility tier. Moving a field clear
of a wire can improve the score without changing connectivity. `unchanged` means
every compared term tied; other comparisons are `not_improved`. The wire clutter
terms count simple bends, straight splits, diagonals, stacked wire pairs, and excess
supply symbols for the same rail on one drawn node, not across separate islands
joined by name.
`PWR_FLAG` does not count toward that redundancy term.

Compare alternative results of one authoring call, not the input with the new
connectivity. For idempotence, also check that the operation under test wrote no
bytes. An `unchanged` score alone does not prove that.

For clearance, check the actual items a call adds or moves. They must add no
`check_schematic_overlaps` finding (including `wire_label_overlap`), collinear
wire overlap or pin contact. A better sheet-wide score does not establish
clearance for an individual item. Wire-label overlaps and the wire-run measures
from #768 are not ranking terms in this version.
