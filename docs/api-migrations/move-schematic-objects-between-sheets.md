### `move_schematic_objects_between_sheets` moves objects between sheets (minor release)

`move_schematic_objects_between_sheets` is a new tool in `sch_hierarchy`. It moves
UUID-selected symbols, wires, junctions, local and global labels, no-connect flags, text
and text boxes from one sheet of a saved project to another, offset by `offset_x_mm` and
`offset_y_mm`. Each sheet must be placed only once. Symbol and pin UUIDs, references, unit
membership, properties and footprint assignments don't change. Instance paths change to
the destination sheet.

- It defaults to a dry run. To apply, pass the dry run's `plan_revision` as
  `expected_plan_revision`. A missing revision is `invalid_argument`. Apply rebuilds and
  checks the plan again, so failed validation can return `plan_blocked`. If the plan
  revision no longer matches, including because the verification result changed, apply
  returns `stale_target`. None of these writes anything.
- Without `kicad-cli`, a dry run returns the plan with `verification.status: "not_run"`
  and apply refuses with `plan_blocked`. A check that can't be read is a refusal, not a
  pass.
- The two sheets may sit under different top-level schematics of one project. A sheet path
  is the top-level instance UUID followed by child sheet-symbol UUIDs; use the
  project-declared top-level UUID. A lone top-level sheet whose declared UUID differs from
  its file's UUID is refused. Projects with several top-level schematics need KiCad 10.0.7
  or newer.
- It doesn't write the PCB. It refuses a move that would leave a footprint path stale,
  reuse a path or net name the PCB already has, change an effective netclass or inherited
  simulation exclusion, or need dependent net names edited. A net rename is also refused
  when the project has a netclass pattern other than `*`, because Copperpour can't prove
  the patterns still match, or custom PCB rules, because the schematic checks don't
  evaluate them.
- It checks every project file it read, and that files it expects to be missing still are,
  before it writes and again during recovery.
- A failure before the journal is written is a refusal and changes nothing:
  `invalid_argument`, `stale_target`, `unsafe_file_fallback`, `conflict` or
  `plan_blocked`. An inspected older unfinished journal is a `conflict` that names it. A
  failure after that is `mutation_outcome_uncertain`. It names the journal and, when
  available, the file where the write stopped, or, if the move finished but reading it
  back didn't match, the transaction ID. Run `copperpour transaction status` on the
  project directory, and `copperpour transaction recover` only if a journal remains.
  Recovery checks that each sheet matches its contents before or after the move, writes
  only sheets still in their before state, and requires the other read files unchanged,
  expected-missing files still missing and no KiCad editor lock present.
- Its journals record files that must stay missing, so recovering one needs a Copperpour
  version that reads version 2 journals. Recover any pending journal before downgrading.

See [cross-sheet transfer][move-objects-guide] for the refusals and the recovery steps.

[move-objects-guide]:
  https://github.com/Copperpour/Copperpour/blob/main/docs/CROSS_SHEET_TRANSFER.md
