# Issue 95 live KiCad IPC captures

The `issue_95_*.ipc.bin` files are KiCad's answers to its IPC API, byte for byte, for
[#95](https://github.com/Copperpour/Copperpour/issues/95) (`apply_copper`). Nothing was
rebuilt or edited after capture.

The board was the [#96](https://github.com/Copperpour/Copperpour/issues/96) repro board
(`issue96.kicad_pcb`, opened and re-saved by KiCad 10.0.8), open in the PCB Editor: R1
(`R_0603_1608Metric`, pad 1 `NET_A`, pad 2 `NET_B`) at (110, 95), R2 (pad 1 `NET_C`, pad 2
`NET_D`) at (100, 110), nets `NET_A` to `NET_D`, two copper layers, 3 tracks and 2 vias.

One explicit commit (`BeginCommit`, `CreateItems`, `EndCommit`) created four items, all
requested on `NET_D`:

| Item | Request |
|------|---------|
| T1 | track (130, 90)-(135, 90), F.Cu, 0.25 mm, clear of other copper |
| V1 | through via at (135, 90), 0.6 mm, 0.3 mm drill, at T1's end |
| T2 | track (95, 110)-(99.175, 110), F.Cu, 0.25 mm, ending on R2 pad 1 (`NET_C`) |
| V2 | through via at (109.175, 95), 0.6 mm, 0.3 mm drill, on R1 pad 1 (`NET_A`) |

Every created item was then read back by KIID, and all four were deleted in a second commit.
The board was never saved.

| File | Message | Request |
|------|---------|---------|
| `issue_95_nets.ipc.bin` | `NetsResponse` | `GetNets`, before the batch |
| `issue_95_enabled_layers.ipc.bin` | `BoardEnabledLayersResponse` | `GetBoardEnabledLayers` |
| `issue_95_create_response.ipc.bin` | `CreateItemsResponse` | `CreateItems` for T1, V1, T2, V2, inside the commit |
| `issue_95_readback.ipc.bin` | `GetItemsResponse` | `GetItemsById` for the four KIIDs, after `EndCommit` |
| `issue_95_delete_response.ipc.bin` | `DeleteItemsResponse` | `DeleteItems` for the four KIIDs |

## What they show

- `CreateItems` answers `ISC_OK` for all four and names `NET_D` in every created item,
  T2 and V2 included. KiCad serializes the item before the commit is pushed, so its answer
  echoes the request and is no evidence of the net it stores.
- After `EndCommit`, the read-back shows KiCad's choice: T1 and V1 `NET_D`, T2 `NET_C` (the
  pad its end touches), V2 `NET_A` (the pad it sits on).
- `DeleteItems` answers `IDS_OK` for each of the four (KiCad 10.0.8 fills `deleted_items`),
  and the board went back to its 3 tracks and 2 vias.
- `GetItemsById` for KIIDs that no longer exist is refused, not answered with an empty list:
  "none of the requested IDs were found or valid". A refusal carries no response message, so
  there is no file for it. The capture kept its text; the tests send it with `AS_BAD_REQUEST`,
  the status KiCad's `API_HANDLER_PCB::handleGetItemsById` gives that refusal.
- The `CreateItems` request itself is not kept: the capture script built it, not KiCad, and
  its document header names the capture machine's scratch directory. Its items are the
  table above.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8 (macOS arm64), PCB Editor with the IPC API enabled |
| Client | kipy, sending each command and writing the response's serialized bytes |
| Captured | 2026-11-02; `issue_95_enabled_layers.ipc.bin` by the #96 capture of the same board that day, the rest by the #95 capture |
