# Issue 96 repro board and live KiCad IPC captures

`issue_96.kicad_pcb` is the repro board of
[#96](https://github.com/Copperpour/Copperpour/issues/96), the `before.kicad_pcb` of
<https://gist.github.com/octave/29faa1899b7c8b59f2f4176a88b2aedb> (made by that gist's
`make_before.py` with KiCad's own Python), opened and re-saved by KiCad 10.0.8. It holds
R1 (`R_0603_1608Metric`, pad 1 `NET_A`, pad 2 `NET_B`) at (110, 95), R2 (pad 1 `NET_C`,
pad 2 `NET_D`) at (100, 110), and three kinds of `NET_C` copper, all on F.Cu:

- a track (99.175, 110)-(99.175, 106)-(115, 106), 0.25 mm, tied to R2 pad 1 and ending in a
  via at (115, 106);
- a lone via at (120, 100);
- a stub (125, 100)-(128, 100).

Both vias are 0.6 mm through vias with a 0.3 mm drill. The issue's three moves put R1 at
(114.2, 106), (120.825, 100) and (125.825, 100).

The `issue_96_*.ipc.bin` files are KiCad's answers to its IPC API, byte for byte, with that
board open in the PCB Editor. Nothing was rebuilt or edited after capture.

| File | Message | Request |
|------|---------|---------|
| `issue_96_footprints.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_FOOTPRINT` |
| `issue_96_tracks.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_TRACE` |
| `issue_96_vias.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_VIA` |
| `issue_96_arcs.ipc.bin` | `GetItemsResponse` (no items) | `GetItems` `KOT_PCB_ARC` |
| `issue_96_enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` | `GetBoardEnabledLayers` |
| `issue_96_presence.ipc.bin` | `PadstackPresenceResponse` | `CheckPadstackPresenceOnLayers`: R1's and R2's pads and both vias, on F.Cu and B.Cu |
| `issue_96_polygons_before_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | `GetPadShapeAsPolygon`: R1's and R2's pads on F.Cu, R1 as placed |
| `issue_96_polygons_before_B_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same on B.Cu |
| `issue_96_polygons_case2_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same on F.Cu, R1 at (120.825, 100), 0° |
| `issue_96_polygons_case2_rot90_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | R1 at (120.825, 100), 90° |
| `issue_96_polygons_case2_rot45_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | R1 at (120.825, 100), 45° |

## What they show

- `GetPadShapeAsPolygon` returns an outline on B.Cu for these F.Cu-only SMD pads, the same
  polygon as on F.Cu. Only `CheckPadstackPresenceOnLayers` says `PSP_NOT_PRESENT` there,
  so the copper check takes a pad's layers from presence, never from an outline coming back.
- The 0603 roundrect pads come back as 20 point nodes and no arc nodes.
- The `case2*` outlines are KiCad's own pad shapes at the planned poses, so they check the
  transform the copper check moves the `before` outlines by, independently of it.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8 (macOS arm64), PCB Editor with the IPC API enabled |
| Client | kipy, sending each command and writing the response's serialized bytes |
| Captured | 2026-11-02 |

The `case2*` captures were taken with R1 moved by kipy (`update_items` of the footprint,
which carries its pads along) inside one commit that was dropped afterwards, so the board
was left as it was and nothing was saved. Captures R1 was meant to hold at (114.2, 106)
are not included: they came back identical to the `before` ones, R1 not having moved yet
when they were taken.

## Redaction

Nothing was removed. The messages hold the stock library's strings (`Resistor_SMD`, its
description, `KiLib_Generator`, the `${KICAD10_3DMODEL_DIR}` model path), the field names,
the four net names, protobuf type URLs and KIIDs. They hold no board path, username or
hostname.

## Regenerating them

Open a copy of `issue_96.kicad_pcb` in the PCB Editor with the API enabled. Send each
request in the table above for that board and write the response message's serialized
bytes unchanged. For the `case2*` outlines, begin a commit, set R1's orientation to 0°,
its position to (120.825, 100) and its orientation to the pose's angle, update it, ask for
the outlines, and drop the commit when done.
