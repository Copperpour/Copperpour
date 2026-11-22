# Route elbows fixture

`route_elbows_kicad10.kicad_sch` puts a symbol body or label text on one elbow of a
pin-to-pin route, next to controls. It backs the proposal that pin-to-pin routing takes
the clearer of its two elbows: which elbow `connect_pins`, `add_schematic_connection` and
`batch_connect_pins` draw.

## Provenance

Drawn by tool calls, not by hand, against KiCad's stock `Device` library:
`create_schematic` (A4), one `batch_place_components` call for R1 to R19 (`Device:R`,
value `10k`), and `add_schematic_net_label` for `SIG_TEXT_OBSTACLE` and `SIG_E`. Then
resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8), so the header reads
`(version 20260306)` and `(generator "eeschema")`. The resave kept every item, UUID,
coordinate and property; it added KiCad's default fields and pin UUIDs, and wrote the
symbol instances under the project name `""`, as a save without a project does. KiCad
10.0.8's netlist, ERC and plotted drawing are the same before and after the resave, so the
tables below still hold.

Every part is `Device:R`. At rotation 0, pin 1 is at `y - 3.81` and pin 2 at `y + 3.81`,
and the body spans 2.032 x 5.08 mm about the centre. R6 and R11 are at rotation 90, so pin
1 is at `x - 3.81` and pin 2 at `x + 3.81`.

| Group | Parts | Route | In the way |
|---|---|---|---|
| body | R1 (50.8, 50.8), R2 (76.2, 67.31), R3 (63.5, 54.61) | R1.2 -> R2.1 | R3's body on the horizontal-first leg y = 54.61; no pin on either elbow |
| text | R4 (101.6, 50.8), R5 (127, 67.31), R6 (114.3, 59.69) rot 90; label `SIG_TEXT_OBSTACLE` on R6.2 at (118.11, 59.69) | R4.2 -> R5.1 | the label's text, drawn by KiCad from x 118.11 to 136.94, crosses the horizontal-first leg x = 127; its anchor is on neither elbow |
| clear | R7 (50.8, 101.6), R8 (76.2, 118.11) | R7.2 -> R8.1 | nothing |
| straight | R9 (101.6, 101.6), R10 (101.6, 119.38), R11 (101.6, 110.49) rot 90 | R9.2 -> R10.1 | R11's body on the straight line; its pins are off it |
| fallback | R12 (50.8, 139.7), R13 (76.2, 156.21), R14 (63.5, 143.51), R15 (63.5, 156.21); label `SIG_E` on R15.1 at (63.5, 152.4), rot 180 | R12.2 -> R13.1 | R14's body on the horizontal-first leg; R15.1 and `SIG_E` on the vertical-first leg |
| blocked | R16 (101.6, 139.7), R17 (127, 156.21), R18 (114.3, 147.32), R19 (120.65, 156.21) | R16.2 -> R17.1 | R18.1 on the horizontal-first leg, R19.1 on the vertical-first leg |

KiCad 10.0.8's netlist of the fixture has 38 nets: `/SIG_E` (R15.1), `/SIG_TEXT_OBSTACLE`
(R6.2), and an `unconnected-(Rn-Padm)` net for every other pin.
`kicad-cli sch erc --severity-all` reports 36 `pin_not_connected` and 2
`isolated_pin_label`.

## Expected routes, checked with KiCad's netlist of each result

Each tool ran on a copy of the fixture through the served `tools/call` dispatch:
`connect_pins` once per pair in the order above, and one `batch_connect_pins` with all six
pairs. Both give the same wires. "Today" is `main` at `456a8a66`; "Proposed" is the
clearer-elbow rule.

| Group | Today | Proposed |
|---|---|---|
| body | (50.8, 54.61) -> (76.2, 54.61) -> (76.2, 63.5), through R3's body | (50.8, 54.61) -> (50.8, 63.5) -> (76.2, 63.5) |
| text | (101.6, 54.61) -> (127, 54.61) -> (127, 63.5), through the label text | (101.6, 54.61) -> (101.6, 63.5) -> (127, 63.5) |
| clear | (50.8, 105.41) -> (76.2, 105.41) -> (76.2, 114.3) | the same |
| straight | (101.6, 105.41) -> (101.6, 115.57) | the same |
| fallback | (50.8, 143.51) -> (76.2, 143.51) -> (76.2, 152.4) | the same: the clearer elbow would join `SIG_E` |
| blocked | refused, `plan_blocked`, nothing written | the same, with the same two reasons |

On both, `kicad-cli sch export netlist` (10.0.8) adds exactly five two-pin nets,
`Net-(R1-Pad2)` (R1.2 R2.1), `Net-(R4-Pad2)` (R4.2 R5.1), `Net-(R7-Pad2)` (R7.2 R8.1),
`Net-(R10-Pad1)` (R9.2 R10.1) and `Net-(R12-Pad2)` (R12.2 R13.1), and removes the ten
single-pin nets they replace. `/SIG_E`, `/SIG_TEXT_OBSTACLE` and R14 to R19 are unchanged.
ERC drops the ten `pin_not_connected` for those pins and adds nothing.

`batch_connect_pins` reports `connected_count` 5 and one error:

```text
R16.2 -> R17.1: the horizontal-first route would join R18 pin 1 at (114.3, 143.51); the vertical-first route would join R19 pin 1 at (120.65, 152.4)
```
