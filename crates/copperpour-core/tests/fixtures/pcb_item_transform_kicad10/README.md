# Transform fixtures for KiCad 10

`pcb_item_transform_kicad10.kicad_pcb` is a board built by pcbnew 10.0.8's Python and saved
by pcbnew; the script is below. It builds every footprint itself, so nothing on the board
comes from KiCad's libraries. The board has two copper layers, F.Cu and B.Cu.

The `*.ipc.bin` files are KiCad's answers to its IPC API, byte for byte, with that board open
in the PCB Editor. `oracle.json` holds pcbnew's own results of moving and turning every item
of the board. The section on `issue_96.kicad_pcb` holds pcbnew's turns of R2 on that board.

## What the board holds

Positions are in millimetres in this table. Each UUID is the `(uuid "...")` of the item in
`pcb_item_transform_kicad10.kicad_pcb`.

| Item | UUID | What it is |
|---|---|---|
| `R1` | `1246cf7f-374e-404f-934e-571386b0b1a4` | footprint `fixture:R_two_pad`, Reference R1, Value R, at (95, 90) mm, 0°; centred fields |
| R1 pad 1 | `80f4997f-252d-48ff-839f-e235937eaede` | pad |
| R1 pad 2 | `1a99abc7-d0f9-48f3-8d3e-4cfab8e601fb` | pad |
| R1 segment | `42a8dc6d-1efe-4bd8-a8a6-abdea099ce62` | footprint graphic, F.SilkS |
| `R1_pad1_track` | `1fb1cfb9-ba2a-4a62-9725-57dee911eb77` | track, net `R1_PAD1`, F.Cu, 0.2 mm, from R1 pad 1 to `R1_pad1_via` |
| `R1_pad1_via` | `c7635528-f2bd-4d58-a0e5-8381c5e06ed6` | via, net `R1_PAD1`, 0.6 mm, 0.3 mm drill, 1.5 mm beyond R1 pad 1 |
| `U1` | `ab4b2e28-2bae-48d6-925a-da00f845a634` | footprint `fixture:U_odd_pads`, Reference U1, Value ODD_PADS, at (125, 90) mm, 0°; four pads at 23° and 113°, a silk segment, a fab rectangle; Reference left-justified, keep upright |
| U1 pad 1 | `0427f6bf-0153-4a2b-88a3-887adf67c227` | pad |
| U1 pad 2 | `1c0ab032-cb61-4e25-8a9a-864bed26cc60` | pad |
| U1 pad 3 | `56d6b73f-d3ef-451d-8a71-74f142c2846b` | pad |
| U1 pad 4 | `b8de2083-4007-4ea4-8736-ed78c753ea67` | pad |
| U1 segment | `2183dfc7-7bcd-4426-81e9-c1dd0db35e09` | footprint graphic, F.SilkS |
| U1 rectangle | `3faceec2-359d-4ecd-a9c8-b2289014431f` | footprint graphic, F.Fab |
| `U1_pad1_track` | `39d2a434-b469-4cbd-baef-4d738c430b87` | track, net `U1_PAD1`, F.Cu, 0.2 mm, from U1 pad 1 to `U1_pad1_via` |
| `U1_pad1_via` | `c028a965-c686-44a0-ad6c-735d0846dbaf` | via, net `U1_PAD1`, 1.5 mm left of U1 pad 1 |
| `R2` | `b01d0162-75af-4bc0-88ef-b24aeea43f19` | footprint `fixture:R_two_pad`, Reference R2, Value R, at (100, 125) mm, 23° |
| R2 pad 1 | `5ae49402-6eb2-4ed2-a053-d04810627375` | pad |
| R2 pad 2 | `525e18b6-5271-4de6-b443-49eca0fe7101` | pad |
| R2 segment | `0e988891-96c7-45f3-8d70-303a70ee2031` | footprint graphic, F.SilkS |
| `R2_pad1_track` | `6fd2a0cb-accf-4103-b913-32d83cf914f9` | track, net `R2_PAD1`, F.Cu, 0.2 mm, from R2 pad 1 to `R2_pad1_via` |
| `R2_pad1_via` | `7337bfc6-a9a6-4e33-a112-9493eca84890` | via, net `R2_PAD1`, 1.5 mm beyond R2 pad 1 |
| `arc_r2_90` | `db3f64f1-8d48-495f-9f16-c9b22153f941` | arc track, net `ARCS`, F.Cu, 0.25 mm; centre (135, 95) mm, radius 2 mm, from 0° over 90° |
| `arc_r4.5_135` | `e961c33a-c823-4d2a-84ca-1faa74f8f096` | arc track, net `ARCS`; centre (145, 97) mm, radius 4.5 mm, from 30° over 135° |
| `arc_r1.2_200` | `c8b9d2dd-6289-4df1-bab0-94a851fc7e92` | arc track, net `ARCS`; centre (125, 137) mm, radius 1.2 mm, from -10° over 200° |
| `text_centred` | `ed03cddb-89f0-4402-811c-c42daae39c81` | board text `CENTRED`, F.SilkS, centred, 0° |
| `text_left_30` | `8b5fb671-6078-40a5-b32d-6e30ec908755` | board text `LEFT 30`, F.SilkS, left-justified, 30° |
| `text_multiline` | `5dd5020d-0e3f-4eee-b653-b4a96ad4df0b` | board text `LINE 1` / `LINE 2` on two lines, F.SilkS |
| `text_mirrored` | `ac97b337-6da1-423a-833d-e225945853df` | board text `MIRRORED`, B.SilkS, mirrored, right-justified |
| `text_knockout` | `1cd531d6-e328-4fe1-8b21-683d77109479` | board text `KNOCKOUT`, F.SilkS, knockout |
| `text_locked` | `99c2fc57-728b-4c1e-8287-079820d10778` | board text `LOCKED`, F.SilkS, locked |
| `text_box` | `cfce4625-a94f-4851-815a-d2560a456841` | text box `TEXT BOX`, F.SilkS, (84, 130) to (94, 134) mm |
| `text_grouped` | `ee1d28ce-633e-48a3-9285-19d77b920bd3` | board text `GROUPED`, F.SilkS |
| `graphic_segment` | `a2885af4-60b0-428a-b775-dcd5d61d0db1` | graphic segment, F.SilkS |
| `graphic_rectangle` | `d003f2ea-312a-495a-8256-599856b55f28` | graphic rectangle, F.SilkS |
| `graphic_rounded_rectangle` | `dd96496c-36a3-4820-8819-83e89fc79a30` | graphic rectangle with 0.8 mm corners, F.SilkS |
| `graphic_arc` | `4f146f35-72f6-49a9-8303-20960c6e1c8d` | graphic arc, F.SilkS |
| `graphic_circle` | `f613284e-1758-4747-bed4-ce99a6ceae58` | graphic circle, F.SilkS, filled |
| `graphic_polygon` | `69015b8e-ce66-472b-ba64-c91ef1c7aa12` | graphic polygon of five corners, F.SilkS, filled |
| `graphic_bezier` | `aa1429b3-052e-409f-b745-5ff1f427ac56` | graphic Bézier curve, F.SilkS |
| `graphic_copper_segment` | `68957760-5eb5-4c79-8fe1-b75b147d9f19` | graphic segment, F.Cu, net `COPPER_GRAPHIC`, 0.3 mm |
| `graphic_locked_segment` | `d7a70902-c034-489b-8580-8b42c2d2ad40` | graphic segment, F.SilkS, locked |
| `graphic_grouped_segment` | `96f65347-cafb-4759-bf45-da9a15caf068` | graphic segment, F.SilkS |
| `edge_cuts_rectangle` | `fd4018b2-b1c6-46a8-a626-3e3ecfbd4e79` | Edge.Cuts rectangle (80, 80) to (162, 142) mm around everything |
| `zone_gnd` | `4227b8fa-ac49-4681-beaa-830ab93169f6` | copper zone `ZONE_GND`, F.Cu, net `GND`, filled, with a cutout; priority 3, clearance 0.4 mm, minimum thickness 0.3 mm, pads connected solid, border hatched full at 0.7 mm, islands kept, thermal gap 0.6 mm |
| `zone_signal_via` | `396ccf33-8665-426c-aba1-4d97b1a40401` | via, net `ZONE_SIGNAL`, inside `ZONE_GND` |
| `rule_area` | `ab6e0302-903f-4143-8626-12509431f43e` | rule area `RULE_AREA`, all copper layers; no tracks, vias, pads, footprints or copper pours |
| `zone_two_layers` | `3b6f0eec-80e9-4357-9923-b2ef6997b7e5` | copper zone `ZONE_TWO_LAYERS`, F.Cu and B.Cu, net `GND`, priority 1 |
| `zone_locked` | `450aa490-a376-42a6-9416-d6588e2e4a92` | copper zone `ZONE_LOCKED`, B.Cu, net `GND`, locked |
| `rule_area_locked` | `7a19b987-636d-46ec-926d-1773cd7c7808` | rule area `RULE_AREA_LOCKED`, all copper layers, the same restrictions, locked |
| `zone_grouped` | `f97b6dd2-b590-43d7-a952-733d21736975` | copper zone `ZONE_GROUPED`, B.Cu, net `GND` |
| `flat_track` | `a6c9d48a-4c63-4c0f-96ca-d2807e6fd2e1` | track, net `FLAT` |
| `flat_via` | `435ec50a-230b-4d28-a3b7-ef795459dbd2` | via, net `FLAT` |
| `group_flat_via_track` | `1b291518-96d0-44dc-bc77-3d4f5a424991` | group `FLAT_VIA_TRACK`: `flat_track`, `flat_via` |
| `group_flat_kinds` | `4b7fc793-81d8-4490-bb07-88d663dc99f0` | group `FLAT_KINDS`: `text_grouped`, `graphic_grouped_segment`, `arc_r1.2_200`, `zone_grouped` |
| `group_footprint_via` | `c3c198cd-6433-46ca-81a1-7caf6ef3d2e2` | group `FOOTPRINT_VIA`: R1, `R1_pad1_via` |
| `inner_track` | `9655b5a0-b4c9-4efd-905e-125fdd00ccbb` | track, net `NESTED` |
| `inner_via` | `9b98ffbe-27fe-4b2b-a7b0-9f8699c17720` | via, net `NESTED` |
| `group_inner` | `11369ef1-3a5b-4c95-af61-f91fcb30e7ad` | group `INNER`: `inner_track`, `inner_via` |
| `outer_track` | `d47eaaea-bb7a-44f1-b72d-e8a6b2e4ef52` | track, net `NESTED` |
| `group_outer` | `c726217b-52ef-47c6-bb8c-c7a0412989d0` | group `OUTER`: `outer_track`, group `INNER` |
| `group_mixed_inner` | `b54ad519-cb4c-4c37-a5f1-014e93b7f4b1` | group `MIXED_INNER`: R2, `R2_pad1_via` |
| `group_mixed_outer` | `9e1d1756-9122-4ee2-91e2-0f1a16a9300f` | group `MIXED_OUTER`: `R2_pad1_track`, group `MIXED_INNER` |
| `locked_track` | `fa2fb718-b36f-407e-9eae-59091f3dd1c2` | track, net `LOCKED_GROUP` |
| `locked_via` | `75d7c08f-94f1-4131-b32f-152135278086` | via, net `LOCKED_GROUP` |
| `group_locked` | `ce97d006-d9a9-4371-bc45-98f7008a8ccf` | group `LOCKED`, locked: `locked_track`, `locked_via` |
| `three_via` | `ab25ca20-e715-4243-9404-8a79df88742a` | via, net `THREE` |
| `three_track_a` | `5b4f3d5a-ee77-494c-9d34-fa7f340fdd46` | track, net `THREE` |
| `three_track_b` | `e10cf4bf-67e4-4c81-97c5-c815fe3d287a` | track, net `THREE` |
| `group_three` | `07dcbb38-a16c-4021-90aa-137b37777382` | group `THREE`: `three_via`, `three_track_a`, `three_track_b` |

Nets: `GND` on pad 2 of R1 and of R2 and on the copper zones; `R1_PAD1`, `R2_PAD1` and
`U1_PAD1` to `U1_PAD4` on the pads they name; `ARCS`, `FLAT`, `NESTED`, `LOCKED_GROUP`,
`THREE`, `ZONE_SIGNAL` and `COPPER_GRAPHIC` on the items above. Tracks are 0.2 mm wide, arcs
0.25 mm. Vias are through vias of 0.6 mm with a 0.3 mm drill. Every copper zone is filled.
No item lies on (110, 105) mm, the centre of the turns.

The board holds no teardrop zone. pcbnew 10.0.8's Python enables teardrops on a via
(`SetTeardropsEnabled`) but has no call that builds them, and `ZONE_FILLER` builds none.

## The captures

| File | Message | Request |
|------|---------|---------|
| `footprints.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_FOOTPRINT` |
| `tracks.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_TRACE` |
| `vias.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_VIA` |
| `arcs.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_ARC` |
| `texts.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_TEXT` |
| `text_boxes.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_TEXTBOX` |
| `shapes.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_SHAPE` |
| `zones.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_ZONE` |
| `groups.ipc.bin` | `GetItemsResponse` | `GetItems` `KOT_PCB_GROUP` |
| `enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` | `GetBoardEnabledLayers` |
| `presence.ipc.bin` | `PadstackPresenceResponse` | `CheckPadstackPresenceOnLayers`: the 8 pads and the 8 vias, on F.Cu and B.Cu |
| `pad_polygons_F_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | `GetPadShapeAsPolygon`: the 8 pads, on F.Cu |
| `pad_polygons_B_Cu.ipc.bin` | `PadShapeAsPolygonResponse` | `GetPadShapeAsPolygon`: the 8 pads, on B.Cu |
| `footprint_boxes.ipc.bin` | `GetBoundingBoxResponse` | `GetBoundingBox`: the 3 footprints, `BBM_ITEM_ONLY` |

Each `GetItems` asks for one type. The other requests name their items in UUID order and
their layers F.Cu then B.Cu.

Every KIID in the captures is a `(uuid "...")` of the board but one. The `parent` of each
item outside a footprint is `821d50e7-2905-43d5-a664-b11df3c9e3ac`, the board's own KIID,
which KiCad gave the board when it opened it and which the board file does not hold.

The captures were taken with KiCad 10.0.8 on Linux, the PCB Editor's IPC API enabled. Nothing
was rebuilt, edited or removed after capture. The messages hold the script's strings (the
library names `fixture:R_two_pad` and `fixture:U_odd_pads`, references, values, field names,
net names, text contents, zone and group names), protobuf type URLs and KIIDs. They hold no
board path, username or hostname.

## `oracle.json`

It holds `kicad`, the version pcbnew gives; `centre_nm`, the centre of the turns,
(110000000, 105000000); `offset_nm`, the offset, (3300000, -1700000); `cases`; and
`variants`. Each case
loads a fresh copy of the board and moves or turns every item in memory with pcbnew's own
call, `Move` or `Rotate`, a footprint by `FOOTPRINT::Move` or `FOOTPRINT::Rotate`, which
carries its pads, fields and graphics. A positive angle is the sense of
`EDA_ANGLE(deg, DEGREES_T)` passed to `Rotate`.

| Case | What pcbnew did |
|---|---|
| `held` | nothing: the board as loaded |
| `offset` | `Move` by `offset_nm` |
| `turn_90`, `turn_45`, `turn_33`, `turn_-135`, `turn_180`, `turn_-90` | `Rotate` by that many degrees about `centre_nm` |

`variants` holds one entry, `u1_reference_not_keep_upright`: the cases `turn_90`, `turn_180`,
`turn_-135`, `turn_45` and `turn_-90` for U1 alone, each on a fresh load with keep-upright
cleared on U1's Reference (`SetKeepUpright(False)`) before `FOOTPRINT::Rotate`. Each holds
U1's record under its UUID, as `cases` does.

Each case maps an item's UUID to what that kind of item has. Lengths are pcbnew's integer
nanometres and points `[x, y]`. Angles are degrees as pcbnew holds them: a footprint's and a
pad's `GetOrientationDegrees()`, a text's `GetTextAngle().AsDegrees()`. Justification is
pcbnew's `GR_TEXT_H_ALIGN_*` and `GR_TEXT_V_ALIGN_*` name, lower case after the last
underscore; a shape is pcbnew's `SHAPE_T_asString()`.

| `kind` | Fields |
|---|---|
| `footprint` | `at`, `orientation`, `pads` (by UUID: `at`, `angle`), `fields` (by name: `at`, `angle`, `h`, `v`), `graphics` (by UUID: `shape`, `points`) |
| `track` | `start`, `end` |
| `via` | `at` |
| `arc` | `start`, `mid`, `end` |
| `text` | `at`, `angle`, `h`, `v`, `mirrored` |
| `text_box` | `shape`, `points` (its four corners), `angle` |
| `graphic` | `shape`, `points` |
| `zone` | `outline`, `holes`; the fill is not given |

A graphic's `points` are, by shape: `S_SEGMENT` start, end; `S_RECT` start, end; `S_ARC`
start, mid, end; `S_CIRCLE` centre, a point on the circle; `S_POLYGON` its outline; `S_CURVE`
start, the two control points, end. The text box and `graphic_rounded_rectangle` appear in
`held`, `offset`, `turn_90`, `turn_180` and `turn_-90` only, the cases where they stay
rectangles.

Two facts about pcbnew bear on comparing a saved board with `oracle.json`. A pad of a turned
footprint can read 1 nm off after a save and a load: at 45°, R2 pad 1 reads
(116762017, 126978131) and pad 2 (117380119, 125448275), where `turn_45` holds
(116762017, 126978130) and (117380119, 125448276). A turned rectangle's start and end are
read back with their y values swapped: at 90°, `graphic_rounded_rectangle` reads start
(99000000, 57000000) and end (103000000, 63000000), where `turn_90` holds
(99000000, 63000000) and (103000000, 57000000).

## pcbnew's turn of R2, the NET_C tracks and the vias of `issue_96.kicad_pcb`

pcbnew 10.0.8's own results for turning R2, the three `NET_C` tracks and both vias of
`issue_96.kicad_pcb`, in the directory above this one, about a point. They are the expected
values of the transform tests. No new board belongs to this section: each case loads a fresh
copy of `issue_96.kicad_pcb`, applies the
turn in memory and prints the result. Lengths are pcbnew's internal integer nanometres,
angles are degrees as pcbnew reports them. A positive angle is the sense of
`EDA_ANGLE(deg, DEGREES_T)` passed to `Rotate`.

The footprint is turned two ways, each on its own fresh load:

1. `fp.Rotate(centre, angle)`. The tables give this way.
2. `fp.SetPosition(anchor)` with the anchor of way 1, then
   `fp.SetOrientation(old orientation + angle)`. pcbnew's Python exposes no point-rotation
   function, so the anchor is taken from way 1.

Each track is turned with `track.Rotate(centre, angle)` and each via with
`via.Rotate(centre, angle)`. The board holds no arc. Every UUID below is the `(uuid "...")`
of the matching footprint, pad, segment or via in `issue_96.kicad_pcb`.

### The board before any turn

| Item | UUID | Held pose (nm) |
|---|---|---|
| R1 | `10037c7b-af43-4fde-a3cd-11a43a5c9055` | anchor (110000000, 95000000), 0.0 |
| R1 pad 1, `NET_A` | `868530b1-e4d1-41a1-9f30-416b9442cd39` | (109175000, 95000000), 0.0 |
| R1 pad 2, `NET_B` | `82783ffe-437f-4e7a-a719-240bb65baadd` | (110825000, 95000000), 0.0 |
| R2 | `a23adadf-1dd1-491b-9f4b-997b6570350a` | anchor (100000000, 110000000), 0.0 |
| R2 pad 1, `NET_C` | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (99175000, 110000000), 0.0 |
| R2 pad 2, `NET_D` | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (100825000, 110000000), 0.0 |
| R2 Reference | | (100000000, 108570000), 0.0, keep upright, justify 0 / 0 |
| R2 Value | | (100000000, 111430000), 0.0, keep upright, justify 0 / 0 |
| track a, `NET_C`, F.Cu, 250000 wide | `0335fe60-230b-4695-95ac-698836799eeb` | (99175000, 110000000) to (99175000, 106000000) |
| track b, `NET_C`, F.Cu, 250000 wide | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (99175000, 106000000) to (115000000, 106000000) |
| track c (stub), `NET_C`, F.Cu, 250000 wide | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (125000000, 100000000) to (128000000, 100000000) |
| via 1, `NET_C` | `df431fc3-7c5e-4772-b3f8-33744514f439` | (115000000, 106000000) |
| via 2, `NET_C` | `424e71ba-90b9-4287-835a-410e3510db12` | (120000000, 100000000) |

Both Reference and Value of R2 report keep-upright set and justification 0 horizontal and 0
vertical, before and after every turn.

### Case A: 90 about (105, 100) mm = (105000000, 100000000) nm

| Item | UUID | After (nm, degrees) |
|---|---|---|
| R2 anchor | `a23adadf-1dd1-491b-9f4b-997b6570350a` | (115000000, 105000000), 90.0 |
| R2 pad 1 | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (115000000, 105825000), 90.0, `NET_C` |
| R2 pad 2 | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (115000000, 104175000), 90.0, `NET_D` |
| R2 Reference | | (113570000, 105000000), 90.0 |
| R2 Value | | (116430000, 105000000), 90.0 |
| track a | `0335fe60-230b-4695-95ac-698836799eeb` | (115000000, 105825000) to (111000000, 105825000) |
| track b | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (111000000, 105825000) to (111000000, 90000000) |
| track c | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (105000000, 80000000) to (105000000, 77000000) |
| via 1 | `df431fc3-7c5e-4772-b3f8-33744514f439` | (111000000, 90000000) |
| via 2 | `424e71ba-90b9-4287-835a-410e3510db12` | (105000000, 85000000) |

Way 2 gives the same values everywhere.

### Case B: 45 about (105, 100) mm = (105000000, 100000000) nm

| Item | UUID | After (nm, degrees) |
|---|---|---|
| R2 anchor | `a23adadf-1dd1-491b-9f4b-997b6570350a` | (108535534, 110606602), 45.0 |
| R2 pad 1 | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (107952171, 111189965), 45.0, `NET_C` |
| R2 pad 2 | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (109118897, 110023239), 45.0, `NET_D` |
| R2 Reference | | (107524371, 109595439), 45.0 |
| R2 Value | | (109546697, 111617765), 45.0 |
| track a | `0335fe60-230b-4695-95ac-698836799eeb` | (107952171, 111189965) to (105123744, 108361538) |
| track b | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (105123744, 108361538) to (116313708, 97171573) |
| track c | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (119142136, 85857864) to (121263456, 83736544) |
| via 1 | `df431fc3-7c5e-4772-b3f8-33744514f439` | (116313708, 97171573) |
| via 2 | `424e71ba-90b9-4287-835a-410e3510db12` | (115606602, 89393398) |

Way 2 gives the same values everywhere.

### Case C: 33 about (105, 100) mm = (105000000, 100000000) nm

| Item | UUID | After (nm, degrees) |
|---|---|---|
| R2 anchor | `a23adadf-1dd1-491b-9f4b-997b6570350a` | (106253038, 111109901), 33.0 |
| R2 pad 1 | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (105561135, 111559228), 33.0, `NET_C` |
| R2 pad 2 | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (106944941, 110660574), 33.0, `NET_D` |
| R2 Reference | | (105474204, 109910602), 33.0 |
| R2 Value | | (107031872, 112309200), 33.0 |
| track a | `0335fe60-230b-4695-95ac-698836799eeb` | (105561134, 111559228) to (103382578, 108204546) |
| track b | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (103382578, 108204546) to (116654540, 99585633) |
| track c | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (121773411, 89107219) to (124289423, 87473302) |
| via 1 | `df431fc3-7c5e-4772-b3f8-33744514f439` | (116654540, 99585633) |
| via 2 | `424e71ba-90b9-4287-835a-410e3510db12` | (117580059, 91830414) |

Way 2 gives the same values everywhere. The start of track a is 1 nm from pad 1's
position: the two are turned by different calls and each rounds its own result.

### Case D: -135 about (98.7654, 111.2345) mm = (98765400, 111234500) nm

| Item | UUID | After (nm, degrees) |
|---|---|---|
| R2 anchor | `a23adadf-1dd1-491b-9f4b-997b6570350a` | (98765329, 112980417), -135.0 |
| R2 pad 1 | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (99348692, 112397054), 225.0, `NET_C` |
| R2 pad 2 | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (98181966, 113563780), 225.0, `NET_D` |
| R2 Reference | | (99776492, 113991580), 45.0 |
| R2 Value | | (97754166, 111969254), 45.0 |
| track a | `0335fe60-230b-4695-95ac-698836799eeb` | (99348692, 112397054) to (102177120, 115225481) |
| track b | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (102177120, 115225481) to (90987155, 126415446) |
| track c | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (88158728, 137729155) to (86037407, 139850475) |
| via 1 | `df431fc3-7c5e-4772-b3f8-33744514f439` | (90987155, 126415446) |
| via 2 | `424e71ba-90b9-4287-835a-410e3510db12` | (91694261, 134193621) |

Way 2 differs only in the text angles of Reference and Value, 225.0 where way 1 gives 45.0.
Every position, the anchor's -135.0 and the pads' 225.0 are the same.

### Case E: 180 about (100, 110) mm = (100000000, 110000000) nm

| Item | UUID | After (nm, degrees) |
|---|---|---|
| R2 anchor | `a23adadf-1dd1-491b-9f4b-997b6570350a` | (100000000, 110000000), 180.0 |
| R2 pad 1 | `4933a322-03a2-4f6c-872a-de7458bd94b7` | (100825000, 110000000), 180.0, `NET_C` |
| R2 pad 2 | `bcd8d1a0-eaaf-47d8-8263-214f2aa07c59` | (99175000, 110000000), 180.0, `NET_D` |
| R2 Reference | | (100000000, 111430000), 0.0 |
| R2 Value | | (100000000, 108570000), 0.0 |
| track a | `0335fe60-230b-4695-95ac-698836799eeb` | (100825000, 110000000) to (100825000, 114000000) |
| track b | `e86c73df-a536-483a-8c1e-cf0b7f96cf64` | (100825000, 114000000) to (85000000, 114000000) |
| track c | `3236141c-2c15-4b0f-9c9f-b39f826067c1` | (75000000, 120000000) to (72000000, 120000000) |
| via 1 | `df431fc3-7c5e-4772-b3f8-33744514f439` | (85000000, 114000000) |
| via 2 | `424e71ba-90b9-4287-835a-410e3510db12` | (80000000, 120000000) |

Way 2 differs only in the text angles of Reference and Value, 180.0 where way 1 gives 0.0.
Every position, and the anchor's and pads' 180.0, are the same.

### Rerunning

With KiCad 10's `pcbnew` Python on the path, from this directory:

```
python3 r2_oracle.py ../issue_96.kicad_pcb
```

It prints the held poses, then each case. The script, `r2_oracle.py`:

```python
import sys
import pcbnew

SRC = sys.argv[1]
CASES = [("A", 90, 105, 100), ("B", 45, 105, 100), ("C", 33, 105, 100),
         ("D", -135, 98.7654, 111.2345), ("E", 180, 100, 110)]


def load():
    return pcbnew.LoadBoard(SRC)


def p(v):
    return "(%d, %d)" % (v.x, v.y)


def fp_dump(fp):
    print("  anchor %s orientation %s" % (p(fp.GetPosition()), fp.GetOrientationDegrees()))
    for pad in sorted(fp.Pads(), key=lambda q: q.GetNumber()):
        print("  pad %s uuid %s pos %s orient %s net %s" % (
            pad.GetNumber(), pad.m_Uuid.AsString(), p(pad.GetPosition()),
            pad.GetOrientationDegrees(), pad.GetNetname()))
    for name, t in (("Reference", fp.Reference()), ("Value", fp.Value())):
        print("  %s pos %s angle %s keep_upright %s hjust %s vjust %s" % (
            name, p(t.GetPosition()), t.GetTextAngleDegrees(), t.IsKeepUpright(),
            t.GetHorizJustify(), t.GetVertJustify()))


def items(b):
    tr, vi = [], []
    for t in b.GetTracks():
        cls = t.GetClass()
        if cls == "PCB_VIA":
            vi.append(t)
        elif t.GetNetname() == "NET_C":
            if cls != "PCB_TRACK":
                print("NOT A STRAIGHT SEGMENT:", cls, t.m_Uuid.AsString())
            tr.append(t)
    key = lambda t: t.m_Uuid.AsString()
    return sorted(tr, key=key), sorted(vi, key=key)


print("pcbnew", pcbnew.Version())
print("== held poses")
b = load()
for ref in ("R1", "R2"):
    fp = b.FindFootprintByReference(ref)
    print(ref, "uuid", fp.m_Uuid.AsString())
    fp_dump(fp)

for name, deg, ox, oy in CASES:
    c = pcbnew.VECTOR2I(pcbnew.FromMM(ox), pcbnew.FromMM(oy))
    ang = pcbnew.EDA_ANGLE(deg, pcbnew.DEGREES_T)
    print("== case %s: %s deg about (%s, %s) mm = %s nm" % (name, deg, ox, oy, p(c)))

    b1 = load()
    fp = b1.FindFootprintByReference("R2")
    print(" way 1: fp.Rotate(centre, angle)  [R2 uuid %s]" % fp.m_Uuid.AsString())
    fp.Rotate(c, ang)
    fp_dump(fp)
    way1_anchor = pcbnew.VECTOR2I(fp.GetPosition())

    b2 = load()
    fp2 = b2.FindFootprintByReference("R2")
    # pcbnew exposes no point-rotation function in Python; anchor taken from way 1
    old = fp2.GetOrientation()
    fp2.SetPosition(way1_anchor)
    fp2.SetOrientation(old + ang)
    print(" way 2: SetPosition(anchor from way 1), SetOrientation(old + angle)")
    fp_dump(fp2)

    b3 = load()
    tr, vi = items(b3)
    for t in tr:
        s0, e0 = pcbnew.VECTOR2I(t.GetStart()), pcbnew.VECTOR2I(t.GetEnd())
        t.Rotate(c, ang)
        print(" track %s start %s -> %s end %s -> %s net %s layer %s width %d" % (
            t.m_Uuid.AsString(), p(s0), p(t.GetStart()), p(e0), p(t.GetEnd()),
            t.GetNetname(), b3.GetLayerName(t.GetLayer()), t.GetWidth()))
    for v in vi:
        p0 = pcbnew.VECTOR2I(v.GetPosition())
        v.Rotate(c, ang)
        print(" via %s pos %s -> %s net %s" % (
            v.m_Uuid.AsString(), p(p0), p(v.GetPosition()), v.GetNetname()))
```

## Regenerating

### The board

With KiCad 10's `pcbnew` Python on the path, `python3 make_board.py OUT` writes
`OUT/pcb_item_transform_kicad10.kicad_pcb` and, beside it, each item's name and UUID. A rerun
gives new UUIDs. `make_board.py`:

```python
"""Build pcb_item_transform_kicad10.kicad_pcb with pcbnew's Python.

usage: python3 make_board.py OUT_DIR

Writes OUT_DIR/pcb_item_transform_kicad10.kicad_pcb, and beside it
pcb_item_transform_kicad10.ids.json, each item's name and UUID.
"""
import json
import math
import os
import sys

import pcbnew as P

MM = P.FromMM
BOARD = "pcb_item_transform_kicad10"


def v(x, y):
    return P.VECTOR2I(MM(x), MM(y))


def build():
    board = P.BOARD()
    ids = {}

    def name(label, item):
        ids[label] = item.m_Uuid.AsString()
        return item

    def net(netname):
        found = board.FindNet(netname)
        if found is None:
            found = P.NETINFO_ITEM(board, netname)
            board.Add(found)
        return found

    def track(label, a, b, netname, layer=P.F_Cu):
        t = P.PCB_TRACK(board)
        t.SetStart(a)
        t.SetEnd(b)
        t.SetWidth(MM(0.2))
        t.SetLayer(layer)
        t.SetNet(net(netname))
        board.Add(t)
        return name(label, t)

    def via(label, at, netname):
        w = P.PCB_VIA(board)
        w.SetPosition(at)
        w.SetViaType(P.VIATYPE_THROUGH)
        w.SetWidth(P.F_Cu, MM(0.6))
        w.SetDrill(MM(0.3))
        w.SetNet(net(netname))
        board.Add(w)
        return name(label, w)

    def arc(label, cx, cy, r, start, sweep, netname):
        def at(deg):
            t = math.radians(deg)
            return P.VECTOR2I(round(MM(cx) + MM(r) * math.cos(t)), round(MM(cy) + MM(r) * math.sin(t)))
        a = P.PCB_ARC(board)
        a.SetStart(at(start))
        a.SetMid(at(start + sweep / 2))
        a.SetEnd(at(start + sweep))
        a.SetWidth(MM(0.25))
        a.SetLayer(P.F_Cu)
        a.SetNet(net(netname))
        board.Add(a)
        return name(label, a)

    def two_pad(label, ref, x, y, degrees, net1):
        fp = P.FOOTPRINT(board)
        fp.SetReference(ref)
        fp.SetValue("R")
        fp.SetFPID(P.LIB_ID("fixture", "R_two_pad"))
        fp.SetLayer(P.F_Cu)
        board.Add(fp)
        fp.SetPosition(v(0, 0))
        for number, px, netname in (("1", -0.825, net1), ("2", 0.825, "GND")):
            p = P.PAD(fp)
            p.SetNumber(number)
            p.SetShape(P.F_Cu, P.PAD_SHAPE_RECTANGLE)
            p.SetAttribute(P.PAD_ATTRIB_SMD)
            p.SetLayerSet(P.PAD.SMDMask())
            p.SetSize(P.F_Cu, v(0.8, 0.95))
            p.SetPosition(v(px, 0))
            p.SetNet(net(netname))
            fp.Add(p)
        s = P.PCB_SHAPE(fp, P.SHAPE_T_SEGMENT)
        s.SetLayer(P.F_SilkS)
        s.SetWidth(MM(0.12))
        s.SetStart(v(-0.25, -0.6))
        s.SetEnd(v(0.25, -0.6))
        fp.Add(s)
        fp.Reference().SetPosition(v(0, -1.5))
        fp.Value().SetPosition(v(0, 1.5))
        fp.SetPosition(v(x, y))
        fp.SetOrientationDegrees(degrees)
        return name(label, fp)

    def pad_out(fp, label, netname):
        """A track from pad 1 to a via 1.5 mm further out, on the line from
        pad 2 through pad 1."""
        pads = {p.GetNumber(): p.GetPosition() for p in fp.Pads()}
        a, b = pads["1"], pads["2"]
        length = math.hypot(a.x - b.x, a.y - b.y)
        out = P.VECTOR2I(a.x + round((a.x - b.x) * MM(1.5) / length),
                         a.y + round((a.y - b.y) * MM(1.5) / length))
        track(label + "_track", a, out, netname)
        return via(label + "_via", out, netname)

    def group(label, groupname, members):
        g = P.PCB_GROUP(board)
        g.SetName(groupname)
        board.Add(g)
        for m in members:
            g.AddItem(m)
        return name(label, g)

    # Footprints.
    r1 = two_pad("R1", "R1", 95, 90, 0, "R1_PAD1")
    r1_via = pad_out(r1, "R1_pad1", "R1_PAD1")

    u1 = P.FOOTPRINT(board)
    u1.SetReference("U1")
    u1.SetValue("ODD_PADS")
    u1.SetFPID(P.LIB_ID("fixture", "U_odd_pads"))
    u1.SetLayer(P.F_Cu)
    board.Add(u1)
    u1.SetPosition(v(0, 0))
    for i, (px, py, degrees) in enumerate(((-1.5, -1, 23), (1.5, -1, 23), (1.5, 1, 113), (-1.5, 1, 113))):
        p = P.PAD(u1)
        p.SetNumber(str(i + 1))
        p.SetShape(P.F_Cu, P.PAD_SHAPE_RECTANGLE)
        p.SetAttribute(P.PAD_ATTRIB_SMD)
        p.SetLayerSet(P.PAD.SMDMask())
        p.SetSize(P.F_Cu, v(0.6, 0.9))
        p.SetPosition(v(px, py))
        p.SetOrientationDegrees(degrees)
        p.SetNet(net("U1_PAD%d" % (i + 1)))
        u1.Add(p)
    s = P.PCB_SHAPE(u1, P.SHAPE_T_SEGMENT)
    s.SetLayer(P.F_SilkS)
    s.SetWidth(MM(0.12))
    s.SetStart(v(-2, -1.9))
    s.SetEnd(v(2, -1.9))
    u1.Add(s)
    s = P.PCB_SHAPE(u1, P.SHAPE_T_RECTANGLE)
    s.SetLayer(P.F_Fab)
    s.SetWidth(MM(0.1))
    s.SetStart(v(-2.3, -1.6))
    s.SetEnd(v(2.3, 1.6))
    u1.Add(s)
    ref = u1.Reference()
    ref.SetPosition(v(2.5, -3))
    ref.SetHorizJustify(P.GR_TEXT_H_ALIGN_LEFT)
    ref.SetKeepUpright(True)
    u1.Value().SetPosition(v(0, 3))
    u1.SetPosition(v(125, 90))
    name("U1", u1)
    pads = {p.GetNumber(): p.GetPosition() for p in u1.Pads()}
    track("U1_pad1_track", pads["1"], P.VECTOR2I(pads["1"].x - MM(1.5), pads["1"].y), "U1_PAD1")
    via("U1_pad1_via", P.VECTOR2I(pads["1"].x - MM(1.5), pads["1"].y), "U1_PAD1")

    r2 = two_pad("R2", "R2", 100, 125, 23, "R2_PAD1")
    r2_via = pad_out(r2, "R2_pad1", "R2_PAD1")

    # Arc tracks, three radii and sweeps.
    arc("arc_r2_90", 135, 95, 2.0, 0, 90, "ARCS")
    arc("arc_r4.5_135", 145, 97, 4.5, 30, 135, "ARCS")
    arc_grouped = arc("arc_r1.2_200", 125, 137, 1.2, -10, 200, "ARCS")

    # Board texts and a text box.
    def text(label, s, x, y, degrees=0, layer=P.F_SilkS):
        t = P.PCB_TEXT(board)
        t.SetText(s)
        t.SetLayer(layer)
        t.SetPosition(v(x, y))
        t.SetTextAngleDegrees(degrees)
        t.SetTextSize(v(1.2, 1.0))
        t.SetTextThickness(MM(0.15))
        board.Add(t)
        return name(label, t)

    text("text_centred", "CENTRED", 90, 100)
    t = text("text_left_30", "LEFT 30", 87, 104, 30)
    t.SetHorizJustify(P.GR_TEXT_H_ALIGN_LEFT)
    text("text_multiline", "LINE 1\nLINE 2", 90, 108)
    t = text("text_mirrored", "MIRRORED", 90, 113, 0, P.B_SilkS)
    t.SetMirrored(True)
    t.SetHorizJustify(P.GR_TEXT_H_ALIGN_RIGHT)
    t = text("text_knockout", "KNOCKOUT", 90, 118)
    t.SetIsKnockout(True)
    t = text("text_locked", "LOCKED", 90, 122)
    t.SetLocked(True)
    tb = P.PCB_TEXTBOX(board)
    tb.SetLayer(P.F_SilkS)
    tb.SetText("TEXT BOX")
    tb.SetStart(v(84, 130))
    tb.SetEnd(v(94, 134))
    tb.SetTextSize(v(1, 1))
    board.Add(tb)
    name("text_box", tb)
    text_grouped = text("text_grouped", "GROUPED", 121, 133)

    # Board graphics.
    def shape(label, kind, layer=P.F_SilkS, width=0.15):
        s = P.PCB_SHAPE(board, kind)
        s.SetLayer(layer)
        s.SetWidth(MM(width))
        return s

    def add(label, s):
        board.Add(s)
        return name(label, s)

    s = shape("segment", P.SHAPE_T_SEGMENT)
    s.SetStart(v(152, 85))
    s.SetEnd(v(156.5, 86.25))
    add("graphic_segment", s)
    s = shape("rectangle", P.SHAPE_T_RECTANGLE)
    s.SetStart(v(152, 89))
    s.SetEnd(v(158, 92))
    add("graphic_rectangle", s)
    s = shape("rounded", P.SHAPE_T_RECTANGLE)
    s.SetStart(v(152, 94))
    s.SetEnd(v(158, 98))
    s.SetCornerRadius(MM(0.8))
    add("graphic_rounded_rectangle", s)
    s = shape("arc", P.SHAPE_T_ARC)
    s.SetArcGeometry(v(152, 101), v(154, 100), v(156, 101))
    add("graphic_arc", s)
    s = shape("circle", P.SHAPE_T_CIRCLE)
    s.SetCenter(v(154, 105))
    s.SetEnd(v(155.5, 105))
    s.SetFilled(True)
    add("graphic_circle", s)
    s = shape("polygon", P.SHAPE_T_POLY)
    poly = P.SHAPE_POLY_SET()
    poly.NewOutline()
    for x, y in ((152, 108), (158, 108), (158, 112), (155, 113), (152, 112)):
        poly.Append(MM(x), MM(y))
    s.SetPolyShape(poly)
    s.SetFilled(True)
    add("graphic_polygon", s)
    s = shape("bezier", P.SHAPE_T_BEZIER)
    s.SetStart(v(152, 117))
    s.SetBezierC1(v(153, 114))
    s.SetBezierC2(v(156, 120))
    s.SetEnd(v(157, 117))
    s.RebuildBezierToSegmentsPointsList(MM(0.005))
    add("graphic_bezier", s)
    s = shape("copper", P.SHAPE_T_SEGMENT, P.F_Cu, 0.3)
    s.SetStart(v(152, 122))
    s.SetEnd(v(157, 123))
    s.SetNet(net("COPPER_GRAPHIC"))
    add("graphic_copper_segment", s)
    s = shape("locked", P.SHAPE_T_SEGMENT)
    s.SetStart(v(152, 126))
    s.SetEnd(v(157, 126))
    s.SetLocked(True)
    add("graphic_locked_segment", s)
    s = shape("grouped", P.SHAPE_T_SEGMENT)
    s.SetStart(v(120, 130))
    s.SetEnd(v(124, 130))
    segment_grouped = add("graphic_grouped_segment", s)
    s = shape("edge", P.SHAPE_T_RECTANGLE, P.Edge_Cuts, 0.05)
    s.SetStart(v(80, 80))
    s.SetEnd(v(162, 142))
    add("edge_cuts_rectangle", s)

    # Zones.
    def zone(label, zonename, layer, netname, x0, y0, x1, y1):
        z = P.ZONE(board)
        z.SetLayer(layer)
        if netname:
            z.SetNet(net(netname))
        o = z.Outline()
        o.NewOutline()
        for x, y in ((x0, y0), (x1, y0), (x1, y1), (x0, y1)):
            o.Append(MM(x), MM(y))
        z.SetZoneName(zonename)
        board.Add(z)
        return z

    def rule_area(z):
        z.SetIsRuleArea(True)
        z.SetLayerSet(P.LSET.AllCuMask())
        z.SetDoNotAllowTracks(True)
        z.SetDoNotAllowVias(True)
        z.SetDoNotAllowFootprints(True)
        z.SetDoNotAllowZoneFills(True)
        z.SetDoNotAllowPads(True)

    z = zone("zone_gnd", "ZONE_GND", P.F_Cu, "GND", 112, 110, 128, 124)
    o = z.Outline()
    o.NewHole()
    for x, y in ((118, 115), (122, 115), (122, 119), (118, 119)):
        o.Append(MM(x), MM(y), -1, 0)
    z.SetAssignedPriority(3)
    z.SetLocalClearance(MM(0.4))
    z.SetMinThickness(MM(0.3))
    z.SetPadConnection(P.ZONE_CONNECTION_FULL)
    z.SetBorderDisplayStyle(P.ZONE_BORDER_DISPLAY_STYLE_DIAGONAL_FULL, MM(0.7), True)
    z.SetIslandRemovalMode(P.ISLAND_REMOVAL_MODE_NEVER)
    z.SetThermalReliefGap(MM(0.6))
    name("zone_gnd", z)
    via("zone_signal_via", v(114, 112), "ZONE_SIGNAL")
    z = zone("rule_area", "RULE_AREA", P.F_Cu, None, 132, 110, 138, 116)
    rule_area(z)
    name("rule_area", z)
    z = zone("zone_two_layers", "ZONE_TWO_LAYERS", P.F_Cu, "GND", 140, 110, 146, 116)
    layers = P.LSET()
    layers.AddLayer(P.F_Cu)
    layers.AddLayer(P.B_Cu)
    z.SetLayerSet(layers)
    z.SetAssignedPriority(1)
    name("zone_two_layers", z)
    z = zone("zone_locked", "ZONE_LOCKED", P.B_Cu, "GND", 132, 120, 138, 126)
    z.SetLocked(True)
    name("zone_locked", z)
    z = zone("rule_area_locked", "RULE_AREA_LOCKED", P.F_Cu, None, 140, 120, 146, 126)
    rule_area(z)
    z.SetLocked(True)
    name("rule_area_locked", z)
    zone_grouped = name("zone_grouped", zone("zone_grouped", "ZONE_GROUPED", P.B_Cu, "GND", 112, 128, 118, 134))

    # Groups.
    group("group_flat_via_track", "FLAT_VIA_TRACK",
          [track("flat_track", v(100, 92), v(104, 92), "FLAT"), via("flat_via", v(104, 92), "FLAT")])
    group("group_flat_kinds", "FLAT_KINDS", [text_grouped, segment_grouped, arc_grouped, zone_grouped])
    group("group_footprint_via", "FOOTPRINT_VIA", [r1, r1_via])
    inner = group("group_inner", "INNER",
                  [track("inner_track", v(100, 97), v(104, 97), "NESTED"), via("inner_via", v(104, 97), "NESTED")])
    group("group_outer", "OUTER", [inner, track("outer_track", v(104, 97), v(104, 100), "NESTED")])
    mixed_inner = group("group_mixed_inner", "MIXED_INNER", [r2, r2_via])
    r2_track = [t for t in board.GetTracks() if t.m_Uuid.AsString() == ids["R2_pad1_track"]][0]
    group("group_mixed_outer", "MIXED_OUTER", [mixed_inner, r2_track])
    locked = group("group_locked", "LOCKED",
                   [track("locked_track", v(112, 92), v(116, 92), "LOCKED_GROUP"),
                    via("locked_via", v(116, 92), "LOCKED_GROUP")])
    locked.SetLocked(True)
    group("group_three", "THREE",
          [via("three_via", v(116, 97), "THREE"), track("three_track_a", v(112, 97), v(116, 97), "THREE"),
           track("three_track_b", v(116, 97), v(116, 100), "THREE")])

    P.ZONE_FILLER(board).Fill(board.Zones())
    return board, ids


if __name__ == "__main__":
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    board, ids = build()
    path = os.path.join(out, BOARD + ".kicad_pcb")
    P.SaveBoard(path, board)
    with open(os.path.join(out, BOARD + ".ids.json"), "w") as handle:
        json.dump(ids, handle, indent=1, sort_keys=True)
    print(P.Version(), path, len(ids), "named items")
```

### `oracle.json`

`python3 oracle.py pcb_item_transform_kicad10.kicad_pcb oracle.json` writes it. `oracle.py`:

```python
"""pcbnew's own Move and Rotate of every item of pcb_item_transform_kicad10.kicad_pcb.

usage: python3 oracle.py pcb_item_transform_kicad10.kicad_pcb oracle.json

Each case loads a fresh copy of the board and moves or turns every item in
memory. Lengths are integer nanometres, angles degrees as pcbnew holds them.
"""
import json
import sys

import pcbnew as P

CENTRE_MM = (110, 105)
OFFSET_MM = (3.3, -1.7)
TURNS = (90, 45, 33, -135, 180, -90)
# U1 turned alone, its Reference's keep-upright cleared in memory first.
VARIANT_TURNS = (90, 180, -135, 45, -90)
H = {getattr(P, n): n.rsplit("_", 1)[1].lower() for n in dir(P) if n.startswith("GR_TEXT_H_ALIGN_")}
V = {getattr(P, n): n.rsplit("_", 1)[1].lower() for n in dir(P) if n.startswith("GR_TEXT_V_ALIGN_")}


def xy(p):
    return [int(p.x), int(p.y)]


def uid(item):
    return item.m_Uuid.AsString()


def chain(c):
    return [xy(c.CPoint(i)) for i in range(c.PointCount())]


def one_polygon(poly_set):
    assert poly_set.OutlineCount() == 1
    return poly_set


def shape_points(s):
    k = s.GetShape()
    if k == P.SHAPE_T_SEGMENT:
        return [xy(s.GetStart()), xy(s.GetEnd())]
    if k == P.SHAPE_T_RECTANGLE:
        return [xy(s.GetStart()), xy(s.GetEnd())]
    if k == P.SHAPE_T_ARC:
        return [xy(s.GetStart()), xy(s.GetArcMid()), xy(s.GetEnd())]
    if k == P.SHAPE_T_CIRCLE:
        return [xy(s.GetCenter()), xy(s.GetEnd())]
    if k == P.SHAPE_T_POLY:
        poly = one_polygon(s.GetPolyShape())
        assert poly.HoleCount(0) == 0
        return chain(poly.Outline(0))
    if k == P.SHAPE_T_BEZIER:
        return [xy(s.GetStart()), xy(s.GetBezierC1()), xy(s.GetBezierC2()), xy(s.GetEnd())]
    raise ValueError(s.SHAPE_T_asString())


def graphic(s):
    return {"shape": s.SHAPE_T_asString(), "points": shape_points(s)}


def text(t):
    return {"at": xy(t.GetPosition()), "angle": t.GetTextAngle().AsDegrees(),
            "h": H[t.GetHorizJustify()], "v": V[t.GetVertJustify()]}


def record(item):
    c = item.GetClass()
    if c == "FOOTPRINT":
        return {"kind": "footprint", "at": xy(item.GetPosition()), "orientation": item.GetOrientationDegrees(),
                "pads": {uid(p): {"at": xy(p.GetPosition()), "angle": p.GetOrientationDegrees()}
                         for p in item.Pads()},
                "fields": {f.GetName(): text(f) for f in item.GetFields()},
                "graphics": {uid(g): graphic(g) for g in item.GraphicalItems() if g.GetClass() == "PCB_SHAPE"}}
    if c == "PCB_VIA":
        return {"kind": "via", "at": xy(item.GetPosition())}
    if c == "PCB_TRACK":
        return {"kind": "track", "start": xy(item.GetStart()), "end": xy(item.GetEnd())}
    if c == "PCB_ARC":
        return {"kind": "arc", "start": xy(item.GetStart()), "mid": xy(item.GetMid()), "end": xy(item.GetEnd())}
    if c == "PCB_TEXT":
        return dict(text(item), kind="text", mirrored=item.IsMirrored())
    if c == "PCB_TEXTBOX":
        return {"kind": "text_box", "shape": item.SHAPE_T_asString(),
                "points": [xy(p) for p in item.GetCorners()], "angle": item.GetTextAngle().AsDegrees()}
    if c == "PCB_SHAPE":
        return dict(graphic(item), kind="graphic")
    if c == "ZONE":
        poly = one_polygon(item.Outline())
        return {"kind": "zone", "outline": chain(poly.Outline(0)),
                "holes": [chain(poly.Hole(0, h)) for h in range(poly.HoleCount(0))]}
    raise ValueError(c)


def items(board):
    return (list(board.GetFootprints()) + list(board.GetTracks()) + list(board.Zones())
            + list(board.GetDrawings()))


def turned_only_square(item, degrees):
    """The text box and the rounded rectangle are turned by right angles only."""
    if degrees % 90 == 0:
        return False
    rounded = (item.GetClass() == "PCB_SHAPE" and item.GetShape() == P.SHAPE_T_RECTANGLE
               and item.GetCornerRadius() > 0)
    return item.GetClass() == "PCB_TEXTBOX" or rounded


def dump(value, indent=0):
    """JSON with sorted keys and two-space indent; a point, or a list of points, on one line."""
    pad = "  " * indent
    if isinstance(value, dict):
        if not value:
            return "{}"
        rows = ["%s  %s: %s" % (pad, json.dumps(k), dump(value[k], indent + 1)) for k in sorted(value)]
        return "{\n" + ",\n".join(rows) + "\n" + pad + "}"
    if isinstance(value, list):
        flat = all(isinstance(v, int) for v in value)
        points = all(isinstance(v, list) and all(isinstance(n, int) for n in v) for v in value)
        if flat or points:
            return json.dumps(value)
        rows = ["%s  %s" % (pad, dump(v, indent + 1)) for v in value]
        return "[\n" + ",\n".join(rows) + "\n" + pad + "]"
    return json.dumps(value)


def main(source, out):
    centre = P.VECTOR2I(P.FromMM(CENTRE_MM[0]), P.FromMM(CENTRE_MM[1]))
    offset = P.VECTOR2I(P.FromMM(OFFSET_MM[0]), P.FromMM(OFFSET_MM[1]))
    cases = {}
    for name, degrees in [("held", None), ("offset", None)] + [("turn_%d" % d, d) for d in TURNS]:
        board = P.LoadBoard(source)
        case = cases[name] = {}
        for item in items(board):
            if degrees is not None:
                if turned_only_square(item, degrees):
                    continue
                item.Rotate(centre, P.EDA_ANGLE(float(degrees), P.DEGREES_T))
            elif name == "offset":
                item.Move(offset)
            case[uid(item)] = record(item)
    variant = {}
    for degrees in VARIANT_TURNS:
        board = P.LoadBoard(source)
        u1 = board.FindFootprintByReference("U1")
        u1.Reference().SetKeepUpright(False)
        u1.Rotate(centre, P.EDA_ANGLE(float(degrees), P.DEGREES_T))
        variant["turn_%d" % degrees] = {uid(u1): record(u1)}
    with open(out, "w") as handle:
        handle.write(dump({"kicad": P.Version(), "centre_nm": xy(centre), "offset_nm": xy(offset),
                           "cases": cases, "variants": {"u1_reference_not_keep_upright": variant}})
                     + "\n")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
```

### The captures

Open a copy of `pcb_item_transform_kicad10.kicad_pcb` in the PCB Editor with the API
enabled. Send each request in the captures table for that board: `GetItems` with one type;
`GetPadShapeAsPolygon` and `CheckPadstackPresenceOnLayers` naming the pads, and the pads and
vias, in UUID order; `GetBoundingBox` naming the footprints in UUID order. Write each response
message's serialized bytes unchanged. The `parent` KIID of the items outside a footprint
changes each time KiCad opens the board.
