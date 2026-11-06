# `issue_283_*`

KiCad 10.0.8's own IPC answers for three footprints on a board built only
from KiCad's stock libraries, and those two library footprints.
`pcb_footprint_update`'s tests read them for
[#283](https://github.com/Copperpour/Copperpour/issues/283), where
`update_footprints_from_library` refused library footprints with a pad
fabrication property or a reference point.

| File | What |
|------|------|
| `issue_283_u1.ipc.bin` | U1, `Package_DFN_QFN:DFN-14-1EP_3x4.5mm_P0.65mm_EP1.65x4.25mm_ThermalVias` at (110, 95) mm, 0°, front, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `issue_283_u2.ipc.bin` | U2, `Package_DIP:DIP-8_W7.62mm` at (125, 95) mm, 0°, front, likewise |
| `issue_283_u3.ipc.bin` | U3, the same DIP-8 at (145, 95) mm, 90°, on the back, likewise |
| `issue_283_enabled_layers.ipc.bin` | KiCad's `BoardEnabledLayersResponse` for the board: 2 copper layers |
| `issue_320_u1_after_refresh.ipc.bin` | U1 again, as KiCad reported it after refresh had restored its pin-1 triangle ([#320](https://github.com/Copperpour/Copperpour/issues/320)), likewise |
| `issue_283_dfn14_thermalvias_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_DFN_QFN.pretty/DFN-14-1EP_3x4.5mm_P0.65mm_EP1.65x4.25mm_ThermalVias.kicad_mod`, unchanged |
| `issue_283_dip8_kicad10.kicad_mod` | KiCad 10.0.8's stock `Package_DIP.pretty/DIP-8_W7.62mm.kicad_mod`, unchanged |

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, macOS arm64 |
| Board | A new 2-layer board made with KiCad 10.0.8's `pcbnew` Python: each footprint loaded with `FootprintLoad` from KiCad's stock library as shipped, its library id set with `SetFPID`, placed with `SetPosition`, no nets. U3 was flipped with `Flip(…, FLIP_DIRECTION_LEFT_RIGHT)`, the direction KiCad 10's PCB Editor uses by default, then turned to 90° with `SetOrientationDegrees`. Saved with `SaveBoard`, then opened and saved by KiCad 10.0.8's PCB Editor |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` and `GetBoardEnabledLayers` over the running PCB Editor's IPC socket, read-only, using protobuf classes generated from KiCad 10.0.8's `api/proto` |
| Captured | 2026-11-05, after the PCB Editor's save; the footprint and reference-point KIIDs in the answers are those in the saved file |

Each `.ipc.bin` footprint is the `Any.value` of one item of KiCad's
`GetItemsResponse`, byte for byte, as the `issue_254_*` captures store them.

## What each footprint shows

- **U1: pad fabrication properties.** The library's eight thermal-via pads,
  number 15, have `(property pad_prop_heatsink)`. KiCad reports them as
  `PFP_HEATSINK` and its other 25 pads as `PFP_NONE`.
- **U2: a reference point on the front.** The library's
  `(point (at 3.81 3.81) (size 1.27) (layer "F.Fab"))` is reported as a
  `kiapi.board.types.ReferencePoint` on F.Fab at (128.81, 98.81) mm, 1.27 mm,
  unlocked.
- **U3: a reference point on the back.** The same point, mirrored and turned
  with the footprint, is reported on B.Fab at (141.19, 91.19) mm.

U3's `${REFERENCE}` text on B.Fab is reported at 0°, where refresh builds
180°. The text is keep-upright, so both draw the same. The angle KiCad keeps
depends on the flip direction: flipped top to bottom instead, KiCad keeps
180°. That difference is not part of #283, so the tests compare only U3's
pads and point.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock library's names, descriptions, keywords and `${KICAD10_3DMODEL_DIR}`
model paths, the references U1 to U3, field names, KIIDs and protobuf type
URLs. They hold no path, username or hostname.

## Regenerating them

With KiCad 10.0.8's Python, load the two stock footprints, place them as
above, flip U3 left to right and turn it, and save the board. Open it in the
PCB Editor with the API enabled and save it there. Then send `GetItems` for
footprints and `GetBoardEnabledLayers` for that board without changing
anything, and write each footprint item's `Any.value` and the layers
response's bytes unchanged.

## After refresh (#320)

`issue_320_u1_after_refresh.ipc.bin` was captured the same way, on the same
board, during #283's live check. The check first moved one vertex of U1's
pin-1 triangle with `edit_board_footprint_graphic`, then let
`update_footprints_from_library` restore it. KiCad reports the triangle with
four nodes in `issue_283_u1.ipc.bin`, the last repeating the first, as the
library lists them. In this capture it reports three: KiCad drops the repeat
when the outline comes over the API.
