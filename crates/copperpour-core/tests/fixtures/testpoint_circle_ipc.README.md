# `testpoint_circle_*`

KiCad 10.0.8's own IPC answers for two placements of a stock footprint whose
silkscreen circle has its radius point below its centre, and that library
footprint. `pcb_footprint_update`'s tests read them: the library build gave
every circle the radius point centre + (radius, 0), so a refresh planned
`graphics` for this footprint straight after it was placed from its library,
and an applied refresh would have left a circle KiCad's library parity check
reports as different.

| File | What |
|------|------|
| `testpoint_circle_tp1.ipc.bin` | TP1, `TestPoint:TestPoint_THTPad_D2.0mm_Drill1.0mm` at (120, 80) mm, 0°, as KiCad reported it: one `kiapi.board.types.FootprintInstance` |
| `testpoint_circle_tp2.ipc.bin` | TP2, the same footprint at (130, 80) mm, 90°, likewise |
| `testpoint_thtpad_d2.0mm_drill1.0mm_kicad10.kicad_mod` | KiCad 10.0.8's stock `TestPoint.pretty/TestPoint_THTPad_D2.0mm_Drill1.0mm.kicad_mod`, with LF line endings as in KiCad's footprint library (the Windows installer ships it with CRLF); otherwise unchanged |

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Windows 11 |
| Board | A new board made with KiCad 10.0.8's `pcbnew` Python (`NewBoard`): the footprint loaded twice with `FootprintLoad` from KiCad's stock library as installed, its library id set with `SetFPID`, its reference set, placed with `SetPosition` and `SetOrientationDegrees`, its pad put on a made-up net `TP_SIG`, and saved with `SaveBoard`. The board was then opened in KiCad 10.0.8's PCB Editor, unchanged |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` over the running PCB Editor's IPC socket, read-only, as `update_footprints_from_library` reads them |
| Captured | 2026-11-16 |

Each `.ipc.bin` is the `Any.value` of one item of KiCad's `GetItemsResponse`,
byte for byte, as the `c_0603_*` and `issue_254_*` captures store them.

## What they show

- **KiCad keeps the library's radius point.** The silkscreen circle is
  `(center 0 0) (end 0 1.2)` in the library. KiCad reports TP1's at
  (120, 81.2) mm, below the centre, and TP2's at (131.2, 80) mm, the same
  point turned 90° with the footprint. The courtyard circle,
  `(end 1.5 0)`, is at (121.5, 80) and (130, 78.5) mm.
- **The refresh.** With the circle built from the library's radius point,
  both footprints dry-run to `noop`. Built as centre + (radius, 0), both
  plan `graphics`.

On the same board in KiCad, `update_footprints_from_library` dry-ran to
`ready` with `graphics` planned for TP1 and TP2 on `main` (`b89f9b73`), and
to `noop` with the radius point kept.

## Redaction

Nothing was removed; there is nothing private in them. The messages hold the
stock library's id, description, keywords and fields, the references TP1 and
TP2, the made-up net, field names, KIIDs and protobuf type URLs. They hold no
path, username or hostname.

## Regenerating them

Build the board as above with KiCad's Python, open it in KiCad's PCB Editor
with the API enabled, and without changing anything send `GetItems` for
`KOT_PCB_FOOTPRINT` for that board; write each returned footprint's
`Any.value` unchanged. The library file is copied from KiCad's stock footprint
directory, with its line endings made LF.
