### `update_pcb_from_schematic` puts its verdict first and bounds its change list (minor release)

The sync's response listed every planned change with every pad-to-net pair,
and its keys were sorted, so `status` and `plan_revision` came after the whole
list. For 340 footprints the dry run was 166 KB, more than a client would take,
and the plan could not be applied from the chat session (#69).

The top-level keys now come in this order: `status`, `plan_revision`,
`coverage`, `changes_count`, `changes_shown`, `changes_truncated`, `staging`,
`undo`, `diagnostics`, `unassigned_footprints`, then `changes`. `changes` is
still an array, now of the first `limit` planned changes, 50 unless the call
names another. `changes_count` is how many the plan has, and
`changes_shown` and `changes_truncated` say how many are listed, as `run_drc`
reports `shown` and `truncated` beside its violations. Only the response is
bounded: `plan_revision` covers every change, and an apply sends every change,
whatever `limit` the dry run used. `diagnostics` and `unassigned_footprints`
are listed whole. A caller that needs every change passes a larger `limit` or
an `output`.

`output` names a file to receive the whole response, every change listed, as
`run_drc` writes its report. It is written with every response that carries a
plan: ready, noop, conflict, a stale revision and applied. A refusal before a
plan exists, or an IPC failure, writes no file and leaves an earlier one in
place. A path that is the board or a file of the saved hierarchy, a directory,
or under something that is not a directory is refused before the board is
read. A write that fails after that check fails the call, as `run_drc` does:
with nothing applied the call can be repeated, and after an apply the error
carries the applied response with an `output_not_written` diagnostic, so the
commit is not repeated. `run_drc` and `get_drc_violations` share the same check
and writer, unchanged.
