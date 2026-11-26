# Hidden pin-tip contacts

Built with Copperpour's served `create_project`, `edit_symbol_pin`,
`register_symbol_library`, `add_schematic_component`, and `add_power_symbol`
tools, then saved by KiCad 10.0.8 with `kicad-cli sch upgrade --force`.

The project used separate copies of KiCad's stock `power` and `Device` library
files. The GND copy hides pin 1. Two copies of `Device:R` hide pin 1, one
leaving it passive and one changing its type to `no_connect`. Only the schematic
is needed by the Copperpour test because it contains the placed pin definitions.

- `#PWR01.1` touches `R30.1` at `(124.46, 137.16)`.
- `R1.1` is a hidden passive pin and touches `R2.1` at `(55.88, 50.8)`.
- `R3.1` is a hidden `no_connect` pin and touches `R4.1` at `(55.88, 76.2)`.
