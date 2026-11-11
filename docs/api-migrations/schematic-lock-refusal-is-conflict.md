### Placement, annotation and templates refuse a locked schematic as `conflict` (patch release)

`add_schematic_component`, `batch_place_components`, `annotate_schematic` and
`apply_template` reported a write that never happened as
`mutation_outcome_uncertain`, with `retry.safe: false`
([#427](https://github.com/Copperpour/Copperpour/issues/427)). That covered
KiCad's `~<name>.kicad_sch.lck` editor lock, and a schematic saved by another
program after the tool read it. Both are checked before the file is replaced
(for `annotate_schematic` over a hierarchy, before the transaction's journal is
written), so the files are unchanged.

They now return `conflict`, with `paths` holding the schematic, and write
nothing. For the lock, the message names the lock file: save and close the
schematic in Eeschema, then retry; a lock file that could not be inspected is
reported as that, with the reason. For a changed file, reload it and retry.
Where the tool returns an `outcome`, it is `status: "failed"` with
`retry.safe: true`. A readback that does not match after the replacement is
still `mutation_outcome_uncertain`.
