# Issue 96 four-layer board and live KiCad IPC captures

`issue_96_layers.kicad_pcb` is a scratch board for the review of
[#258](https://github.com/Copperpour/Copperpour/pull/258), the copper check of
[#96](https://github.com/Copperpour/Copperpour/issues/96). It was made with KiCad 10.0.8's
own Python (`pcbnew`: a `BOARD` with four copper layers, footprints loaded from the stock
`Resistor_SMD` library, `PCB_TRACK`, `PCB_ARC`, and `PCB_VIA` with `VIATYPE_BLIND` and a
layer pair), then opened and saved by KiCad 10.0.8's PCB Editor. Its copper layers are
F.Cu, In1.Cu, In2.Cu and B.Cu. Its four footprints are `R_0603_1608Metric`, whose pads are
0.8 × 0.95 mm roundrects on F.Cu only, at ±0.825 mm:

- R1 (pad 1 `NET_A`, pad 2 `NET_B`) at (110, 95), 0°;
- R3 (pad 1 `NET_E`, pad 2 `NET_F`) at (110, 105), 90°, so pad 1 is at (110, 105.825) and
  pad 2 at (110, 104.175);
- two footprints both referenced R4 (pad 1 `NET_G`, pad 2 `NET_H`), at (120, 110) and
  (125, 110).

Its `NET_C` copper is one piece at each of six spots. Moving R1 to spots A to E puts its
pad 1 on that copper; spot F is for R3, moved and kept at 90°.

| Spot | Move | `NET_C` copper | KIID | Check |
|------|------|----------------|------|-------|
| A | R1 to (130.825, 90) | track (128, 90)-(134, 90), 0.25 mm, on B.Cu | `ce46f51e-60c1-427f-80e7-eb0029ca708b` | clear |
| B | R1 to (140.825, 90) | blind via In2.Cu-B.Cu at (140, 90), 0.6 mm, 0.3 mm drill | `17b4d6e4-982d-4d3d-aa69-1ad59d44a1c3` | clear |
| C | R1 to (146.825, 90) | blind via F.Cu-In1.Cu at (146, 90), 0.6 mm, 0.3 mm drill | `ab6f8834-1aac-4fb7-b536-bb2b9e1d69cf` | R1 pad 1 on F.Cu |
| D | R1 to (155.825, 90) | arc (153.5, 91)-(155, 90)-(156.5, 91), 0.25 mm, on F.Cu | `37d64abc-9509-4951-b192-03a717da9b67` | R1 pad 1 on F.Cu |
| E | R1 to (165.825, 90) | arc (163.5, 91)-(165, 90)-(166.5, 91), 0.25 mm, on B.Cu | `978cdb14-be70-4945-a4cb-950c7e12a88c` | clear |
| F | R3 to (175, 90) | track (174.8, 88.9)-(175.2, 88.9), 0.25 mm, on F.Cu | `2b77c1c4-22b4-48fe-bffd-db391fd65958` | R3 pad 2 on F.Cu |

At D the arc peaks at (155, 90), inside pad 1, while its chord along y = 91 passes 0.4 mm
below the pad, so only the curve reaches it; pad 2 clears the arc by about 31 µm. At F,
R3's pad 2 lands at (175, 89.175) on the track; at 0° both pads would clear it.

The `issue_96_layers_*.ipc.bin` files are KiCad's answers to its IPC API, byte for byte,
with that board open in the PCB Editor. Nothing was rebuilt or edited after capture.

| File | Message | Request |
|------|---------|---------|
| `issue_96_layers_footprints.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_FOOTPRINT` |
| `issue_96_layers_tracks.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_TRACE` |
| `issue_96_layers_vias.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_VIA` |
| `issue_96_layers_arcs.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_ARC` |
| `issue_96_layers_enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` | `GetBoardEnabledLayers` |
| `issue_96_layers_presence.ipc.bin` | `PadstackPresenceResponse` | `CheckPadstackPresenceOnLayers`: every pad and both vias, on F.Cu, In1.Cu, In2.Cu and B.Cu |
| `issue_96_layers_polygons_before_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | `GetPadShapeAsPolygon`: every pad on F.Cu, as placed |
| `issue_96_layers_polygons_before_In1_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same on In1.Cu |
| `issue_96_layers_polygons_before_In2_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same on In2.Cu |
| `issue_96_layers_polygons_before_B_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same on B.Cu |
| `issue_96_layers_polygons_A_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | every pad on F.Cu, R1 moved to spot A |
| `issue_96_layers_polygons_B_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same, R1 at spot B |
| `issue_96_layers_polygons_C_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same, R1 at spot C |
| `issue_96_layers_polygons_D_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same, R1 at spot D |
| `issue_96_layers_polygons_E_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same, R1 at spot E |
| `issue_96_layers_polygons_F_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | the same, R3 moved to spot F at 90° (R1 then at spot A) |

## What they show

- `CheckPadstackPresenceOnLayers` gives the blind vias' layers: the F.Cu-In1.Cu via is
  `PSP_PRESENT` on F.Cu and In1.Cu and `PSP_NOT_PRESENT` on In2.Cu and B.Cu; the
  In2.Cu-B.Cu via is the reverse. Every pad is `PSP_PRESENT` on F.Cu only.
- Each via's padstack lists its two layers but holds a single copper entry, on F.Cu,
  0.6 mm, the In2.Cu-B.Cu via's included.
- `GetPadShapeAsPolygon` returns an outline on all four layers for these F.Cu-only pads,
  and the four `before` files are byte-identical, as on the #96 board for B.Cu
  (`issue_96_ipc.README.md`). Presence, not an outline coming back, decides a pad's layers.
- The 0603 roundrect pads come back as 20 point nodes and no arc nodes.
- The spot outlines are KiCad's own pad shapes with the footprint moved there, so they
  check the transform the copper check moves the `before` outlines by, independently of
  it; F with R3 kept at its held 90°.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8 (macOS arm64), PCB Editor with the IPC API enabled |
| Client | kipy, sending each command and writing the response's serialized bytes |
| Captured | 2026-11-02 |

The spot captures were taken with the footprint moved by kipy (`update_items` of the
footprint with a new position and its held orientation, which carries its pads along)
inside a commit that was dropped afterwards. Nothing was saved, and the board file's
SHA-256 was the same after recording as before.

Inside an open commit, the first `update_items` of a footprint was not reflected by
`GetPadShapeAsPolygon`: its outlines came back at the held pose. `GetItems` kept answering
the held position throughout, so it could not confirm a move either. The first pass took
spot A (R1's first move) and spot F (R3's first move) at the held pose; B to E, R1's later
moves, were right. A and F were taken again in a second dropped commit by moving each
footprint 20 mm to the right of its spot first and then to the spot, keeping a capture
only when its pad outlines' centroids matched the pad positions worked out by hand (both
did on the first try). All six spot captures were checked against hand-computed pad
positions. That second commit moved R1 before R3, which is why R1 sits at spot A in the F
capture.

## Redaction

Nothing was removed. The messages hold the stock library's strings (its description,
`KiLib_Generator`, the `${KICAD10_3DMODEL_DIR}` model path), the field names, the seven
net names, protobuf type URLs and KIIDs. They hold no board path, username or hostname.

## Regenerating them

Build the board as above with KiCad's own Python, open it in the PCB Editor with the API
enabled, and save it there. Send each request in the table above for that board and write
the response message's serialized bytes unchanged. For a spot's outlines, begin a commit,
update the footprint at a throw-away position and then at the spot, its orientation as
held, ask for every pad's outline on F.Cu, check the moved pads' centroids against their
positions worked out by hand, and drop the commit when done.
