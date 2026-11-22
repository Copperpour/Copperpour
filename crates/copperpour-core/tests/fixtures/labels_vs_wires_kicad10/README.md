# Labels versus wires fixtures

Four standalone one-sheet A4 projects for deciding when a net label reads better than a
wire. They back the proposal to advise when one long wire run would read better as two
same-sheet labels, and when it would not. Each `.kicad_pro` is the project file
`create_project` wrote; keep each sheet beside it.

## Provenance

Drawn by tool calls, not by hand (`create_project`, `batch_place_components`,
`batch_add_wire`, `add_power_symbol`, `add_schematic_net_label`, `batch_add_junction`,
`batch_connect_to_net`, `move_schematic_component`, `delete_schematic_net_label`) against
KiCad 10's stock `Device`, `Connector_Generic`, `Amplifier_Operational` and `power`
libraries, then resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8) beside each
`.kicad_pro`, so each carries `(generator "eeschema")` and `(version 20260306)`. Nothing
was edited by hand. kicad-cli 10.0.8 on Linux gives the same netlist and ERC before and
after the resave, and its plots draw the same paths (only drawing order and the plot date
differ).

| Sheet | SHA-256 as authored | SHA-256 after the resave |
|---|---|---|
| `crossing/crossing.kicad_sch` | `a919d65c…1ec7b22e` | `77fe7b65…e57d3ba9` |
| `analog/analog.kicad_sch` | `337f7ac8…22862b8d` | `f198d8af…d792a977` |
| `adjacent/adjacent.kicad_sch` | `6296a593…b70f45ee` | `0e60ce69…60647090` |
| `names/names.kicad_sch` | `4d4d1c44…ac05d93c` | `fd92fba5…f409899f` |

## `crossing/crossing.kicad_sch`: a long run through another block

J1 (`Conn_01x02`, pins facing right at x 55.88) carries `SIG_A` on pin 2 at
`(55.88, 96.52)`, named by a local label on the pin tip. One 143.51 mm wire runs straight
from there to R3 pin 1 at `(199.39, 96.52)`, which drives the LED D1. On its way it
crosses two wires of block B, a divider R1/R2 with C1 and C2: the `+5V` feed to C2 at x
114.3 and the R1-R2 midpoint wire at x 127. Neither crossing has a junction dot, so
neither connects, but at x 127 the run passes the divider's midpoint where a reader
expects a connection.

## `analog/analog.kicad_sch`: a compact op-amp stage (control)

U1 (`MCP6001-OT`) as a non-inverting amplifier: J1 pin 2 (`SIG_IN`) to U1 `+`, the output
U1 pin 1 to J2 pin 1 through a local `SIG_OUT` label on the wire at `(147.32, 88.9)`, and
the feedback divider R1 (output to `-`) and R2 (`-` to GND). The feedback run goes from
the output junction at `(137.16, 88.9)` down to `(137.16, 101.6)` and left to R1 pin 2 at
`(135.89, 101.6)`. C1 decouples `+3V3`. Every wire is short and none crosses another net.

## `adjacent/adjacent.kicad_sch`: two labels a wire would replace

R1 and R2, horizontal on one row 38.1 mm apart, each with its outer pin to GND. R1 pin 2
and R2 pin 1 are joined only by two local `SIG_C` labels on 2.54 mm stubs, at
`(107.95, 88.9)` and `(133.35, 88.9)`, 25.4 mm apart with nothing between them.

## `names/names.kicad_sch`: net-name cases

Four rows of two horizontal resistors joined by a 30.48 mm wire from the left part's pin 2
at x 80.01 to the right part's pin 1 at x 110.49, each outer pin to GND, and R9 alone
below:

| Row y | Parts | The joining net |
|---|---|---|
| 50.8 | R1, R2 | unnamed (`Net-(R1-Pad2)`) |
| 71.12 | R3, R4 | local label `SIG_B` on R3 pin 2 |
| 91.44 | R5, R6 | `+3V3` power symbol on R5 pin 2 |
| 111.76 | R7, R8 | unnamed (`Net-(R7-Pad2)`) |
| 132.08 | R9 | local label `SIG_X` on R9 pin 2, which reaches no other pin |

## KiCad's answer (kicad-cli 10.0.8)

`kicad-cli sch export netlist`, power symbols left out:

| Sheet | Nets |
|---|---|
| crossing | `+5V` C2.1 R1.1; `/SIG_A` J1.2 R3.1; `GND` C1.2 C2.2 D1.1 J1.1 R2.2; `Net-(C1-Pad1)` C1.1 R1.2 R2.1; `Net-(D1-A)` D1.2 R3.2 |
| analog | `+3V3` C1.1 J1.3 U1.5; `/SIG_IN` J1.2 U1.3; `/SIG_OUT` J2.1 R1.2 U1.1; `GND` C1.2 J1.1 J2.2 R2.2 U1.2; `Net-(U1--)` R1.1 R2.1 U1.4 |
| adjacent | `/SIG_C` R1.2 R2.1; `GND` R1.1 R2.2 |
| names | `+3V3` R5.2 R6.1; `/SIG_B` R3.2 R4.1; `/SIG_X` R9.2; `GND` the nine outer pins; `Net-(R1-Pad2)` R1.2 R2.1; `Net-(R7-Pad2)` R7.2 R8.1 |

`kicad-cli sch erc --severity-all` reports `power_pin_not_driven` on the sheets' power
symbols (there is no `PWR_FLAG`: two on crossing, analog and names, one on adjacent), one
`isolated_pin_label` for `SIG_X` on names, and `footprint_link_issues` for U1 on analog.
Without KiCad's library tables it also reports `lib_symbol_issues` for every symbol.
