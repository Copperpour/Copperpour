# Symbol units fixture

`pcb_sync_symbol_units_kicad10.net` is a netlist of the kind
`update_pcb_from_schematic` reads, exported by KiCad 10.0.8 from a schematic
with a three-unit symbol and a one-unit symbol. Each component carries the
`(units …)` node KiCad's Update PCB copies onto its footprint.

## Provenance

The schematic was built through Copperpour's `tools/call`, against KiCad
10.0.8's stock libraries:

```text
create_project           name symbol_units
add_schematic_component  Amplifier_Operational:LM358, reference U1, unit 1, footprint Package_SO:SOIC-8_3.9x4.9mm_P1.27mm
add_schematic_component  Amplifier_Operational:LM358, reference U1, unit 2, footprint Package_SO:SOIC-8_3.9x4.9mm_P1.27mm
add_schematic_component  Amplifier_Operational:LM358, reference U1, unit 3, footprint Package_SO:SOIC-8_3.9x4.9mm_P1.27mm
add_schematic_component  Device:R, reference R1, value 10k, footprint Resistor_SMD:R_0603_1608Metric
```

KiCad 10.0.8 then force-resaved the schematic and exported the netlist:

```text
kicad-cli sch upgrade --force symbol_units.kicad_sch
kicad-cli sch export netlist --format kicadsexpr -o pcb_sync_symbol_units_kicad10.net symbol_units.kicad_sch
```

The schematic is not committed: nothing reads it, and the calls above rebuild
it.

Three sections of the export are removed: `(design …)`, which names the
exporting machine's path and the export time, `(libparts …)` and
`(libraries …)`. What remains is KiCad's output unchanged: `(version …)`,
`(components …)`, `(groups)`, `(variants)` and `(nets …)`.

## Cases

| Component | Units, with their pins in the order KiCad writes them |
|---|---|
| `R1`, `Device:R` | `A`: 1, 2 |
| `U1`, `Amplifier_Operational:LM358` | `A`: 3, 2, 1; `B`: 5, 6, 7; `C`: 8, 4 |

KiCad writes a unit for every unit of the library symbol, under the name it
displays for the unit, with its pins sorted by position (`LIB_SYMBOL::GetUnitPinInfo`,
`eeschema/lib_symbol.cpp:1222`, KiCad 10.0.8). A one-unit symbol has one unit,
`A`. Update PCB compares the list with the footprint's, in order, and sets it
where they differ (`BOARD_NETLIST_UPDATER::updateComponentUnits`,
`pcbnew/netlist_reader/board_netlist_updater.cpp:1263-1321`).
