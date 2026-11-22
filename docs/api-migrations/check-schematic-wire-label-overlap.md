### `check_schematic_overlaps` reports wires crossing label text (minor release)

The read-only check now returns `wire_label_overlap` when a wire segment enters the text
bounds of a local, global, or hierarchical label or the visible Value of a power symbol.
The check uses the text run, not a label's outline or flag. A wire that attaches at a
label's anchor without entering the run does not count. For a local label, a wire along
the baseline or meeting it from the side opposite the text does not count.

`a` contains the wire UUID and endpoints, `b` identifies the label or power symbol field
in the same form as `text_overlap`, and `bounds_b` gives the text bounds in millimetres.
Existing arguments and finding types are unchanged. Callers that group findings by type
should handle the new type. A previously clean sheet may now have findings, and
`overlap_count` includes them.
