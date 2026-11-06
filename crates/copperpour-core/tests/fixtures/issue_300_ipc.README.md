# `issue_300_*`

KiCad 10.0.8's own IPC answers for eight footprints on a board built only
from KiCad's stock libraries, and those eight library footprints.
`pcb_footprint_update`'s tests read them for
[#300](https://github.com/Copperpour/Copperpour/issues/300), where
`update_footprints_from_library` refused library footprints with a pad zone
connection or thermal relief setting.

| File | What |
|------|------|
| `issue_300_u1.ipc.bin` | U1, `MountingHole:MountingHole_2.2mm_M2_DIN965_Pad` at (110, 95) mm, 0°, front, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `issue_300_u2.ipc.bin` | U2, `Package_SON:WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias` at (125, 95) mm, 0°, front, likewise |
| `issue_300_u3.ipc.bin` | U3, `Button_Switch_THT:SW_TH_Tactile_Omron_B3F-100x` at (140, 92) mm, 0°, front, likewise |
| `issue_300_u4.ipc.bin` | U4, `Jumper:Jumper_Harwin_S1621_P10.9mm` at (165, 95) mm, 0°, front, likewise |
| `issue_300_u5.ipc.bin` | U5, `Package_DFN_QFN:Texas_RGY_R-PVQFN-N20_EP2.05x3.05mm` at (125, 115) mm, 90°, on the back, likewise |
| `issue_300_u6.ipc.bin` | U6, `Package_DFN_QFN:PQFN-8-EP_6x5mm_P1.27mm_Generic` at (140, 115) mm, 0°, front, likewise |
| `issue_300_u7.ipc.bin` | U7, `Package_DFN_QFN:Texas_B3QFN-14-1EP_5x5.5mm_P0.65mm_ThermalVia` at (155, 115) mm, 0°, front, likewise |
| `issue_300_u8.ipc.bin` | U8, `RF_Antenna:Antenova_SR4G013_GPS` at (170, 115) mm, 0°, front, likewise |
| `issue_300_mountinghole_m2_pad_kicad10.kicad_mod` | KiCad 10.0.8's stock `MountingHole.pretty/MountingHole_2.2mm_M2_DIN965_Pad.kicad_mod`, unchanged |
| `issue_300_wson6_thermalvias_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_SON.pretty/WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias.kicad_mod`, unchanged |
| `issue_300_omron_b3f_100x_kicad10.kicad_mod` | KiCad 10.0.8's stock `Button_Switch_THT.pretty/SW_TH_Tactile_Omron_B3F-100x.kicad_mod`, unchanged |
| `issue_300_harwin_s1621_kicad10.kicad_mod` | KiCad 10.0.8's stock `Jumper.pretty/Jumper_Harwin_S1621_P10.9mm.kicad_mod`, unchanged |
| `issue_300_texas_rgy_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_DFN_QFN.pretty/Texas_RGY_R-PVQFN-N20_EP2.05x3.05mm.kicad_mod`, unchanged |
| `issue_300_pqfn8_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_DFN_QFN.pretty/PQFN-8-EP_6x5mm_P1.27mm_Generic.kicad_mod`, unchanged |
| `issue_300_b3qfn14_thermalvia_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_DFN_QFN.pretty/Texas_B3QFN-14-1EP_5x5.5mm_P0.65mm_ThermalVia.kicad_mod`, unchanged |
| `issue_300_antenova_sr4g013_kicad10.kicad_mod` | KiCad 10.0.8's stock `RF_Antenna.pretty/Antenova_SR4G013_GPS.kicad_mod`, unchanged |

KiCad's `BoardEnabledLayersResponse` for this board (2 copper layers) was
byte for byte `issue_283_enabled_layers.ipc.bin`, so the tests read that one.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, macOS arm64 |
| Board | A new 2-layer board made with KiCad 10.0.8's `pcbnew` Python: each footprint loaded with `FootprintLoad` from KiCad's stock library as shipped, its library id set with `SetFPID`, placed with `SetPosition`, no nets. U5 was flipped with `Flip(…, FLIP_DIRECTION_TOP_BOTTOM)`, then turned to 90° with `SetOrientationDegrees`. Saved with `SaveBoard`, then opened and saved by KiCad 10.0.8's PCB Editor |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` and `GetBoardEnabledLayers` over the running PCB Editor's IPC socket, read-only, using protobuf classes generated from KiCad 10.0.8's `api/proto` |
| Captured | 2026-11-05, after the PCB Editor's save; the footprint and pad KIIDs in the answers are those in the saved file |

Each `.ipc.bin` footprint is the `Any.value` of one item of KiCad's
`GetItemsResponse`, byte for byte, as the `issue_254_*` captures store them.

## What each footprint shows

KiCad reports every pad with `zone_settings`: a connection and a spoke angle.
A pad whose library sets neither is reported `ZCS_INHERITED` at 45° if round
and 90° if rectangular or rounded. No pad here reports a spoke gap.

Refresh reads U1 to U5 whole:

- **U1: `(zone_connect 2)` on a round plated pad.** Reported `ZCS_FULL`, 45°.
- **U2: `(zone_connect 2)` beside `(property pad_prop_heatsink)`.** The two
  rectangular pad-7 copper areas are reported `ZCS_FULL`, 90°, and
  `PFP_HEATSINK`; its two round thermal vias, with only the property,
  `ZCS_INHERITED`, 45°.
- **U3: `(thermal_bridge_angle 90)` on one round pad, without
  `zone_connect`.** That pad is reported `ZCS_INHERITED`, 90°; the other three
  round pads 45°.
- **U4: `(thermal_bridge_angle 45)` on two rounded rectangles.** Reported
  `ZCS_INHERITED`, 45°, not their default 90°.
- **U5: `(zone_connect 2) (thermal_bridge_angle 45)` on 20 rounded
  rectangles, on the back.** Reported `ZCS_FULL`, 45°. Flipped top to bottom,
  KiCad keeps its keep-upright text at the angle refresh builds (#319).

The tests read U6 to U8 only pad by pad, for the values U1 to U5 don't have.
Refresh still refuses all three for other clauses: U6 for a pad's
`solder_paste_margin`, U7 for its custom pad, U8 for `net_tie_pad_groups`.

- **U6: `(zone_connect 0)` on 16 rectangles.** Reported `ZCS_NONE`, 90°.
- **U7: `(zone_connect 1)` on 11 round thermal vias.** Reported `ZCS_THERMAL`,
  45°, and `(zone_connect 0)` on eight rounded rectangles, `ZCS_NONE`, 90°.
- **U8: `(thermal_bridge_width 0.5)` on three rounded rectangles, the stock
  library's only pad spoke width.** Reported `ZCS_INHERITED`, 90°, 0.5 mm.
  Another pad 2 has `(zone_connect 2) (thermal_bridge_angle 45)`, reported
  `ZCS_FULL`, 45°.

The tests skip U6's and U7's custom pads, which refresh refuses.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock library's names, descriptions, keywords and `${KICAD10_3DMODEL_DIR}`
model paths, the references U1 to U8, field names, KIIDs and protobuf type
URLs. They hold no path, username or hostname.

## Regenerating them

With KiCad 10.0.8's Python, load the eight stock footprints, place them as
above, flip U5 top to bottom and turn it, and save the board. Open it in the
PCB Editor with the API enabled and save it there. Then send `GetItems` for
footprints and `GetBoardEnabledLayers` for that board without changing
anything, and write each footprint item's `Any.value` and the layers
response's bytes unchanged.
