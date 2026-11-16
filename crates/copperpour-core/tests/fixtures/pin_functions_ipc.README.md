# `pin_functions_*`

KiCad 10.0.8's own IPC answers for two stock footprints whose pads carry
schematic pins (pin function and pin type, `symbol_pin` in the message), and
their library footprints. `pcb_footprint_update`'s tests read them: an applied
`update_footprints_from_library` rebuilt every pad without `symbol_pin`, and
`PAD::Deserialize` then emptied the pad's pin function and type.

| File | What |
|------|------|
| `pin_functions_j1.ipc.bin` | J1, `Connector_PinHeader_2.54mm:PinHeader_1x03_P2.54mm_Vertical` at (120, 80) mm, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `pin_functions_j2.ipc.bin` | J2, `Connector_JST:JST_SH_SM02B-SRSS-TB_1x02-1MP_P1.00mm_Horizontal` at (135, 80) mm, likewise |
| `pin_functions_pinheader_1x03_p2.54mm_vertical_kicad10.kicad_mod` | KiCad 10.0.8's stock `Connector_PinHeader_2.54mm.pretty/PinHeader_1x03_P2.54mm_Vertical.kicad_mod`, with LF line endings as in KiCad's footprint library (the Windows installer ships it with CRLF); otherwise unchanged |
| `pin_functions_jst_sh_sm02b-srss-tb_1x02-1mp_p1.00mm_horizontal_kicad10.kicad_mod` | KiCad 10.0.8's stock `Connector_JST.pretty/JST_SH_SM02B-SRSS-TB_1x02-1MP_P1.00mm_Horizontal.kicad_mod`, likewise |

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Windows 11 |
| Board | A new board made with KiCad 10.0.8's `pcbnew` Python (`NewBoard`): each footprint loaded with `FootprintLoad` from KiCad's stock library as installed, its library id set with `SetFPID`, its reference set, placed with `SetPosition`, and each pad given a pin with `PAD::SetPinFunction` and `PAD::SetPinType`, the setters Update PCB from Schematic uses, and a made-up net with `SetNet`; saved with `SaveBoard`. The board was then opened in KiCad 10.0.8's PCB Editor, unchanged |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` over the running PCB Editor's IPC socket, read-only, as `update_footprints_from_library` reads them |
| Captured | 2026-11-16 |

Each `.ipc.bin` is the `Any.value` of one item of KiCad's `GetItemsResponse`,
byte for byte, as the `c_0603_*`, `issue_254_*` and `testpoint_circle_*`
captures store them.

The pins, as the saved board file wrote them:

| Footprint | Pad | `pinfunction` | `pintype` | Net |
|---|---|---|---|---|
| J1 | 1 | `A_1` | `passive` | `SIG_A` |
| J1 | 2 | `C_2` | `passive` | `SIG_C` |
| J1 | 3 | `B_3` | `passive` | `SIG_B` |
| J2 | 1 | `SDA` | `bidirectional` | `SDA` |
| J2 | 2 | `NC` | `passive+no_connect` | none |
| J2 | first `MP` | `SHIELD_A` | `passive` | `SHIELD` |
| J2 | second `MP` | `SHIELD_B` | `passive` | `SHIELD` |

J1 is a 1x03 header with the pins Update PCB from Schematic gave such headers
on a real board. J2's pad 2 is a pin the schematic marks unconnected, which
KiCad writes as `+no_connect` on the pin type and reports as
`symbol_pin.no_connect`. Its two `MP` pads were given different pins, as a hand
edit of pad properties would; Update PCB gives pads that share a number the
same pin.

## What they show

- **KiCad reports the pins.** Each pad's `symbol_pin` holds the pin function as
  `name`, the pin type as `type`, and `no_connect` for J2's pad 2.
- **The refresh.** Rebuilt from the library files, J1's and J2's pads keep
  their nets and pins by pad number. For J2's `MP` pads, the first one's pin
  (`SHIELD_A`) goes to both, the rule the builder already uses for the net.
  KiCad's own footprint exchange pairs pads by number, position and padstack
  (`matchItemsBySimilarity` in `pcbnew/pcb_edit_frame.cpp`), so there each
  `MP` pad would keep its own.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock library's ids, descriptions, keywords, datasheet URL and fields, the
references J1 and J2, the made-up nets and pins, field names, KIIDs and
protobuf type URLs. They hold no path, username or hostname.

## Regenerating them

Build the board as above with KiCad's Python, open it in KiCad's PCB Editor
with the API enabled, and without changing anything send `GetItems` for
`KOT_PCB_FOOTPRINT` for that board; write each returned footprint's
`Any.value` unchanged. The library files are copied from KiCad's stock
footprint directory, with their line endings made LF.
