# `teardrops_kicad10.kicad_pcb` and `teardrops_kicad10.ipc.bin`

A four-layer board whose GND pads and via carry native teardrop settings, saved by KiCad 10.0.8, and KiCad 10.0.8's `GetItems` answer for its pads and vias, serialised exactly as KiCad's IPC API sent it. Used by the teardrop-settings work ([#555](https://github.com/Copperpour/Copperpour/issues/555)):

- `teardrop_fixture_test.rs`, which CI runs: the mock server replays the capture, and every decoded pad and via is checked against the board's own `(teardrops …)` blocks;
- `live_teardrop_test.rs`, ignored, which drives a running KiCad through the same changes.
- `copperpour-core`'s `pcb_teardrops` tests, which CI runs: the `get_teardrop_settings` and `set_teardrop_settings` tools against a KiCad double holding this capture and board, and, ignored, `live_teardrop_tools_change_settings_and_copper` against a running KiCad holding a copy of the board (`KICAD_API_SOCKET`, `COPPERPOUR_LIVE_TEARDROP_BOARD`).

## The board

The starting point is `copperpour-core/tests/fixtures/specctra_zones.kicad_pcb`, unchanged (its README says how KiCad generated it): R1 and R2 (0402) and J1 (1x02 header), a GND track from R1 pad 2 to a through via at (105, 50) and on to In1.Cu, and three pours.

The live test opened a copy in KiCad 10.0.8's `pcbnew`, set these settings on the three GND pads and the via through IPC (`UpdateItems` in one commit; pads inside their footprints), refilled, and had KiCad save it:

| Setting | Value |
|---|---|
| enabled | yes |
| curved edges | yes |
| allow two segments | no |
| prefer zone connections | no |
| max length | 1.5 mm |
| max width | none (`0`) |
| best length ratio | 0.6 |
| best width ratio | 0.9 |
| filter ratio (max track width ratio) | 0.8 |

The VCC pads keep KiCad's defaults, which KiCad does not write, so the board has both forms. KiCad built three teardrop zones (R1 pad 2 on F.Cu; the via on F.Cu and In1.Cu); J1 pad 2 and R2 pad 1 have no track and get none. Nothing in the file was edited by hand.

## The capture

| | |
|---|---|
| KiCad | 10.0.8 (`kicad/kicad:10.0.8` image), Linux, standalone `pcbnew` under Xvfb with the API enabled |
| Source board | the board above, open in the editor after KiCad saved it |
| Message | `kiapi.common.commands.GetItemsResponse`; the file is its `Any.value` |
| Request | `GetItems` with `KOT_PCB_PAD` and `KOT_PCB_VIA` |
| Size | 2058 bytes: six pads and one via |

## Checked on 10.0.7

The same live test, run the same way in `kicad/kicad:10.0.7`, passes. KiCad 10.0.7 saved a board byte-identical to this one, and its capture differs only in the via's `parent` KIID, which KiCad assigns per session. 10.0.7 is the first release that carries these settings over IPC.

## Regenerating it

Open a copy of `specctra_zones.kicad_pcb`, named `teardrops_kicad10.kicad_pcb`, in KiCad 10.0.7 or newer with its API enabled, and run the ignored live test with `COPPERPOUR_CAPTURE_IPC_FIXTURE` set. `live_kicad_test.rs`'s header says how to find the socket.

```sh
cp crates/copperpour-core/tests/fixtures/specctra_zones.kicad_pcb /tmp/live/teardrops_kicad10.kicad_pcb
# open /tmp/live/teardrops_kicad10.kicad_pcb in pcbnew, then:
KICAD_API_SOCKET=<the editor's socket> \
COPPERPOUR_LIVE_KICAD_BOARD=/tmp/live/teardrops_kicad10.kicad_pcb \
COPPERPOUR_CAPTURE_IPC_FIXTURE=1 \
cargo test -p copperpour-ipc --test live_teardrop_test -- --ignored --nocapture
cp /tmp/live/teardrops_kicad10.kicad_pcb crates/copperpour-ipc/tests/fixtures/
```

The capture is written in place only when the open board's file name is this one. KiCad saves the board before the capture is taken, so the two describe the same state; the test then disables the settings again in the editor without saving.
