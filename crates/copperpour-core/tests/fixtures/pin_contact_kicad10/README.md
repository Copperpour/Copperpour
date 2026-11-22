# Pin-to-pin contact fixture

`pin_contact.kicad_sch` holds pins of two symbols that meet with no wire between them,
next to the near cases that must not count as such a contact. It backs the proposed report
of symbols whose pins touch with no wire between them, and the reproduction of
`connect_pins` writing a zero-length wire when two pins already touch. The power-symbol
direction proposal uses its free pins as a baseline.

## Provenance

Drawn by tool calls, not by hand, against KiCad 10.0.8's stock `Device`,
`Connector_Generic` and `power` libraries: `create_project` named `pin_contact`, one
`batch_place_components` call, `add_wire`, coordinate `add_power_symbol` calls,
`connect_to_net` for `SIG_A`, `add_schematic_net_label` for `SIG_B`, `add_junction` and
`batch_add_no_connect`. KiCad 10.0.8 then parsed and force-resaved it:

```text
kicad-cli sch upgrade --force pin_contact.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`. The instance records name project
`pin_contact`, so a test should copy the sheet under that name. KiCad's netlist is the
same before and after the resave apart from its `source` and `date` lines, and ERC reports
the same 17 findings.

## Cases

| Item | Placement | Case |
|---|---|---|
| `J1` `Conn_01x01`, `R1` | J1.1 and R1.1 meet at `(55.88, 50.8)` | contact: connector to resistor |
| `R2`, `#PWR01` `GND` | `GND` on R2.2's tip `(76.2, 54.61)`; `SIG_A` on a stub from R2.1 | contact: ground symbol on a pin |
| `R3`, `#PWR02` `PWR_FLAG` | flag on R3.1's tip `(93.98, 46.99)`, which a 10.16 mm wire also puts on `+5V` (`#PWR05`) | contact: flag on a pin |
| `R3`, `#PWR03` `GND` | 2.54 mm wire from R3.2 | control: power symbol on a stub |
| `J2`, `R4` | 5.08 mm wire J2.1 to R4.1; no-connect on R4.2 | control: wire between pins, no-connect |
| `R5`, `R6`, `R7` | pin 2s joined by wires with a junction at `(91.44, 83.82)`; `+5V` on a 2.54 mm stub from R5.1; `SIG_B` label on R6.1's tip; no-connect on R7.1 | control: junction, stub, label on a pin |
| `R8`, `R9` | 1.27 mm wire R8.2 to R9.1 | short wire between pins |
| `R10`, `R11` | 2.54 mm wire R10.2 to R11.1 | control: the shortest allowed wire |
| `R20`–`R23`, `J3` | unconnected | free pins for placing power symbols and parts on |

## KiCad's own answer

```text
kicad-cli sch export netlist --format kicadsexpr pin_contact.kicad_sch
kicad-cli sch erc --format json --severity-all --units mm pin_contact.kicad_sch
```

KiCad connects all three contacts: `Net-(J1-Pin_1)` holds J1.1 and R1.1, `GND` holds R2.2
and R3.2, and `+5V` holds R3.1 and R5.1. ERC has no finding at any contact. Its 17
findings are `pin_not_connected` on pins left unconnected, `power_pin_not_driven` on the
undriven `GND` net, and `isolated_pin_label` for `SIG_A` and `SIG_B`.
