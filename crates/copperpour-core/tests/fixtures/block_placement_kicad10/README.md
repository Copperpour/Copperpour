# Block placement fixtures

Four standalone A4 one-sheet projects for two proposals: previewing where an IC's
supporting parts could go around it, and previewing the signal order of explicit blocks on
one sheet. Only the before-states are stored. These are drawing examples, not electrical
design recommendations.

## Provenance

Drawn by tool calls, not by hand, against KiCad 10 stock libraries, with explicit wires
and junctions and with power symbols on wire stubs, then resaved with
`kicad-cli sch upgrade --force` (KiCad 10.0.8). Each `.kicad_pro` is the project file
`create_project` wrote, with its defaults. The symbol instances name that project, so keep
each sheet in its own directory beside it.

## The sheets

| Sheet | Circuit | Case |
| --- | --- | --- |
| `opamp_stage/opamp_stage.kicad_sch` | One MCP6001 with a series input resistor, feedback and ground-return resistors, and a bypass capacitor, all in a row below U1 | Arrange the four members around the fixed IC. Wires loop around the stage, and C1 sits on a separate `+5V` island with its own symbol. |
| `ldo_enable/ldo_enable.kicad_sch` | AP2112K-3.3 with a resistor pulling EN to VIN and one capacitor per rail | Arrange the three members, keeping the no-connect flag on U1.4 |
| `two_blocks/two_blocks.kicad_sch` | Two copies of the op-amp stage, each arranged, but with U1 drawn to the right of the U2 it drives | J1 feeds U1, U1 drives U2 through R4, and U2 feeds J2. Three long runs wrap around the stages. Order the blocks between the connectors. |
| `shared_rail/shared_rail.kicad_sch` | Two open-drain buffers with a shared pull-up and two rail capacitors | Both capacitors sit across the same `+3V3` and `GND` as both buffers, and R1 pulls up a net that U1.4 and U2.4 both reach. Without an assignment, no supporting part belongs to one buffer. |

## Expected results (KiCad 10.0.8)

`kicad-cli sch export netlist`, power symbols left out:

| Sheet | Nets | Wires |
| --- | --- | --- |
| opamp_stage | `+5V` C1.1 J3.1 U1.5; `GND` C1.2 J1.2 J2.2 J3.2 R3.2 U1.2; `Net-(J1-Pin_1)` J1.1 R1.1; `Net-(J2-Pin_1)` J2.1 R2.2 U1.1; `Net-(U1-+)` R1.2 U1.3; `Net-(U1--)` R2.1 R3.1 U1.4 | 27 segments, 497.84 mm |
| ldo_enable | `+3V3` C2.1 J2.1 U1.5; `+5V` C1.1 J1.1 R1.1 U1.1; `GND` C1.2 C2.2 J1.2 J2.2 U1.2; `Net-(U1-EN)` R1.2 U1.3; U1.4 unconnected (no-connect flag) | 20 segments, 170.18 mm |
| two_blocks | `+5V` C1.1 C2.1 J3.1 U1.5 U2.5; `GND` C1.2 C2.2 J1.2 J2.2 J3.2 R3.2 R6.2 U1.2 U2.2; `Net-(J1-Pin_1)` J1.1 R1.1; `Net-(J2-Pin_1)` J2.1 R5.2 U2.1; `Net-(R2-Pad2)` R2.2 R4.1 U1.1; `Net-(U1-+)` R1.2 U1.3; `Net-(U1--)` R2.1 R3.1 U1.4; `Net-(U2-+)` R4.2 U2.3; `Net-(U2--)` R5.1 R6.1 U2.4 | 43 segments, 718.82 mm |
| shared_rail | `+3V3` C1.1 C2.1 J3.1 R1.1 U1.5 U2.5; `GND` C1.2 C2.2 J2.2 J3.2 U1.3 U2.3; `Net-(J1-Pin_1)` J1.1 U1.2; `Net-(J1-Pin_2)` J1.2 U2.2; `Net-(J2-Pin_1)` J2.1 R1.2 U1.4 U2.4; U1.1 and U2.1 unconnected | 24 segments, 274.32 mm |

`kicad-cli sch erc --severity-all` reports nothing on any of the four sheets. ERC counts
leave out `lib_symbol_issues` and `footprint_link_issues`, which depend on the machine's
KiCad library tables. A placement preview must keep every pin on the net listed here.
