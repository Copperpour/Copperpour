### `set_erc_severity` (minor release)

A new `sch_export` tool sets the severity of ERC checks in a project file
([#231](https://github.com/Copperpour/Copperpour/issues/231)):
`set_erc_severity(project, severities)`. `project` is the `.kicad_pro`;
`severities` maps a check, by its key under `erc.rule_severities` (for
example `single_global_label`), to `error`, `warning` or `ignore`. A check is
one KiCad 10.0.6 has or one the file already names, as a later KiCad writes
the checks it adds. For `pin_to_pin`, `error` and `warning` both turn the check
on, and KiCad's pin map sets each conflict's severity. Only the named keys are
written, through the project file's revision check, and a project with no
`erc` section gets one holding only them. Any other check or severity, an
empty map, a path that is not a `.kicad_pro`, and a project KiCad holds (its
`~<project>.kicad_pro.lck`) are refused, and nothing is written.

The response gives `file`, `changed` (false when the file already held every
severity named, and then nothing is written) and `severities`, each check's
`previous` and `current`, `previous` null where the file did not name the
check. `run_erc` reports by the new severities from its next run. No existing
tool's arguments or responses change. `sch_export` rises from 10 to 11 tools.
