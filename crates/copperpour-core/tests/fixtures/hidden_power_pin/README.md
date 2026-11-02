# Hidden power pin fixture

`single_pin_nets.kicad_sch` here is the root of the single-pin-net pair
(`../single_pin_nets.kicad_sch`) with one symbol added: `U99`, whose pin 1 is a
hidden `power_in` pin named `HIDDEN`, at (200.66, 50.8), away from every wire.
KiCad makes such a pin, on a symbol that is not a power symbol, a global power
net of the pin's name (`SCH_PIN::IsGlobalPower`, `eeschema/sch_pin.cpp`). The
pair's child, `../single_pin_nets_child.kicad_sch`, is used as it is.

## Provenance

The symbol was made with `create_symbol` in a scratch library `Hidden` (pin 1
`power_in` with `hide: true`, pin 2 `passive`), placed with
`add_schematic_component` as `U99`, and the root force-resaved by KiCad 10.0.6
with `single_pin_nets.kicad_pro` beside it:

```text
kicad-cli sch upgrade --force single_pin_nets.kicad_sch
```

The `Hidden:R_HIDDEN` library record and the `U99` instance are KiCad's
serialization; the rest of the file is byte-identical to the pair's root.

## KiCad's own answer

```text
kicad-cli sch export netlist --output single_pin_nets.net single_pin_nets.kicad_sch
```

The pair's nets are unchanged, and two are added:

| Net | Pins |
|---|---|
| `HIDDEN` | U99.1 |
| `unconnected-(U99-~-Pad2)` | U99.2 |
