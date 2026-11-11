# Bottom-side Specctra fixture

`specctra_bottom_side.kicad_pcb` is `specctra_four_layers.kicad_pcb` with
footprints on the bottom, made and saved by KiCad 10.0.8's `pcbnew` API with
`specctra_bottom_side.generate.py`, then re-saved with
`kicad-cli pcb upgrade --force`:

- `R2` turned to 90 degrees, then flipped left/right (pcbnew's `F` key);
- KiCad's stock `Package_TO_SOT_SMD:SOT-23W` added as `U1` at (112, 56) mm,
  pad 2 on `GND` and pads 1 and 3 on `VCC`, turned to 30 degrees, then flipped
  left/right: its three pads are asymmetric, so a mirror error moves pin 3;
- KiCad's stock `Resistor_SMD:R_0603_1608Metric` added as `R3` at
  (104, 56) mm, pad 1 on `VCC` and pad 2 on `GND`, pad 2 turned 90 degrees
  more than its footprint, the footprint turned to 45 degrees, then flipped
  left/right;
- the through-hole header `J1` flipped top/bottom.

The stock footprints were chosen with whole-micrometre pads: the native-DSN
reader still refuses KiCad's sub-micrometre pin positions (#841).

KiCad keeps a bottom footprint's pads mirrored about its local X axis
(`U1` pad 1 at `(-1.2 0.95)`, the library has `(-1.2 -0.95)`), with
`(layers "B.Cu" "B.Mask" "B.Paste")` for an SMD pad.

`specctra_bottom_side.native-kicad-10.dsn` was exported from that board by
KiCad 10.0.8's `pcbnew.ExportSpecctraDSN(board, path)`. Only the absolute
output path in the root `(pcb ...)` identifier was normalized to `board.dsn`.
KiCad writes each bottom footprint's front image (SMD pads on `F.Cu`) and
places it `back` at its KiCad rotation plus 180: `R2` 270, `U1` 330, `R3`
315 (its pad 2 `(rotate 90)`), `J1` 180.

`specctra_bottom_side.native-kicad-10.freerouting-2.3.0.ses` is Freerouting
2.3.0's session for that native DSN (`-mp 2`). It keeps every placement,
routes the bottom pads on `B.Cu` (9 wires), and also uses `F.Cu`, `In1.Cu`
and `In2.Cu` with three vias: no connection unrouted, no violation.
