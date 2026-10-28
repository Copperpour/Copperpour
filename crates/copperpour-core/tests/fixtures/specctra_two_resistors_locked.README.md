# Locked-routing Specctra fixture

`specctra_two_resistors_locked.kicad_pcb` was produced by loading
`specctra_two_resistors.kicad_pcb` through KiCad 10.0.5's `pcbnew` API, adding
one locked `PCB_TRACK` and one locked through `PCB_VIA`, and saving through
KiCad. Its matching `.native-kicad-10.dsn` file was then produced by KiCad
10.0.5's native `ExportSpecctraDSN` implementation. The matching
`.freerouting-2.3.0.ses` was produced by routing that DSN through Freerouting
2.3.0 with two passes. The native DSN's identifying `pcb` atom was normalized
from the generation-machine output path to the stable fixture filename; no
routing semantics were changed.

The `_locked_arc` pair was produced the same way with one additional locked
`PCB_ARC`. These native files are parity oracles, not runtime dependencies. The
exports prove that KiCad represents locked straight tracks and vias as
Specctra `type fix`, while lowering the locked arc to its straight start/end
chord. Copperpour's fail-closed Rust profile therefore rejects locked arcs rather
than silently approximating their geometry.
