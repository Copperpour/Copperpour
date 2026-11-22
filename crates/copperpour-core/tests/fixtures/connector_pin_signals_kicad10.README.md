# Connector pin signal fixture

`connector_pin_signals_kicad10.kicad_sch` holds three connectors whose pins are each drawn
one way a reader can or cannot tell the signal on them, for the proposed check that
reports which connector pins a schematic leaves unidentified.

## Provenance

Drawn by tool calls, not by hand, against KiCad 10.0.8's stock `Connector_Generic`,
`Connector`, `Device` and `power` libraries, using `create_project`,
`batch_place_components`, `move_schematic_component`, `connect_to_net`, `batch_add_wire`,
`batch_add_no_connect`, `add_power_symbol` and `add_schematic_net_label`, then resaved
with `kicad-cli sch upgrade --force` (KiCad 10.0.8). It carries `(generator "eeschema")`
and `(version 20260306)`. The resave kept every item, UUID, coordinate and property; it
added KiCad's default fields and pin UUIDs, and wrote the symbol instances under the
project name `""`, because the project file `create_project` wrote (only defaults) is not
committed. The netlist and ERC below were taken before the resave and are unchanged by it,
and the plotted drawing is the same.

## Cases

| Pin | Drawing | Pin name, as drawn |
|---|---|---|
| J1.1 (`Conn_01x06`) | 5.08 mm stub with a `SIG_A` net label | `Pin_1`, hidden by the symbol's `pin_names (hide yes)` |
| J1.2 | wire to R1.2, no label; R1.1 on `+3V3` | `Pin_2`, hidden |
| J1.3 | no-connect flag | `Pin_3`, hidden |
| J1.4 | 10.16 mm stub to a `GND` power symbol | `Pin_4`, hidden |
| J1.5 | 71.12 mm wire and an 11.43 mm drop to R2.1, `SIG_B` net label 66.04 mm from the pin; R2.2 on `GND` | `Pin_5`, hidden |
| J1.6 | nothing | `Pin_6`, hidden |
| J2.4 (`Conn_ARM_SWD_TagConnect_TC2030-NL`) | wire to J3.1, no label | `SWCLK`, drawn |
| J2.2 | wire to J3.2 with a `SWDIO` net label | `SWDIO`, drawn |
| J2.1, J2.5 | `+3V3` and `GND` power symbols on the pins, each with a `PWR_FLAG` on a stub | `VCC`, `GND`, drawn |
| J2.3, J2.6 | no-connect flags | `~{RESET}`, `SWO`, drawn |
| J3.1 (`Conn_01x02`) | the `SWCLK` wire, no label | `Pin_1`, hidden |
| J3.2 | the `SWDIO` wire and label | `Pin_2`, hidden |

## KiCad's own answer

```text
kicad-cli sch export netlist --output connector_pin_signals_kicad10.net connector_pin_signals_kicad10.kicad_sch
```

| Net | Pins KiCad lists |
|---|---|
| `/SIG_A` | J1.1 |
| `Net-(J1-Pin_2)` | J1.2 R1.2 |
| `unconnected-(J1-Pin_3-Pad3)` | J1.3 |
| `GND` | J1.4 J2.5 R2.2 |
| `/SIG_B` | J1.5 R2.1 |
| `unconnected-(J1-Pin_6-Pad6)` | J1.6 |
| `+3V3` | J2.1 R1.1 |
| `/SWDIO` | J2.2 J3.2 |
| `Net-(J2-SWCLK)` | J2.4 J3.1 |
| `unconnected-(J2-~{RESET}-Pad3)` | J2.3 |
| `unconnected-(J2-SWO-Pad6)` | J2.6 |

The netlist names J1.2's net after the hidden name `Pin_2`, and gives the flagged J1.3 and
the bare J1.6 the same `unconnected-(…)` form.

## ERC

`kicad-cli sch erc --severity-all` (10.0.8) finds 2 violations: `pin_not_connected` on J1
pin 6 (error) and `isolated_pin_label` on `SIG_A` (warning), since that net reaches one
pin. The flagged J1.3 is not reported.
