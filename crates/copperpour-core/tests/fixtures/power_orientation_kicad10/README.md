# Power-symbol orientation fixture

`power_orientation.kicad_sch` holds pins that face each way a power symbol can meet them,
with no power symbol on any of them yet. It backs the proposal for `add_power_symbol` and
`batch_add_power_symbol` by pin: positive rails should point up and ground down, as the
library draws them, without their value text landing on a neighbour.

## Provenance

Drawn by tool calls, not by hand, through Copperpour's stdio server (`copperpour` built
from `main` at `456a8a66`) against KiCad 10.0.8's stock `Connector_Generic`, `Device` and
`power` libraries:

- `create_project` named `power_orientation`;
- `create_symbol` for `Fixture:CONN_6`, a generic 6-pin connector with its pin names drawn
  (`V3`, `SIGA`, `GND`, `SIGB`, `SIGC`, `GND`) and its pins on the left edge, written to a
  scratch library and embedded in the sheet when placed;
- one `batch_place_components` call;
- three coordinate `add_power_symbol` calls that put `#PWR01` (`+3V3`), `#PWR02` (`GND`)
  and `#PWR03` (`-5V`) at `(165.1, 152.4)`, `(177.8, 152.4)` and `(190.5, 152.4)`, clear
  of every pin, so the sheet embeds KiCad's own `power:+3V3`, `power:GND` and `power:-5V`;
- `connect_to_net` (default stub) for `SIG_A`, `SIG_B` and `SIG_C` on J6 pins 2, 4 and 5.

KiCad 10.0.8 then parsed and force-resaved it, with the project file beside it:

```text
kicad-cli sch upgrade --force power_orientation.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`. The instance records name project
`power_orientation`, so a test should copy the sheet under that name. The project file is
not committed; a copy without it still loads. KiCad's netlist and ERC of the sheet are the
same before and after the resave (21 component pins, no pin changes net; the same 42 ERC
findings).

## Cases

| Symbol | Placement | Pins (KiCad's positions) | Case |
|---|---|---|---|
| `J1` `Conn_01x02` | `(63.5, 63.5)`, mirrored about y | 1 `(68.58, 63.5)`, 2 `(68.58, 66.04)`, both facing right | `+3V3` on pin 1 above `GND` on pin 2 |
| `J2` `Conn_01x02` | `(63.5, 88.9)`, rotated 180 | 1 `(68.58, 88.9)`, 2 `(68.58, 86.36)`, both facing right | the same rails with pin 1 below pin 2: turned up and down they would point at each other |
| `J3` `Conn_01x01` | `(63.5, 109.22)`, mirrored about y | 1 `(68.58, 109.22)`, facing right | `-5V`, a negative rail |
| `J4` `Conn_01x01` | `(63.5, 124.46)`, mirrored about y | 1 `(68.58, 124.46)`, facing right | explicit `rotation` control |
| `J5` `Conn_01x03` | `(63.5, 139.7)`, mirrored about y | 1 `(68.58, 137.16)`, 2 `(68.58, 139.7)`, 3 `(68.58, 142.24)` | a row of `GND` pins for `batch_add_power_symbol` |
| `R1` `Device:R` | `(101.6, 63.5)` | 1 `(101.6, 59.69)` up, 2 `(101.6, 67.31)` down | `+3V3` up, `GND` down: already the library direction |
| `R2` `Device:R` | `(119.38, 63.5)` | 1 `(119.38, 59.69)` up, 2 `(119.38, 67.31)` down | `GND` on the upward pin, `+3V3` on the downward pin |
| `R3` `Device:R` | `(137.16, 63.5)` | 1 `(137.16, 59.69)` up, 2 `(137.16, 67.31)` down | explicit `rotation` on pin 1, `-5V` on pin 2 |
| `J6` `Fixture:CONN_6` | `(124.46, 116.84)` | 1 `(116.84, 111.76)` … 6 `(116.84, 124.46)`, facing left | supply on pin 1, ground on pins 3 and 6, labels on 2, 4, 5 |

## Which way a library draws each rail

A KiCad pin's angle points into the body, so a power symbol's single pin angle is the
direction its graphic points when placed unrotated. In KiCad 10.0.8's `power.kicad_sym`,
12 symbols point down (`Earth`, `Earth_Clean`, `Earth_Protective`, `GND`, `GND1`, `GND2`,
`GND3`, `GNDA`, `GNDD`, `GNDPWR`, `GNDREF`, `GNDS`) and the other 90 point up, including
the negative rails (`-5V`, `-12V`, `VEE`, `VSS`) and `PWR_FLAG`.

## What placing them produces today (`main` at `456a8a66`)

Rotation left to the tool, each rail faces away from its pin's body:

| Call | KiCad sees |
|---|---|
| `+3V3` on J1.1, `GND` on J1.2 | rotations 270 and 90: both on their side, value texts collide |
| `+3V3` on J2.1, `GND` on J2.2 | the same |
| `-5V` on J3.1 | rotation 270, on its side |
| `+3V3` and `GND` on R1 | rotation 0 on both: up and down |
| `GND` on R2.1, `+3V3` on R2.2, `-5V` on R3.2 | rotation 180: inverted |
| `+3V3` on J6.1 | rotation 90, value text over the `SIG_A` label |
| `batch_add_power_symbol` `GND` on J6.3 and J6.6 | not joined (the bar would touch `SIG_B`'s label); each rotation 270, pin 3's value over `SIG_B` |

```text
kicad-cli sch export svg power_orientation.kicad_sch
kicad-cli sch export netlist --format kicadsexpr power_orientation.kicad_sch
kicad-cli sch erc --format json --severity-all --units mm power_orientation.kicad_sch
```

Spelling the proposed shape with today's explicit arguments (`add_wire` for a 2.54 mm stub
out of J1.1, J1.2, J3.1, J6.1 and J6.6, `add_power_symbol` at its end with rotation 0, a
stub-and-bar row on J5 with `GND` unrotated at its lower end, today's call everywhere
else) gives the same netlist as the default calls (no component pin changes net) and the
same ERC findings, and leaves only the J2 and J6.3 value collisions, which the proposal
reports rather than fixes. Placing the rail unrotated on the pin tip instead, without a
stub, hangs J1.2's `GND` over J1's own `Conn_01x02` value text, so the proposal uses the
stub.
