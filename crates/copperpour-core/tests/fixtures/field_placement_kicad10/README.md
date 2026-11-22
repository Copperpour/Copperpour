# Field placement fixture

`field_placement.kicad_sch` is a synthetic A4 one-sheet project for symbol field
placement: where a placed part's Reference and Value text lands, and whether it reads
clearly. It backs two proposals, reporting a wire drawn through Reference or Value text
and previewing clear Reference and Value placement, and the cleanup preview's power-lead
case (`#PWR03`).

## Provenance

Drawn by tool calls, not by hand: Copperpour's served tools (`tools/call` over stdio) from
`main` at `891243ab`, with KiCad 10.0.8's stock symbol libraries: `create_project`, one
`batch_place_components` call, `add_power_symbol`, `batch_add_wire` and `add_wire`. The
sheet was then resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8, format
`20260306`) with the project file beside it. Every field therefore sits on its library
anchor, carried through the placement, and no symbol is marked `fields_autoplaced`. The
`.kicad_pro` is the project file `create_project` wrote.

## Contents

| Row | Parts | What it shows |
|---|---|---|
| y = 50.8 | R1 `Device:R` | Value drawn vertically inside the body |
| | R2 `Device:R` placed at 90° | Value drawn over the body |
| | R3 `Device:R` | a part to rotate |
| | C1 `Device:C` | control: fields already beside the body |
| | C2 `Device:C` placed at 90° | both fields drawn vertically |
| | D1, D2 `Device:LED` | control; D2 is a part to rotate |
| | R12 `Device:R` at 90° | a part to mirror |
| | Q1 `Transistor_BJT:BC547` mirrored `y` | control |
| y = 88.9 | U1 `Regulator_Linear:AMS1117-3.3` at `(60.96, 88.9)` | its output wire leaves the right pin and turns up at x = 71.12 through its `AMS1117-3.3` Value |
| | R4, R5, R6 `Device:R` at a 5.08 mm pitch | crowded: the right of R4 and R5 is taken |
| | J1 `Connector_Generic:Conn_01x04` mirrored `y` | control |
| | R8–R11 `Device:R` | parts to move |
| y = 139.7 | U2 `MCU_Microchip_ATtiny:ATtiny85-20P` at `(76.2, 139.7)` | the `+3V3` lead to VCC (pin 8) runs along y = 124.46 through its Reference |
| | R7, R13, R14 `Device:R` | R13's fields for a caller-positioned control |
| | `+3V3`, `GND` | power symbols, not connected |

Neither crossing wire enters a symbol body, so `wire_overlap` stays clear, and
`check_schematic_overlaps` on `main` returns zero findings on the sheet. KiCad 10.0.8
plots both wires through the lettering.

## Expected results (KiCad 10.0.8)

`kicad-cli sch export netlist` lists 54 component pins. Only U2.8 joins a named net
(`+3V3`); the parts are placed for their fields, not wired, so the other 53 pins are on
`unconnected-(…)` nets. The sheet has three wires, 40.64 mm in total.
`kicad-cli sch erc --severity-all` reports 55 `pin_not_connected`, 5
`power_pin_not_driven` and 1 `pin_not_driven` error and 1 `unconnected_wire_endpoint`
warning: this is not a clean sheet, and a field move must leave those counts as they are.
ERC counts leave out `lib_symbol_issues` and `footprint_link_issues`, which depend on the
machine's KiCad library tables.
