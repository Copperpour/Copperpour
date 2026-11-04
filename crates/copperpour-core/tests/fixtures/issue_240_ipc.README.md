# `issue_240_*`

KiCad 10.0.8's own IPC answers for a board holding one footprint for each
refusal the footprint transform made inside a live placement, and that
board. `pcb_copper_check`'s served tests replay them for
[#240](https://github.com/Copperpour/Copperpour/issues/240), where those
refusals, Copperpour's own, reached the caller as KiCad rejections. Since
[#270](https://github.com/Copperpour/Copperpour/issues/270), U2 is no longer
refused (below).

| File | What |
|------|------|
| `issue_240.kicad_pcb` | The board, as KiCad 10.0.8's PCB Editor saved it |
| `issue_240_footprints.ipc.bin` | `GetItemsResponse` for `GetItems` `KOT_PCB_FOOTPRINT`: Y1, U1 and U2 |
| `issue_240_tracks.ipc.bin` | `GetItemsResponse` for `KOT_PCB_TRACE`, empty |
| `issue_240_vias.ipc.bin` | `GetItemsResponse` for `KOT_PCB_VIA`, empty |
| `issue_240_arcs.ipc.bin` | `GetItemsResponse` for `KOT_PCB_ARC`, empty |
| `issue_240_enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` for `GetBoardEnabledLayers`: F.Cu and B.Cu |
| `issue_240_presence.ipc.bin` | `PadstackPresenceResponse` for `CheckPadstackPresenceOnLayers`: every pad of Y1, U1 and U2 on F.Cu and B.Cu |
| `issue_240_polygons_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` for `GetPadShapeAsPolygon`: every pad of Y1, U1 and U2, as placed, on F.Cu |
| `issue_240_polygons_B_Cu.ipc.bin` | The same on B.Cu |

The `.ipc.bin` files are the response messages' serialized bytes, unchanged.
The copper check runs before every live placement, so the tests answer its
commands with these before they reach the transform; the board has no
copper, so the check is clear.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, macOS arm64 |
| Board | A new 2-layer board built with KiCad 10.0.8's `pcbnew` Python by a script: three footprints loaded with `FootprintLoad` from KiCad's stock library as shipped, placed at 0° with `SetPosition`, no nets, and saved with `SaveBoard`. Before Y1 was placed, one rectangle was added to it (below). The board was then opened in KiCad 10.0.8's PCB Editor and saved there |
| Transport | kipy, read-only, over the running PCB Editor's IPC socket: the five `GetItems`, `GetBoardEnabledLayers`, `CheckPadstackPresenceOnLayers` and two `GetPadShapeAsPolygon`, each response's bytes written unchanged. Nothing was moved, no commit was opened |
| Captured | 2026-11-03, 9 s before the PCB Editor's save; the three footprint KIIDs in the answers are those in the saved file |

## What each footprint shows

- **Y1: a rounded rectangle.** The stock
  `Crystal:Crystal_SMD_3225-4Pin_3.2x2.5mm` at (110, 95) mm, plus one
  rectangle on F.Fab from (−1.6, −1.25) to (1.6, 1.25) mm about its anchor,
  with a 0.25 mm corner radius. No KiCad 10.0.8 stock footprint has a
  rounded-rectangle graphic, and the crystal's own courtyard rectangle is
  square-cornered: KiCad sends it with `corner_radius` 0, which rotates since
  [#224](https://github.com/Copperpour/Copperpour/issues/224). KiCad sends
  the added one with `corner_radius` 250000 nm, and the transform refuses to
  turn it by an angle that is not a multiple of 90°.
- **U1: textboxes.** The stock `Sensor_Pressure:Honeywell_ABPDAN` at
  (125, 95) mm, unchanged. Its two `fp_text_box` on Cmts.User come back as
  `kiapi.board.types.BoardTextBox`, which the transform refuses to turn off
  axis. Its pads are through-hole, so they have copper on both layers.
- **U2: a reference point.** The stock `Package_DIP:DIP-8_W7.62mm` at
  (140, 95) mm, unchanged. KiCad 10.0.8 sends its `(point …)` as
  `kiapi.board.types.ReferencePoint`, (3.81, 3.81) mm from the anchor. Until
  #270 the transform did not know that type and refused to move or rotate U2
  at all. It now carries the point, and `a_reference_point_follows_its_footprint`
  checks where each placement tool puts it. The unsupported-item refusal is
  tested on `issue_270_unknown_*`, a footprint holding a barcode.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock libraries' names, descriptions, keywords, `KiLib_Generator` properties
and `${KICAD10_3DMODEL_DIR}` model paths, the textboxes' text, the references
Y1, U1 and U2, field names, KIIDs and protobuf type URLs. They hold no path,
username or hostname. The board file holds the same and KiCad's default
board settings.

## Regenerating them

With KiCad 10.0.8's Python, load the three stock footprints as above, add
the rounded rectangle to the crystal while it is still at the origin, place
them, and save the board. Open it in the PCB Editor with the API enabled,
send each request in the table for that board without changing anything,
and write each response's bytes unchanged; then save the board in the PCB
Editor.
