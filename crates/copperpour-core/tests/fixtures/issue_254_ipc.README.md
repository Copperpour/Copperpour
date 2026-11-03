# `issue_254_*`

KiCad 10.0.8's own IPC answers for four footprints on a board built only
from KiCad's stock libraries, and those three library footprints.
`pcb_footprint_update`'s tests read them for
[#254](https://github.com/Copperpour/Copperpour/issues/254), where
`update_footprints_from_library` planned changes for footprints that match
their library.

| File | What |
|------|------|
| `issue_254_j1.ipc.bin` | J1, `Connector_PinHeader_1.27mm:PinHeader_2x10_P1.27mm_Vertical` at (120, 80) mm, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `issue_254_j2.ipc.bin` | J2, `Connector_Coaxial:BNC_TEConnectivity_1478035_Horizontal` at (93.85, 62.4) mm, likewise |
| `issue_254_h1.ipc.bin` | H1, `MountingHole:MountingHole_3.2mm_M3` at (54, 54) mm, likewise |
| `issue_254_h2.ipc.bin` | H2, the same mounting hole at (70, 54) mm, likewise |
| `issue_254_enabled_layers.ipc.bin` | KiCad's `BoardEnabledLayersResponse` for the board: 2 copper layers |
| `issue_254_pinheader_2x10_p1.27mm_vertical_kicad10.kicad_mod` | KiCad 10.0.8's stock `Connector_PinHeader_1.27mm.pretty/PinHeader_2x10_P1.27mm_Vertical.kicad_mod`, unchanged |
| `issue_254_bnc_teconnectivity_1478035_horizontal_kicad10.kicad_mod` | KiCad 10.0.8's stock `Connector_Coaxial.pretty/BNC_TEConnectivity_1478035_Horizontal.kicad_mod`, unchanged |
| `issue_254_mountinghole_3.2mm_m3_kicad10.kicad_mod` | KiCad 10.0.8's stock `MountingHole.pretty/MountingHole_3.2mm_M3.kicad_mod`, unchanged |

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, macOS arm64 |
| Board | A new 2-layer board made with KiCad 10.0.8's `pcbnew` Python: each footprint loaded with `FootprintLoad` from KiCad's stock library as shipped, its library id set with `SetFPID`, placed with `SetPosition` at 0°, its pads put on made-up nets (`SIG_1` to `SIG_20` on J1, `SIG_IN` and `GND` on J2), and saved with `SaveBoard`. H2 then had `FP_EXCLUDE_FROM_POS_FILES` and `FP_EXCLUDE_FROM_BOM` cleared. The board was then opened and saved by KiCad 10.0.8's PCB Editor |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` and `GetBoardEnabledLayers` over the running PCB Editor's IPC socket, read-only, as `update_footprints_from_library` reads them |
| Captured | 2026-11-02 |

Each `.ipc.bin` footprint is the `Any.value` of one item of KiCad's
`GetItemsResponse`, byte for byte, as the `c_0603_*` captures store them.

## What each footprint shows

- **J1: corner rounding on pads that don't use it.** KiCad reports its
  default `corner_rounding_ratio`, 0.25, on every pad-stack layer: J1's 19
  circular pads and its rectangular pin 1 alike. The library build leaves
  0. The ratio only means something for rounded and chamfered rectangles,
  so the comparison now zeroes it for every other shape, as it already did
  `chamfer_ratio`. Before that, J1 planned `pads`.
- **J2: library coordinates 1 nm low.** At (93.85, 62.4) mm, eleven
  coordinates of the library build's lines (x of 88.12, 91.025 and
  98.65 mm, y of 59.575 and 65.225 mm) and the x of two pads (88.825 mm)
  are products such as 88119999.99999999 nm, which a truncating `mm_to_nm`
  landed 1 nm below what KiCad holds. That is
  [#84](https://github.com/Copperpour/Copperpour/issues/84); with its
  rounding, J2 matches, and the comparison has no tolerance for it. Before
  that fix and this one, J2 planned `pads` and `graphics`.
- **H1: the library as shipped.** It matches to the nanometre once the pad
  rounding is ignored; before, it planned `pads` (its unplated hole is a
  circular pad). Its circles are the library's to the nanometre; a test
  moves one radius point 1 nm in code, drift KiCad's DRC accepts within its
  10 nm, and the refresh, comparing exactly, plans `graphics` for it.
- **H2: a real difference.** The same footprint with its position-file and
  BOM exclusions cleared on the board, as an older footprint copy or an
  edit would leave it. An update would set them again, so it plans
  `attributes`, and only that.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold
the stock libraries' ids, descriptions, keywords, `KiLib_Generator`
properties and `${KICAD10_3DMODEL_DIR}` model paths, the references J1, J2,
H1 and H2, the made-up nets above, field names, KIIDs and protobuf type
URLs. They hold no path, username or hostname.

## Regenerating them

Build the board as above with KiCad's Python, open it in KiCad's PCB Editor
with the API enabled, save it, and without changing anything send
`GetItems` for `KOT_PCB_FOOTPRINT` and `GetBoardEnabledLayers` for that
board; write each returned footprint's `Any.value` and the layers response
unchanged. The library files are copied from KiCad's stock footprint
directory as they are.
