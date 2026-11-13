# Sheet fields fixture

`sheet_fields_kicad10.net` is a netlist of the kind `update_pcb_from_schematic`
reads, exported by KiCad 10.0.6 from a schematic that places one sheet file
twice. Each component carries what KiCad's Update PCB gives its footprint as
sheet name, sheet file and footprint filters.

## Provenance

The schematic was built through Copperpour's `tools/call`, against KiCad
10.0.6's stock `Device` library registered in the project's own table:

```text
create_project           name sheet_fields
register_symbol_library  Device.kicad_sym as Device, scope project
add_hierarchical_sheet   sheet_fields.kicad_sch: sheet_file channel.kicad_sch, sheet_name CH1
add_hierarchical_sheet   sheet_fields.kicad_sch: sheet_file channel.kicad_sch, sheet_name CH2
add_schematic_component  channel.kicad_sch: Device:C, value 100n, footprint Capacitor_SMD:C_0603_1608Metric
add_schematic_component  sheet_fields.kicad_sch: Device:C, value 1u, footprint Capacitor_SMD:C_0603_1608Metric
annotate_schematic       sheet_fields.kicad_sch, schematic_scope hierarchy
```

KiCad 10.0.6 then force-resaved both files, one at a time since `upgrade`
converts only the file it is given, and exported the netlist:

```text
kicad-cli sch upgrade --force sheet_fields.kicad_sch
kicad-cli sch upgrade --force channel.kicad_sch
kicad-cli sch export netlist --format kicadsexpr -o sheet_fields_kicad10.net sheet_fields.kicad_sch
```

The schematic is not committed: nothing reads it, and the calls above rebuild
it.

Three sections of the export are removed: `(design …)`, which names the
exporting machine's path and the export time, `(libparts …)` and
`(libraries …)`. What remains is KiCad's output unchanged: `(version …)`,
`(components …)`, `(groups)`, `(variants)` and `(nets …)`.

## Cases

| Component | `sheetpath` names | `Sheetname` | `Sheetfile` | `ki_fp_filters` |
|---|---|---|---|---|
| `C1`, `1u`, on the root | `/` | `sheet_fields` | `sheet_fields.kicad_sch` | `C_*` |
| `C2`, `100n`, on `CH1` | `/CH1/` | `CH1` | `channel.kicad_sch` | `C_*` |
| `C3`, `100n`, on `CH2` | `/CH2/` | `CH2` | `channel.kicad_sch` | `C_*` |

KiCad's Update PCB gives a footprint the sheet path's names as its sheet name,
the `Sheetname` property only when the names are empty, then the `Sheetfile`
property and the `ki_fp_filters` property
(`pcbnew/netlist_reader/board_netlist_updater.cpp:659-674`, KiCad 10.0.6). So
`C1`'s sheet name is `/`, not its `Sheetname`, `sheet_fields`. `C2` and `C3` are one symbol
in one file, with one `tstamps`, told apart by their sheet paths.
