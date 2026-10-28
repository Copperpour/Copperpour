# `four_layer_stackup_kicad10.kicad_pcb` and `four_layer_stackup_kicad10.ipc.bin`

A board that holds a four-layer fabrication stackup and nothing else, and KiCad's
`GetBoardStackup` answer for it, serialised exactly as KiCad's IPC API sent it.

Used by the `GetBoardStackup` client tests
([#78](https://github.com/Copperpour/Copperpour/issues/78)):

- `mock_server_test.rs`, `kicads_captured_answer_is_the_stackup_its_board_file_declares`, which
  CI runs: the mock server replays the capture, and the decoded stackup is checked against the
  board's own `(stackup …)` block;
- `live_kicad_test.rs`, the ignored `the_live_stackup_is_the_one_the_board_file_declares`, which
  asks a running KiCad and makes the same check.

## The board

Made by hand in KiCad 10.0.6's Board Setup on a new board. It has no outline, footprints, nets or
other items.

| Setting | Value |
|---|---|
| Copper layers | 4 |
| Stackup, top to bottom (mm) | mask 0.01 · copper 0.035 · prepreg 0.2104, εr 4.4 · copper 0.0152 · core 0.25, εr 4.23 · copper 0.0152 · prepreg 0.2104, εr 4.4 · copper 0.035 · mask 0.01 |
| Dielectric material, loss tangent | KiCad's defaults: `FR4`, 0.02 |
| Board thickness | 0.7912 mm, as KiCad computes it |
| Copper finish | ENIG |
| Impedance controlled | yes |
| Edge card connector | yes, bevelled |
| Plated board edge | yes |

What it has that `live_ipc.kicad_pcb`'s two-layer stackup does not: prepreg as well as core, four
copper layers, a finish other than `None` (ENIG), impedance control, and both edge settings. It has
no dielectric sub-layers and no specified colours.

## The capture

| | |
|---|---|
| KiCad | 10.0.6, Linux, standalone `pcbnew` under Xvfb with the API enabled |
| Source board | a copy of the board above |
| Message | `kiapi.board.commands.BoardStackupResponse`; the file is its `Any.value` |
| Transport | `GetBoardStackup` over the running editor's IPC socket |
| Size | 510 bytes |

## Regenerating it

Open a copy of the board in KiCad with its API enabled, and run the ignored live test with
`KONNECT_CAPTURE_IPC_FIXTURE` set. `live_kicad_test.rs`'s header says how to find the socket.

```sh
cp crates/copperpour-ipc/tests/fixtures/four_layer_stackup_kicad10.kicad_pcb /tmp/live/
# open /tmp/live/four_layer_stackup_kicad10.kicad_pcb in pcbnew, then:
KICAD_API_SOCKET=<the editor's socket> \
KONNECT_LIVE_KICAD_BOARD=/tmp/live/four_layer_stackup_kicad10.kicad_pcb \
KONNECT_CAPTURE_IPC_FIXTURE=1 \
cargo test -p copperpour-ipc --test live_kicad_test -- \
  --ignored --exact the_live_stackup_is_the_one_the_board_file_declares
```

The capture is written in place, and only when the open board's path ends in this board's file name;
with any other board the test says so and skips it. It writes the capture first, then checks KiCad's
answer against the board's file, so an answer that contradicts the file's declared values fails the
run and, if committed, the replay test in CI. The check covers each entry's layer or dielectric
type, declared thickness and first dielectric sub-layer, the finish, impedance control and edge
settings; not colours, user names, thickness locks, mask εr or silkscreen material.
