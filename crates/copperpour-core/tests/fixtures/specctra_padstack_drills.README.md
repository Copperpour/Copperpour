# Specctra fixture: one pad shape, two drills

Generated with KiCad 10.0.8's native `pcbnew` Python API by
`specctra_padstack_drills.generate.py`, starting from
`specctra_two_resistors.kicad_pcb`. Run with `/usr/bin/python3` on a machine
with KiCad's Python bindings and its stock footprint library. The board is
KiCad's serialization; only the native DSN's root output path is normalized to
`board.dsn`. UUIDs may differ on regeneration.

Two of KiCad's stock `Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical`
headers are added, pad 1 on `VCC` and pad 2 on `GND`:

| Header | At (mm) | Pads | Drill |
|---|---|---|---|
| `J1` | (120, 40) | 1.7 mm, stock | 1.0 mm, stock |
| `J2` | (125, 40) | 1.7 mm, stock | 0.8 mm |

`specctra_padstack_drills.native-kicad-10.dsn` is KiCad 10.0.8's
`pcbnew.ExportSpecctraDSN(board, path)` of that board. It writes one image for
both headers and two through-hole padstacks, `Rect[A]Pad_1700.000000x1700.000000_um`
and `Round[A]Pad_1700.000000_um`, named after the copper only: the 0.8 and 1.0
mm holes are not told apart.

`specctra_padstack_drills.freerouting-2.3.0.ses` is Freerouting 2.3.0's session
(`-mp 2`, 0 unrouted, 0 violations) for Copperpour's own export of the board,
which names one padstack per shape and drill; only the session and base-design
names were normalized to `board`.
