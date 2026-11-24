### `update_pcb_from_schematic` names the footprints without a symbol and deletes them on request (minor release)

A board footprint that no schematic symbol matches is still kept and counted in
`coverage.board_only_preserved`. The dry run and the apply now also list those
without the board-only attribute (KiCad's "Not in schematic") under
`footprints_without_symbols`, each with its `reference`, `kiid`,
`footprint_id` and `locked`. Callers that read the response need no change.

The new argument `delete_footprints_without_symbols` (default `false`)
deletes those footprints in the sync's commit, as KiCad's **Delete footprints
with no symbols** does:

- Each deletion is a change with `kind: "delete"` (`kiid`, `reference`,
  `footprint_id`), part of the plan and its revision, and counted in the new
  `coverage.footprints_deleted`. A plan with only deletions is `ready`, not
  `noop`. Deleted footprints are no longer counted in `board_only_preserved`.
- A footprint with the board-only attribute is never deleted.
- A locked footprint is kept and listed, with a `footprint_locked_not_deleted`
  note that refuses nothing.
- A plan with a conflict deletes nothing.
- The apply reads the board back and reports a footprint KiCad did not delete
  as `board_readback_differs`; it is counted in `board_only_preserved.applied`
  rather than `footprints_deleted.applied` and listed in
  `footprints_without_symbols`.

Pass the same value to the dry run and the apply. An apply that sets it
without the dry run having set it is refused as `stale_plan_revision` when the
plan would delete something.

A readback after the commit that cannot finish, of the footprints the sync
sent or of those it deleted, no longer fails the call with a handler error:
the commit stands, so the apply answers `applied` with an
`apply_readback_unchecked` note for each, and the result is an error. Do not
apply the plan again; check the board instead.
