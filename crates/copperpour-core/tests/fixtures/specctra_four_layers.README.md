# Four-layer Specctra fixture

`specctra_four_layers.kicad_pcb` was produced by loading
`specctra_two_resistors_locked.kicad_pcb` through KiCad 10.0.8's `pcbnew` API
and saving it through KiCad, after:

- `board.SetCopperLayerCount(4)`, which adds `In1.Cu` and `In2.Cu`;
- adding KiCad's stock `Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical`
  as `J1` at (120, 45) mm, pad 1 on `VCC` and pad 2 on `GND`: its through-hole
  pads are `*.Cu`;
- adding one locked `PCB_TRACK` on `In1.Cu`, `GND`, from the locked via at
  (105, 50) mm to (105, 55) mm.

It was then re-saved with `kicad-cli pcb upgrade --force`. The locked through
via keeps KiCad's `(layers "F.Cu" "B.Cu")` span on four layers, and the layer
table lists copper by ordinal (`F.Cu` 0, `In1.Cu` 4, `In2.Cu` 6, `B.Cu` 2).

`specctra_four_layers.native-kicad-10.dsn` was exported from that board by
KiCad 10.0.8's `pcbnew.ExportSpecctraDSN(board, path)`. Only the absolute
output path in the root `(pcb ...)` identifier was normalized to `board.dsn`.

`specctra_four_layers.native-kicad-10.freerouting-2.3.0.ses` is Freerouting
2.3.0's session for that native DSN (`-mp 2`); only the generated session and
base-design names were normalized to `board`. It routes on `F.Cu`, `In1.Cu`
and `In2.Cu` with two vias.
