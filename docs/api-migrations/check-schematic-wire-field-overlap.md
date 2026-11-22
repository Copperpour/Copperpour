### `check_schematic_overlaps` reports wires crossing symbol field text (minor release)

The read-only check now returns `wire_field_overlap` for each wire segment that enters
the text run of a visible Reference or Value on an ordinary symbol. Touching an edge
does not count. Ending at a field anchor does count if the wire enters the run: the
anchor is not an electrical attachment point. Power symbol Values are still reported
only as `wire_label_overlap`. Custom fields and free text are not checked for wire
crossings.

A wire through both a symbol body and one of its fields can report both `wire_overlap`
and `wire_field_overlap`.

Power classification uses the embedded `(power)` marker, not the library ID or
reference. A symbol with unresolved body and pin geometry is listed in
`bounds_unresolved`. If its definition is missing, a surviving visible Reference or
Value can still report as `wire_field_overlap`. If the definition still has the
`(power)` marker, a crossed Value remains a `wire_label_overlap`.

`a` gives the wire UUID and endpoints, `b` identifies the field in the same form as
`text_overlap`, and `bounds_b` gives the text run's bounds in millimetres. Existing
arguments and finding types are unchanged. Callers that group findings by type should
handle `wire_field_overlap`. Previously clean sheets can now report findings, which
are included in `overlap_count`.
