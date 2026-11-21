# rename_footprint fixtures

Four versions of one footprint, each written by KiCad, used to check that
`rename_footprint` renames a footprint exactly as KiCad's footprint editor
does.

The footprint is
`Connector_FFC-FPC.pretty/Hirose_FH12-24S-0.5SH_1x24-1MP_P0.50mm_Horizontal`
from the KiCad 10.0.8 standard library (format 20260206). Its Value equals
its name.

## Provenance

All four files were written by KiCad 10.0.8's own footprint writer on Windows
11, through its Python bindings (`pcbnew.PCB_IO_KICAD_SEXPR`), with
`FootprintLoad(library, name, True)` and `FootprintSave`. The rename does what
`FOOTPRINT_EDITOR_CONTROL::RenameFootprint`
(`pcbnew/tools/footprint_editor_control.cpp`, 10.0.8) does to the footprint:
`SetFPID` to the new name, and `SetValue( newName )` when the Value equals the
old name. Nothing was edited by hand.

| Fixture | Written from | Change made in KiCad | SHA-256 (first 16) |
|---|---|---|---|
| `rename_footprint_source_kicad10.kicad_mod` | the library file | none (load and save) | `65faa8419f7a1128` |
| `rename_footprint_renamed_kicad10.kicad_mod` | `…_source_kicad10` | renamed to `FFC_Hirose_FH12-24S_Renamed` | `d615b77b069c7414` |
| `rename_footprint_kept_source_kicad10.kicad_mod` | `…_source_kicad10` | Value set to `FH12-24S` | `172a598dcf1e3fce` |
| `rename_footprint_kept_kicad10.kicad_mod` | `…_kept_source_kicad10` | renamed to `FFC_Hirose_FH12-24S_Renamed` | `adab432ed7d7d22a` |

`set_footprint_metadata`'s `value` test (#713) uses the first and third: the
third is the first after KiCad's `SetValue( "FH12-24S" )`, so setting that Value
must give it back, UUIDs masked.

## What they show

- A rename changes the name in the footprint's header, and the Value when it
  equals the old name; with another Value, only the header changes.
- Nothing else changes, not even the 3D model path that holds the old name.
- KiCad gives the fields new UUIDs on each save, so the tests compare files
  with their UUIDs masked.
