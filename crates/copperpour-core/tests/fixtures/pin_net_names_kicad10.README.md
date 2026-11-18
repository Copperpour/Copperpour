# Pin-named net fixture

`pin_net_names_kicad10.kicad_sch` and its child
`pin_net_names_sub_kicad10.kicad_sch` carry nets that no label, power symbol or
sheet pin names, one of each shape KiCad names differently after a pin, so
`get_component_nets`, `get_pin_connections` and `get_pin_net_name` can be
checked against an answer that is not their own.

## Provenance

Built through Copperpour against KiCad 10.0.6's stock `Device`, `Connector`,
`Regulator_Linear`, `Amplifier_Operational`, `74xx` and `power` libraries with
`create_project`, `add_schematic_component`, `connect_pins`, `add_wire`,
`add_schematic_connection`, `add_no_connect`, `add_power_symbol`,
`add_hierarchical_sheet`, `add_schematic_net_label`, `add_sheet_pin`,
`move_schematic_component`, `rotate_schematic_component` and
`edit_schematic_component` (`on_board` on `R16`). `R?` was placed with no
reference. Both files were then force-resaved by KiCad 10.0.6 beside the
project `create_project` wrote:

```text
kicad-cli sch upgrade --force pin_net_names_sub_kicad10.kicad_sch
kicad-cli sch upgrade --force pin_net_names_kicad10.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`. The project file, which
holds only `create_project`'s defaults, is not.

## Cases

| Parts | Shape |
|---|---|
| `U1` (`AP2112K-3.3`) pin 1 `VIN` to R1.1 | a named pin beside a numbered one |
| `J1` (`USB_C_Receptacle_USB2.0_16P`) A5 `CC1` to R2.1 | the same, on a connector |
| J1 A6 `D+` to R3.1; B6 `D+` left alone | a name two numbers share |
| J1 `GND` A1, A12, B1, B12 and `VBUS` A4, A9, B4, B9 | pins stacked on one point, no wire |
| `U2` (`LM358`) unit A pin 3 `+` to R4.1 | a unit, whose `+` unit B's pin 5 shares |
| R9.2 to R10.1 | two numbered pins |
| R7.1 under `PWR_FLAG` `#PWR01` | a flag's pin on the net |
| R5.2 to R6.1, a no-connect flag on R5.2 | a flagged net |
| U1 pin 4 `NC`, a `no_connect` pin, to R14.1 and R20.1 | a pin KiCad joins to nothing |
| R13.2 to `R?` pin 1 | an unannotated symbol's pin beside an annotated one |
| `R?` pin 2 to `TP1` (`TestPoint`) pin 1 | an unannotated symbol's pin naming the net |
| R11.1 and R12.2 to sheet `SUB`'s pin `IN` | a net the sheet below names |
| `J2` (`DP_Sink`) `GND`, numbered `[2,5,8,11,16]`, to R15.1, a no-connect flag on R15.1 | a stacked pin number in the name |
| `U3` (`NE5534`) pin 8 `C/B` to R19.2 | a `/` in a pin name |
| R16.1, `on_board no`, to R8.1 | a symbol kept off the board |
| `#R1` pin 1 to R18.1 | a `#` reference on an ordinary symbol |
| `U4` (`74LS00`) unit A, body style 1, wired to nothing | a symbol whose second body style has the same pins in the same places |

## KiCad's own answer

```text
kicad-cli sch export netlist --output pin_net_names.net pin_net_names_kicad10.kicad_sch
```

The table is KiCad 10.0.8's answer.

| Net | Pins KiCad lists | What it shows |
|---|---|---|
| `Net-(U1-VIN)` | R1.1 U1.1 | a named pin outranks a numbered one, whatever the alphabet says |
| `Net-(J1-CC1)` | J1.A5 R2.1 | the same |
| `Net-(J1-D+-PadA6)` | J1.A6 R3.1 | a shared name carries the pin's number |
| `Net-(J1-GND-PadA1)` | J1.A1 J1.A12 J1.B1 J1.B12 | stacked pins are one net |
| `Net-(J1-VBUS-PadA4)` | J1.A4 J1.A9 J1.B4 J1.B9 | the same |
| `Net-(U2A-+)` | R4.1 U2.3 | the unit's letter; unit B's `+` is not counted as shared |
| `Net-(R10-Pad1)` | R10.1 R9.2 | names compared as text, `R10` before `R9` |
| `Net-(R7-Pad1)` | R7.1 | the flag's pin names nothing but makes R7.1 not alone |
| `unconnected-(R5-Pad2)` | R5.2 R6.1 | a no-connect flag renames the net |
| `unconnected-(J2-GND-Pad2)` | J2.2 J2.5 J2.8 J2.11 J2.16 R15.1 | a stacked number by its first |
| `Net-(U3-C{slash}B)` | R19.2 U3.8 | `/` written as `{slash}` |
| `Net-(R8-Pad1)` | R8.1 | R16, off the board, names nothing |
| `Net-(#R1-Pad1)` | R18.1 | `#R1` names the net it is left out of |
| `Net-(R13-Pad2)` | R13.2 R?.1 | `R?`'s pin drives the net too, and `R13` sorts before `R?` |
| `Net-(R?-`*hash*`-Pad2)` | R?.2 TP1.1 | `R?` before `TP1`; *hash* is eight hex digits of `KIID::Hash` of `R?`'s UUID, so the tools answer `null` |
| `Net-(R14-Pad1)` | R14.1 R20.1 | the `no_connect` pin's wire, without it |
| `unconnected-(U1-NC-Pad4)` | U1.4 | the `no_connect` pin, joined to nothing |
| `/SUB/IN` | R11.1 R12.2 | named after the child's hierarchical label |
| `unconnected-(U4-Pad1)`, `-Pad2`, `-Pad3` | U4.1; U4.2; U4.3 | each pin once, from the placed body style |

Every other pin KiCad lists is alone on an `unconnected-(…)` net. It lists no
pin of R16, which is off the board, or of `#R1`.

KiCad 10.0.6 differs in the two `R?` rows, its unannotated pins driving no net
(`SCH_CONNECTION::IsDriver` before KiCad commit 534f93d1): R13.2 and R?.1 are
`unconnected-(R13-Pad2)`, R?.2 and TP1.1 `unconnected-(TP1-Pad1)`. KiCad
`10.0.8~ubuntu24.04.1` writes *hash* as `dc34389b`.

Two copies of the fixture, not committed, give the two unit notations no
Copperpour tool writes. With the project file setting
`"subpart_id_separator": 46` and `"subpart_first_id": 49`, R4.1's net is
`Net-(U2.1-+)`. With `(unit_name "OPA")` added to the `LM358_1_1` unit, it is
`Net-(U2OPA-+)`.

## ERC

`kicad-cli sch erc --severity-all` finds 85 violations on 10.0.6 and 10.0.8
alike, the cost of a sheet built out of stubs: 60 `pin_not_connected`, 9
`pin_not_driven`, 6 `power_pin_not_driven`, 3 `no_connect_connected` (U1.4
with its wire, R6.1 and J2's `GND` with the flags), 1 `label_dangling` (the
child's `IN`), and 2 each of `missing_unit`, `missing_input_pin` and
`missing_power_pin`, for the LM358 and 74LS00 units not placed. `kicad-cli`
also warns of the annotation error `R?` is.
