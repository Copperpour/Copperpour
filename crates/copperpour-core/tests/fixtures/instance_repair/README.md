# Instance repair fixtures

Four projects whose `leaf.kicad_sch` carries instance records that make every
write to it fail as `stale_target`, for `repair_instance_metadata` (#128).

## Provenance

Built through Copperpour with `create_project`, `add_schematic_component`
(KiCad 10.0.6's stock `Device:R`), `add_hierarchical_sheet` and
`annotate_schematic`, copying and renaming files as a user moves a sheet into
another project or renames one. Every `.kicad_sch` was then parsed and
force-resaved by KiCad 10.0.6, one file per call:

```text
kicad-cli sch upgrade --force <file>.kicad_sch
```

What is committed is that Eeschema serialization, `(version 20260306)`.
`kicad-cli` loads the project file named after the sheet when one sits beside
it, and otherwise a default project with an empty name. It saves the records
whose path starts at the sheet's own uuid under that project's name, and every
other record under its own (`eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr.cpp:907-940`
at 10.0.6). Copperpour writes no pin uuids and each resave gave the pins new
ones, so the leaves of `dead` and `shared`, resaved apart, differ there. The
`.kicad_pro` files are those `create_project` wrote.

## Cases

| Case | `leaf.kicad_sch`'s records | Role |
|---|---|---|
| `dead` | R1, R2: `leaf` at the leaf's own uuid; `owner` at both placements of the sheet | a dead record of a project the leaf was once the top-level sheet of |
| `shared` | R1, R2: `""` at the leaf's own uuid; `owner` at both placements; `other` at `/e7c1f0eb…/514ce7ab…` | records of projects no candidate project file defines: `kicad-cli`'s empty name, and a project in a sibling directory |
| `nested` | R1: `mid` at `/10e35b97…/75a3c0c9…` only | a dead record whose path starts at `mid.kicad_sch`, a sheet of `owner`, and no record of `owner` |
| `renamed` | R1/R3 and R2/R4: `old` at both placements of the sheet | a project renamed `new` by renaming its files, its records still under `old` |

`dead`: a project `scratch` placed R1 and R2 on its root; that root was copied
into `owner` as `leaf.kicad_sch` and placed twice. The leaf was resaved with
the project file of a project `create_project` made named `leaf` beside it,
then that file was removed, so the record reads `(project "leaf")`, as Konnect
wrote a child's own stem before mixelpixx/Konnect#389.

`shared`: the same leaf, resaved alone, so its record reads `(project "")`. A
copy was placed in a project `other` in a sibling directory and copied back.
`add_hierarchical_sheet` refuses a `sheet_file` that leaves the parent's
directory, so the copy stands in for a sheet `other` places as
`../owner/leaf.kicad_sch`; the leaf's records are the same either way.

`nested`: a project `mid` placed `leaf.kicad_sch` and R1 on it; `mid.kicad_sch`
and the leaf were copied into `owner` and `mid.kicad_sch` placed on its root.

`renamed`: a project `old` placed `leaf.kicad_sch` twice, R1 and R2 on it, and
`annotate_schematic` over its hierarchy numbered them R1 and R2 at one
placement and R3 and R4 at the other. `old.kicad_pro` and `old.kicad_sch` were
then renamed `new.kicad_pro` and `new.kicad_sch`, as a user renames a project,
which leaves `"filename": "old.kicad_pro"` in the project file's `meta`. The
root's resave beside `new.kicad_pro` renamed its own records `new`; the leaf's
keep `old`.
