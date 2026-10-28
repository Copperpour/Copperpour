# Placement fixture — provenance

Purpose-built project for the placement, test-point, and voltage-drop
work: an SOIC-8 op-amp (U1) with two 0402 decouplers (C1, C2) on
VCC/GND, a 10k resistor (R1, rotated 90), a 4-pin header (J1), a 0.8mm-
pitch BGA fanout target (U2, board-only), two through-hole test points
(TP1, TP2, board-only), a 60x45mm outline, and a three-segment routed
VCC track run.

Provenance: the schematic was authored through Copperpour's own schematic
tools; the BOARD file's serialization is KiCad's — every footprint was
added by `update_pcb_from_schematic` / `place_component` over live IPC
into a running pcbnew 10.0.5, tracks by `route_trace`, and the file was
written by KiCad's own save (`save_project`), twice. Fields, pad nets
(VCC / GND / /SIG_IN), courtyards, and 3D references are exactly what
KiCad emits.

Regenerating: scripts build_t07b.py (schematic phase, KiCad closed) and
build_t07_board.py (board phase, pcbnew open) in the session scratchpad
that produced this; the flow is ordinary Copperpour tool calls and can be
re-derived from this README's description.
