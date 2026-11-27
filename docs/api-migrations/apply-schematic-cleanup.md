### Add `apply_schematic_cleanup` (minor release)

Preview one saved sheet with `plan_schematic_cleanup` from `sch_readability`. Load
`sch_batch` and pass `apply_schematic_cleanup` the same `schematic`, region and
`movable` arguments plus the preview's `revision`. Do not send a plan object.

The tool replans from the saved sheet, checks connectivity and writes at most once.
It checks the saved anchors, wires, junctions and pin nets, and verifies that
the saved file matches the written bytes.
It returns the applied `steps` in preview format, with wire and junction UUIDs, and
the saved `revision`. An empty plan returns `unchanged: true` and does not write.

On `conflict`, close any editor holding the sheet, then preview the current file
before applying again. `plan_blocked` lists the checks that failed before writing.
On `mutation_outcome_uncertain`, inspect the saved sheet before retrying: the file
may have changed.

Use a regular schematic file, not a symbolic link or directory.
Copperpour's lock coordinates its own writers. Close the editor and stop
other writers before applying: a process that ignores the lock can still
save between the last check and the rename.
