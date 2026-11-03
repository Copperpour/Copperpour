### `apply_copper` (minor release)

A new tool, in a new `pcb_copper` toolset (`load_toolset('pcb_copper')`),
creates a batch of straight tracks and through vias on the board KiCad holds
open
([#95](https://github.com/Copperpour/Copperpour/issues/95)):
`apply_copper(board, tracks?, vias?, dry_run?, expected_plan_revision?)`. Each
track is `{start, end, width, layer, net}` and each via
`{position, drill, size, net}`, points as `{x, y}` in mm. It needs KiCad
running with the board open, and it never saves.

`dry_run` defaults to true: it checks every item and returns the normalized
plan, every length rounded to the nanometre as KiCad rounds it, each layer by
its canonical name and each net with its `net_code`, with `track_count`,
`via_count` and a `plan_revision`. The revision covers the plan, the board
KiCad named, its net table and its enabled layers, so a net renamed, added or
removed since the dry run makes an apply `stale_target`. An apply takes
`dry_run: false` and that revision as `expected_plan_revision`; without it the
call is `invalid_argument` and nothing is sent.

Before anything is written it refuses with `invalid_argument`, naming the
field, as `tracks[2].net`, and listing every fault in the message: a net the
board does not have, or an empty net (copper is never put on net 0); a layer
that is not copper, or that the board does not have; a width, drill or size
that is not above zero once rounded; a drill not smaller than its via's size;
a track whose ends round to the same point; a coordinate beyond the
±2147.483637 mm KiCad keeps; and a plan with no tracks and no vias.

An apply is `BeginCommit`, `CreateItems` and `EndCommit`: one commit, one undo
step. It is not an atomic transaction. KiCad answers `CreateItems` per item,
and its answer names the requested net even for an item it then stores on the
net of the copper the item touches (an end on a pad, a via on a track). So
after the commit every created item is read back by UUID, and the response
(`tracks` and `vias` with each item's `uuid`, geometry, layer and `net`, the
counts, `source: "ipc_readback"`) comes from that read-back:

- If KiCad stored any item on another net than requested, the whole batch is
  deleted again in a second commit, and the call fails with `plan_blocked`;
  `reasons` names each such item, its UUID and the net KiCad chose. KiCad's
  undo history then holds both commits.
- If KiCad refused some of the items, or reported one created without its
  UUID, the commit is dropped, and the call fails with `plan_blocked`;
  `reasons` names each such item with KiCad's status.
- In both cases the created UUIDs are then looked up again. When that lookup
  does not show them gone, the call fails with `mutation_outcome_uncertain`
  instead, its `reason` listing the UUIDs to inspect; do not repeat the call
  until they are inspected.
- A read-back that does not account for every created item is
  `mutation_outcome_uncertain` too, and nothing is removed.
- A `CreateItems` answer with a different number of results than items
  cannot be matched to the request: the commit is dropped and the call fails.

Not in this tool: skipping copper already on the board, arcs, blind, buried
and micro vias, zones. No existing tool changes or moves: `pcb_copper` is new,
and deleting and re-netting copper by UUID list are to follow there.
