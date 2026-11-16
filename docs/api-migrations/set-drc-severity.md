### `set_drc_severity` (minor release)

A new `verification` tool sets the severity of DRC checks in a project file
([#519](https://github.com/Copperpour/Copperpour/issues/519)), the sibling of
`set_erc_severity`: `set_drc_severity(project, severities)`. `project` is the
`.kicad_pro`; `severities` maps a check, by its key under
`board.design_settings.rule_severities` (for example `silk_edge_clearance`), to
`error`, `warning` or `ignore`. A check is one KiCad 10.0.6 has or one the file
already names, as a later KiCad writes the checks it adds. Only the named keys
are written, through the project file's revision check, and a project with no
`rule_severities` gets one holding only them. Any other check (KiCad's internal
ones and KiCad 8's `hole_near_hole` included) or severity, an empty map, a path
that is not a `.kicad_pro`, and a project KiCad holds (its
`~<project>.kicad_pro.lck`) are refused, and nothing is written.

The response gives `file`, `changed` (false when the file already held every
severity named, and then nothing is written) and `severities`, each check's
`previous` and `current`, `previous` null where the file did not name the
check. `run_drc` reports by the new severities from its next run, and a check
set to `ignore` is not run at all. No existing tool's arguments or responses
change. `verification` rises from 12 to 13 tools.
