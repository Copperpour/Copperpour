### `check_schematic_wire_runs` measures drawn wire runs (minor release)

Pass `schematic` for one saved sheet. The `sch_readability` tool flags runs at least 100 mm
long or with at least 10 mm of extra wire over the direct orthogonal distance. This
includes a wire that doubles back along the same line without a bend. Both thresholds
are positive, absolute millimetres. Override them with `long_run_mm` and `detour_mm`;
they do not scale with paper size.

Findings list ordered segments with their saved wire UUIDs, endpoint status and the
sheet's net name if known, including pin-derived names. A bare crossing does not split
a run, and a near miss does not count as an attachment. Use `pin_geometry_resolved` to
check whether open ends could be established.
Pin-derived names use the owning project's symbol-unit notation when available and
KiCad's default notation otherwise.
Buses, diagonal and zero-length wires, and other sheets are not scored. A run that
returns to its start is skipped. If attachments divide a loop into runs with distinct
ends, each run is measured as usual.
A missing `schematic` returns `file_not_found` with its `path`. A file that does not
parse as a KiCad schematic returns `malformed_file` with its `path` and `reason`.
Unreadable wire segments count as skipped. Unreadable positions on labels, junctions,
no-connect markers or sheet pins return `malformed_file`.
The tool does not edit the sheet or establish that a shorter route would be safe.
