# Stock half-micrometre Specctra fixture

`specctra_stock_half_um.kicad_pcb` extends `specctra_stock_roundrect.kicad_pcb`
with KiCad's stock `Resistor_SMD:R_0805_2012Metric` (`R3` at `(115, 60)` mm,
pad 1 on `VCC`, pad 2 on `GND`), generated with KiCad 10.0.6's `pcbnew` API
by `specctra_stock_half_um.generate.py` and upgraded with
`kicad-cli pcb upgrade --force`.

`specctra_stock_half_um.native-kicad-10.dsn` was exported from that saved board
with `pcbnew.ExportSpecctraDSN` (only the root `(pcb ...)` design name was
normalized to `board.dsn`). It preserves both features from
[#98](https://github.com/Copperpour/Copperpour/issues/98):

- Four image courtyard `(outline (polygon ...))` entries (one per stock
  footprint).
- `R_0805_2012Metric` pins at `-912.5` and `912.5` µm (`±0.9125` mm), with
  `RoundRect[T]Pad_1025.000000x1400.000000_250.951000_um_0.000000_0`
  (1025 × 1400 µm, 250 µm corner radius).
