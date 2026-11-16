# Rule-area Specctra fixture

Generated with KiCad 10.0.6's native `pcbnew` Python API by
`specctra_rule_areas.generate.py`, starting from `specctra_four_layers.kicad_pcb`.
Run with `/usr/bin/python3` on a machine with KiCad's Python bindings. The
board is KiCad's serialization; only the native DSN's root output path is
normalized to `board.dsn`. UUIDs may differ on regeneration.

Three rule areas inside the 80..130 x 30..70 mm outline, clear of the locked
`GND` track and via at `(100.5, 50) .. (105, 55)`:

| Name | Kind (`tracks` / `vias`) | DSN keep-out | Layers | Outline (mm) |
|---|---|---|---|---|
| `antenna_keepout` | `not_allowed` / `not_allowed` | `keepout` | F.Cu, B.Cu | (113, 44) to (117, 52) |
| `wire_block` | `not_allowed` / `allowed` | `wire_keepout` | F.Cu | (106, 52) to (109, 57) |
| `via_block` | `allowed` / `not_allowed` | `via_keepout` | F.Cu, B.Cu | (96, 44) to (99, 48) |

`specctra_rule_areas.native-kicad-10.dsn` is KiCad 10.0.6's
`pcbnew.ExportSpecctraDSN(board, path)` of that board. It writes five
keep-out polygons (`keepout` on `F.Cu`/`B.Cu`, `wire_keepout` on `F.Cu`, and
`via_keepout` on `F.Cu`/`B.Cu`).

`specctra_rule_areas.native-kicad-10.freerouting-2.5.0.ses` is Freerouting
2.5.0's session for that native DSN (`-mp 2`), run on `board.dsn`. It routes
both `GND` and `VCC` around every active keep-out with zero `items_not_allowed`
violations and zero unconnected items in `kicad-cli pcb drc`.
