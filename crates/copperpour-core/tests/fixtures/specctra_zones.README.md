# Copper-zone Specctra fixture

Generated with KiCad 10.0.8's native `pcbnew` Python API by
`specctra_zones.generate.py`, starting from `specctra_four_layers.kicad_pcb`.
Run with `/usr/bin/python3` on a machine with KiCad's Python bindings. The
board is KiCad's serialization, zones filled by KiCad's `ZONE_FILLER`; only
the native DSN's root output path is normalized to `board.dsn`. UUIDs may
differ on regeneration.

Three zones, inside the 80..130 x 30..70 mm outline:

| Zone | Net | Layers | Outline (mm) |
|---|---|---|---|
| `gnd_plane` | GND | In1.Cu | (81, 31) to (129, 69) |
| `vcc_plane` | VCC | In2.Cu | (81, 31) to (129, 69) |
| `gnd_pour` | GND | F.Cu, B.Cu | (82, 60) to (95, 68) |

`specctra_zones.native-kicad-10.dsn` is KiCad 10.0.8's
`pcbnew.ExportSpecctraDSN(board, path)` of that board. It writes four
planes, one per zone and layer, each a closed polygon of the zone's outline;
it lists the outer pour's planes first.

`specctra_zones.native-kicad-10.freerouting-2.3.0.ses` is Freerouting 2.3.0's
session for that native DSN (`-mp 2`), run as written; the session and base
design are already named `board`. It routes on `F.Cu` only: GND by one wire
between the resistors, and each VCC pad by a short wire to a via that drops to
the VCC plane on `In2.Cu`. The session has wires and vias only: no zone, plane
or fill comes back.

Its copper layers are all `signal` in the layer table. The power-layer test
edits it in memory to mark `In1.Cu` `power` and `In2.Cu` `mixed`: KiCad
10.0.8's exporter, run on that edit (`board.SetLayerType`), writes `In1.Cu` as
`(type power)` and the three others as `(type signal)`.
