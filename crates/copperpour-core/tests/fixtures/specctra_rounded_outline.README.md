# Rounded Edge.Cuts outline fixtures

Generated with KiCad 10.0.6's native `pcbnew` Python API by
`specctra_rounded_outline.generate.py`, starting from the existing
`specctra_two_resistors.kicad_pcb` fixture. Run with `/usr/bin/python3` on a
machine with KiCad's Python bindings. The boards and sibling project are
KiCad's serialization; only the native DSN's root output path is normalized
to `board.dsn`. UUIDs may differ on regeneration.

The rounded board has three convex 2 mm quarter-circle corners and one
concave 5 mm notch. Native export uses the project's `max_error` of 0.005 mm.

| Circle center (board mm) | Radius (mm) | Native chords |
|---|---|---|
| (128, 32) | 2 | 12 |
| (130, 70), concave | 5 | 19 |
| (82, 68) | 2 | 12 |
| (82, 32) | 2 | 12 |

The tests read both native and Rust DSN boundaries and assert these counts,
closure, and vertex and mid-chord radial errors within 5 micrometres. Native
vertices need not coincide with Rust vertices: KiCad distributes error around
the circle. A separate midpoint-to-polyline check in both traversal directions
catches selecting the complementary sweep. At a project tolerance of 0.01 mm,
the Rust exporter uses 9 chords per 2 mm corner and 13 for the notch.

`specctra_multiple_outline_loops.kicad_pcb` adds a disjoint closed triangle.
Its smallest vertex is (70, 40), preceding the rounded board's vertices, so
omitting the all-edges-visited guard would export the triangle alone. The test
requires a multiple-loop refusal. The triangle deliberately lies outside the
rounded boundary; this fixture tests rejection, not a manufacturable board.

Generated `.kicad_prl` files and the multiple-loop board's duplicate project
are not part of the fixtures. No SES round trip is claimed here. D-shaped
two-edge boundaries remain outside this PR's supported profile.
