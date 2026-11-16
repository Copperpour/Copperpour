### `run_drc` names the checks a run ignored (minor release)

A DRC check whose severity is `ignore` is not run, so it never appears among
`run_drc`'s violations or counts, and a clean summary could not be told from a
board whose check was switched off
([#519](https://github.com/Copperpour/Copperpour/issues/519)). KiCad lists
those checks in its own DRC report; `run_drc`'s summary now gives their keys
as `ignored_checks`, in KiCad's order, for example
`["missing_courtyard", "footprint_filters_mismatch"]`. It is `null` when the
report carries no such list, which is not the same as `[]`, nothing ignored.

Read a count of zero beside `ignored_checks`. `set_drc_severity` turns an
ignored check back on. Nothing `run_drc` returned before changes.
