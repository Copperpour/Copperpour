### Schematic writes follow a project's `top_level_sheets` (patch release)

A project's top-level sheets are the files its `.kicad_pro` lists in
`schematic.top_level_sheets`, read as eeschema 10 reads them; a project that
lists none still has `<project>.kicad_sch` alone. A write to another listed
sheet, or to any sheet under one, used to be refused as `conflict`; it now
works ([#130](https://github.com/Copperpour/Copperpour/issues/130)). Each
top-level sheet's instance paths start at the uuid its entry gives unless that
is nil, so a project that gives its sheet another uuid is no longer refused as
`stale_target` ([#125](https://github.com/Copperpour/Copperpour/issues/125)).
A file placed under two top-level sheets keeps the paths each gives it. A file
that a project's list leaves out is no root of it, as eeschema does not load
it, so a write that took `<project>.kicad_sch` for the root although the list
leaves it out is now refused as `conflict`.

On a project that lists more than one top-level sheet,
`rename_schematic_net_label` and `annotate_schematic` with `schematic_scope`
`file` are refused as `invalid_argument`, as `schematic_scope` `hierarchy`
already was: each reads one top-level sheet's hierarchy as the whole design.
Hierarchy-scope annotation still refuses a project that gives its sheet another
uuid, and reads the list as above, so a project file it cannot parse no longer
stops it. `run_erc` on a sheet under a top-level sheet other than
`<project>.kicad_sch` is refused as `unsupported_capability`. The tools that
check the editor lock on a project's root as well as on the files they write
(`edit_schematic_text`, `delete_schematic_text`, `set_schematic_title_block`,
`rename_schematic_net_label`, and `annotate_schematic` over a hierarchy) check
the one beside `<project>.kicad_sch`, the file eeschema opens and locks.
