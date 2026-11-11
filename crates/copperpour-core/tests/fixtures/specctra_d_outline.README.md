# D-shaped Edge.Cuts outline fixture

Generated with KiCad 10.0.8's native `pcbnew` Python API by
`specctra_d_outline.generate.py`, starting from the existing
`specctra_two_resistors.kicad_pcb` fixture. Run with `/usr/bin/python3` on a
machine with KiCad's Python bindings. The board is KiCad's serialization; only
the native DSN's root output path is normalized to `board.dsn`. UUIDs may
differ on regeneration.

The outline has two edges: one line from (90, 25) to (90, 75) and one half
circle through (115, 50), center (90, 50), radius 25 mm. Both resistors, at
(100, 50) and (110, 50), lie inside it. No `.kicad_pro` is kept: KiCad and the
Rust exporter both use the 5 µm default `max_error`.

| Output | Arc chords | Straight edges |
|---|---|---|
| KiCad 10.0.8 native export | 80 | 1 |
| Rust exporter | 85 | 1 |

The counts differ at this radius. KiCad rounds the chord count and spreads the
error around the circle (its vertices sit up to 2.8 µm outside it). The Rust
flattener keeps every chord within `max_error` minus the 0.707 µm DSN grid
allowance, so it rounds up. Both stay within 5 µm of the circle, which the
test checks on every vertex and chord midpoint.

The refusal tests derive two boards from this one in memory: two straight
lines between the same two points, and the same arc drawn twice. Both close a
loop that encloses no area. No SES round trip is claimed here.
