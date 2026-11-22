# `solder_margins_u*.ipc.bin`

KiCad 10.0.8's own IPC answers for ten footprints placed from the footprints
in `solder_margins_kicad10.README.md`. `pcb_footprint_update`'s tests read
them for [#723](https://github.com/Copperpour/Copperpour/issues/723), where
`update_footprints_from_library` refused a library footprint with its own or a
pad's solder mask and paste margins.

| File | What |
|------|------|
| `solder_margins_u1.ipc.bin` | U1, `Margins:ep` (`solder_margins_ep_kicad10.kicad_mod`) at (100, 100) mm, 0°, front, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `solder_margins_u2.ipc.bin` | U2, `Margins:ep_zero` at (110, 100) mm, 0°, front, likewise |
| `solder_margins_u3.ipc.bin` | U3, `Margins:footprint` at (120, 100) mm, 0°, front, likewise |
| `solder_margins_u4.ipc.bin` | U4, `Margins:footprint_zero` at (130, 100) mm, 0°, front, likewise |
| `solder_margins_u5.ipc.bin` | U5, `Margins:ep` at (100, 120) mm, 90°, on the back, likewise |
| `solder_margins_u6.ipc.bin` | U6, `Margins:ep_zero` at (110, 120) mm, 90°, on the back, likewise |
| `solder_margins_u7.ipc.bin` | U7, `Margins:footprint` at (120, 120) mm, 90°, on the back, likewise |
| `solder_margins_u8.ipc.bin` | U8, `Margins:footprint_zero` at (130, 120) mm, 90°, on the back, likewise |
| `solder_margins_u9.ipc.bin` | U9, `Margins:footprint` at (100, 140) mm, 0°, front, with overrides set on the board, likewise |
| `solder_margins_u10.ipc.bin` | U10, `Margins:ep` at (110, 140) mm, 0°, front, with overrides and a pad margin set on the board, likewise |

KiCad's `BoardEnabledLayersResponse` for this board listed the same two copper
layers and the same layers, in the same order, as
`issue_283_enabled_layers.ipc.bin`, so the tests read that one.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Linux x86_64 |
| Board | A new board made with KiCad 10.0.8's `pcbnew` Python (`NewBoard`): each of these ten footprints loaded with `FootprintLoad` from the four `.kicad_mod` files as they are, its library id set to `Margins:<name>` with `SetFPID`, its reference set, placed with `SetPosition` and added to the board. The board held five more, U11 to U15, from copies of the same files under other entry names, for a comparison with KiCad's own Update Footprints from Library; they are not kept here. U5 to U8 were then flipped with `Flip(…, FLIP_DIRECTION_TOP_BOTTOM)` and turned to 90° with `SetOrientationDegrees`. U9 was given `SetLocalClearance(0.2 mm)`, `SetLocalSolderMaskMargin(0.07 mm)`, `SetLocalSolderPasteMarginRatio(-0.3)`, `SetLocalZoneConnection(ZONE_CONNECTION_FULL)` and `SetAllowSolderMaskBridges(True)`; U10 `SetLocalClearance(0.15 mm)`, `SetLocalZoneConnection(ZONE_CONNECTION_NONE)`, and on pad `9` `SetLocalSolderMaskMargin(-0.02 mm)`. Saved with `SaveBoard`, then opened in KiCad 10.0.8's PCB Editor with its API enabled and saved there |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` and `GetBoardEnabledLayers` over the running PCB Editor's IPC socket, read-only, through Copperpour's IPC client (`get_items_in`, `get_enabled_layers_in`), after the PCB Editor's save |

Each `.ipc.bin` footprint is the `Any.value` of one item of KiCad's
`GetItemsResponse`, byte for byte, as the `issue_300_*` captures store them.

## What they show

- **Pads.** KiCad reports a pad's own margins on both `front_outer_layers` and
  `back_outer_layers`, the same on each, on the front and on the back: pad
  `9` of U1 and U5 with a mask margin of -50000 nm, a paste margin of
  -100000 nm and a paste ratio of -0.2, of U2 and U6 with 0, 0 and 0. A pad
  without margins of its own reports neither outer layer.
- **Footprints.** KiCad reports a footprint's own margins on its `overrides`:
  U3 and U7 with 50000 nm, -30000 nm and -0.1, U4 and U8 with 0, 0 and 0. Every
  footprint reports `overrides` with `zone_connection` `ZCS_INHERITED` and no
  `copper_clearance` unless the board sets them.
- **Overrides set on the board.** U9 reports a clearance of 200000 nm, a mask
  margin of 70000 nm, the library's paste margin of -30000 nm, a paste ratio
  of -0.3, `ZCS_FULL` and `allow_soldermask_bridges`. U10 reports a clearance
  of 150000 nm and `ZCS_NONE`, and pad `9` a mask margin of -20000 nm with the
  library's paste margin and ratio.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock footprint's description with its datasheet URL, its keywords, Value text
and `${KICAD10_3DMODEL_DIR}` model path, a `${REFERENCE}` text, the library
ids above, the references U1 to U10, field names, KIIDs and protobuf type URLs.
They hold no local path, username or hostname.

## Regenerating them

With KiCad 10.0.8's Python, build the board as above from the four footprint
files and save it. Open it in the PCB Editor with the API enabled and save it
there. Then send `GetItems` for footprints and `GetBoardEnabledLayers` for
that board without changing anything, and write each footprint item's
`Any.value` unchanged.
