# `issue_270_unknown_*`

KiCad 10.0.8's own IPC answers for a board holding one footprint with an item
the footprint transform does not carry. `pcb_copper_check`'s served test
`an_unsupported_item_refuses_every_move_as_copperpours_own` replays them for
[#240](https://github.com/Copperpour/Copperpour/issues/240)'s refusal kinds.
Until [#270](https://github.com/Copperpour/Copperpour/issues/270), U2's
reference point in `issue_240_*` played that part. Reference points now move
with their footprint, so this board gives the test an item that still doesn't.

| File | What |
|------|------|
| `issue_270_unknown.kicad_pcb` | The board, as KiCad 10.0.8's PCB Editor saved it |
| `issue_270_unknown_footprints.ipc.bin` | `GetItemsResponse` for `GetItems` `KOT_PCB_FOOTPRINT`: J1 |
| `issue_270_unknown_tracks.ipc.bin` | `GetItemsResponse` for `KOT_PCB_TRACE`, empty |
| `issue_270_unknown_vias.ipc.bin` | `GetItemsResponse` for `KOT_PCB_VIA`, empty |
| `issue_270_unknown_arcs.ipc.bin` | `GetItemsResponse` for `KOT_PCB_ARC`, empty |
| `issue_270_unknown_enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` for `GetBoardEnabledLayers`: F.Cu and B.Cu |
| `issue_270_unknown_presence.ipc.bin` | `PadstackPresenceResponse` for `CheckPadstackPresenceOnLayers`: J1's four pads on F.Cu and B.Cu |
| `issue_270_unknown_polygons_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` for `GetPadShapeAsPolygon`: J1's four pads, as placed, on F.Cu |
| `issue_270_unknown_polygons_B_Cu.ipc.bin` | The same on B.Cu |

The `.ipc.bin` files are the response messages' serialized bytes, unchanged.
The copper check runs before every live placement, so the test answers its
commands with these before they reach the transform; the board has no copper,
so the check is clear.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, macOS arm64 |
| Board | A new 2-layer board built with KiCad 10.0.8's `pcbnew` Python by a script: the stock `Connector_PinHeader_2.54mm:PinHeader_1x04_P2.54mm_Vertical` loaded with `FootprintLoad`, one barcode added to it (below), placed at (110, 95) mm and 0° with `SetPosition`, no nets, saved with `SaveBoard`. The board was then opened in KiCad 10.0.8's PCB Editor and saved there |
| Transport | Raw NNG with protobuf classes generated from KiCad 10.0.8's `api/proto`, read-only, over the running PCB Editor's IPC socket: the four `GetItems`, `GetBoardEnabledLayers`, `CheckPadstackPresenceOnLayers` and two `GetPadShapeAsPolygon`, each response's bytes written unchanged. Nothing was moved, no commit was opened |
| Captured | 2026-11-04, after the PCB Editor's save; the footprint, pad, graphic, text and barcode KIIDs in the answers are those in the saved file |

## What J1 shows

**An item the transform does not carry.** The stock 1×4 pin header at
(110, 95) mm, plus one QR barcode on F.SilkS, 3 × 3 mm, centred (3.5, 3.81) mm
from its anchor, text `COPPERPOUR-270`. No stock footprint has a barcode. KiCad
sends it as `kiapi.board.types.Barcode`, which the transform refuses to move or
turn, so every placement of J1 is refused. The stock header has no reference
point, textbox or rounded rectangle, so the barcode is the only refusal. Its
pads are through-hole, so they have copper on both layers.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock library's name, description, keywords and `${KICAD10_3DMODEL_DIR}` model
path, the barcode's made-up text, the reference J1, field names, KIIDs and
protobuf type URLs. They hold no path, username or hostname. The board file
holds the same and KiCad's default board settings.

## Regenerating them

With KiCad 10.0.8's Python, load the stock header, add the barcode while it is
still at the origin, place it, and save the board. Open it in the PCB Editor
with the API enabled, send each request in the table for that board without
changing anything, and write each response's bytes unchanged; then save the
board in the PCB Editor.
