### `repair_instance_metadata` (minor release)

A new `sch_hierarchy` tool repairs the placed-symbol instance records that make
every write to a sheet fail as `stale_target`
([#128](https://github.com/Copperpour/Copperpour/issues/128)):
`repair_instance_metadata(schematic, dry_run?, expected_plan_revision?)`;
`sch_hierarchy`'s description now names it. It drops a path of the owning
project that its sheet tree does not give the file, a repeat of a path a symbol
already records, and every record under a project name that no candidate
`.kicad_pro` defines; the candidates are the project files in the sheet's
directory and those above it, as for ownership. It adds each path the owning
project gives the file and a symbol does not record, with the reference and
unit of a dropped record at that path, as a renamed project's records have,
else with the symbol's Reference and unit. A record of another project that a
candidate defines, or one with no project or path, is kept, and the repair is
refused. A project in another directory that places the sheet through `../` is
not a candidate, so its records are dropped; the dry run lists them first.

`dry_run` defaults to true and returns `status` (`ready`, `partial`, `noop` or
`blocked`), `project`, `instance_paths`, `plan_revision`, the `drop`, `add`
and `keep` records (each `project`, `path`, `symbols`, and for `drop` and `keep`
a `reason`), `stale_after`, `applied: false` and `source: "file"`.
`stale_after` says why the sheet would still be refused once repaired, or is
null; with it, a plan that changes the sheet is `partial`, one that does not is
`noop`. Apply requires `expected_plan_revision` and returns the same keys with
`status: "applied"` and `applied: true`, or `noop` when there is nothing to
write. It returns `plan_blocked`, naming each kept record, for a plan the dry
run calls `blocked`; `stale_target` when the sheet or its project changed since
that dry run, or when the text it prepared would change more than instance
records; and `conflict` while KiCad holds the sheet or its project's root
schematic open, or when the sheet changed after it was read. A file that is not
the text written is `mutation_outcome_uncertain`. A missing sheet is
`file_not_found`; a sheet with no `(uuid …)`, or of a project that lists another
top-level sheet or gives its root another uuid, `invalid_argument`; one whose
project cannot be told, `conflict`. No existing tool's arguments or responses
change.
