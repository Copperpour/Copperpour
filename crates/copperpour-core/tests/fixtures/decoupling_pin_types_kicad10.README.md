# Decoupling pin-type fixture

`decoupling_pin_types_kicad10.kicad_sch` gives `audit_decoupling` two parts,
`U1` and `U2`, whose pins cover each way a pin is or is not a supply input to
decouple (#120), beside `C1`, 100 nF from `+3V3` to `GND`.

## Provenance

Built through Copperpour with `create_project` (project
`decoupling_pin_types`), then `create_symbol` of `Module` and of `Gate` in a
scratch library `Fixture.kicad_sym` with the pins below (`Gate`'s pins 3 and 4
with `hide: true`), `register_symbol_library` of it in the project's table as
`Fixture`, `add_schematic_component` of `U1` (`Fixture:Module`), `C1` (KiCad
10.0.6's stock `Device:C`) and `U2` (`Fixture:Gate`), `add_power_symbol` seven
times (stock `power:+3V3`, `power:GND` and `power:GNDA`, each on a pin),
`connect_to_net` for the labels `EN` and `3V3_OUT`, and `add_no_connect` on U1
pin 5 and U2 pin 2. The sheet was then force-resaved by KiCad 10.0.6 beside the
project `create_project` wrote, and renamed:

```text
kicad-cli sch upgrade --force decoupling_pin_types.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`. The project file and the
scratch library are not; the sheet embeds both symbols.

## Cases

| Pin | Type | On | Audited |
|---|---|---|---|
| U1 1 `IOVDD` | power_in | nothing | yes, no capacitor |
| U1 2 `VREG_IN` | power_in | `+3V3` | yes, C1 |
| U1 3 `VCC` | unspecified | `+3V3` | yes by name, C1 |
| U1 4 `3V3_EN` | bidirectional | label `EN` | no: typed, not `power_in` |
| U1 5 `VBUS` | power_in | a no-connect flag | no: flagged |
| U1 6 `3V3` | power_out | label `3V3_OUT` | no: typed, not `power_in` |
| U1 7 `GND` | power_in | `GND` | no: a ground pin |
| U1 8 `VSS` | power_in | nothing | no: a ground name |
| U1 9 `EP` | power_in | `GNDA`, no capacitor | no: a ground net |
| U2 1 `VDD` | passive | `+3V3` | yes by name, C1 |
| U2 2 `VCC` | passive | a no-connect flag | no: flagged |
| U2 3 `+3V3` | power_in, hidden | nothing drawn; its name's net | yes, C1 |
| U2 4 `GND` | power_in, hidden | nothing drawn; its name's net | no: a ground pin |

The seven `#PWR` symbols' pins are `power_in` and are not audited.

## KiCad's own answer

```text
kicad-cli sch export netlist --output netlist.net decoupling_pin_types.kicad_sch
kicad-cli sch erc --format json --severity-all --output erc.json decoupling_pin_types.kicad_sch
```

KiCad 10.0.6's netlist puts C1.1, U1.2, U1.3, U2.1 and the hidden U2.3 on
`+3V3`; C1.2, U1.7 and the hidden U2.4 on `GND`; U1.9 alone on `GNDA`; U1.4 on
`/EN` and U1.6 on `/3V3_OUT`; and U1.1, U1.5, U1.8 and U2.2 each on an
`unconnected-(…)` net, the flagged U1.5 and U2.2 with pin type `…+no_connect`.

Its ERC reports `pin_not_connected` and `power_pin_not_driven` for U1 pins 1 and
8, `power_pin_not_driven` for `#PWR01`, `#PWR02` and `#PWR06` (no driver on
those nets), `ground_pin_not_ground` for U1 pin 8 `VSS`, `pin_to_pin` for U1
pin 3 against `#PWR04`, and `isolated_pin_label` for `EN` and `3V3_OUT`. It
names nothing on U2, and nothing for either flagged pin.
