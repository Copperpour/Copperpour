# Top-level sheets fixture

The project #130 reproduces: `toplevel.kicad_sch` places `R1` and the sheet
`Child` (`child.kicad_sch`, which places `R2`), and `second.kicad_sch` is blank.
The tests give `toplevel.kicad_pro` its `schematic.top_level_sheets`, which no
tool writes, listing `second.kicad_sch` beside `toplevel.kicad_sch` with nil
uuids or with uuids of their own.

## Provenance

Built through Copperpour 0.15.0 with `create_project`, `add_schematic_component`
(KiCad 10.0.6's stock `Device:R`, as `../reference_prefix_kicad10/` holds it),
`add_hierarchical_sheet` and `create_schematic`. Then each sheet was parsed and
force-resaved by KiCad 10.0.6: `toplevel.kicad_sch` beside the project file, so
its symbols keep the project's name, and the other two each in a directory of
its own:

```text
kicad-cli sch upgrade --force toplevel.kicad_sch
kicad-cli sch upgrade --force child.kicad_sch
kicad-cli sch upgrade --force second.kicad_sch
```

What is committed is that Eeschema serialization, `(generator "eeschema")` and
`(version 20260306)`; `toplevel.kicad_pro` is the one `create_project` wrote.
KiCad gave each pin a uuid and wrote `second.kicad_sch` without sheet instances.
A second `upgrade --force` changes no byte.

## Identities

| Item | uuid |
|---|---|
| `toplevel.kicad_sch` | `b603aa20-aac9-4b43-91f9-885f100bae62` |
| sheet `Child` | `4e754337-e3f1-4d84-85ed-5ff5976e9393` |
| `second.kicad_sch` | `611adf86-3690-4960-a96b-31b4bdfb37a4` |

`R1` records `/b603aa20-…` and `R2` `/b603aa20-…/4e754337-…`, both under the
project `toplevel`.

## Oracle

What KiCad 10.0.6 does with the list:

- eeschema loads each entry as a top-level sheet, and keys it to the entry's
  uuid unless that is nil (`eeschema/files-io.cpp:290-331`);
- a sheet path starts at its top-level sheet's uuid, the nil virtual root left
  out (`eeschema/sch_sheet_path.cpp:474-483`);
- connectivity is one sheet list built from every top-level sheet
  (`eeschema/schematic.cpp:1726-1767`, `:2129-2150`);
- eeschema locks the schematic it opens, and takes the project from that
  file's name (`eeschema/files-io.cpp:116-149`).

kicad-cli 10.0.6 loads only the file it is handed (`EESCHEMA_HELPERS::LoadSchematic`),
so no headless KiCad tool of that version reads a second top-level sheet;
KiCad's `60b2d317`, in 10.0.7, loads them all. Its QA project
`qa/data/eeschema/issue25198/` lists `toplevel2.kicad_sch` under a uuid other
than the file's own, as eeschema writes a sheet added beside the first.
