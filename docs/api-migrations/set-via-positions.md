### `set_via_positions` batches existing via moves (minor release)

Load `pcb_copper` for the new live-only `set_via_positions` tool. Existing
`move_via`, `query_vias` and `apply_copper` calls are unchanged.

Give either `placements: [{uuid, x, y}]` for absolute millimetre destinations,
or `uuids: ["..."]` and `offset: {x, y}` for one shared translation. The two
forms are mutually exclusive and must select at least one unique via. The tool
rounds to nanometres and refuses out-of-range coordinates and destinations,
missing/non-via IDs, and locked or unknown-lock-state targets.

`dry_run` defaults to `true`. Apply the identical plan with `dry_run: false` and
`expected_plan_revision` from the dry run. The revision binds the exact live
board snapshot, document, target preimages and normalized destinations. Changed
copper or target properties require a new dry run. An unchanged plan does not
open a commit.

Apply uses one KiCad commit and reads every selected via back, including all
native fields, not just position. If KiCad re-nets a via, a net-only mismatch
restores the entire batch in a second commit and verifies the original messages.
This is not a promise of atomicity: a failed or unverifiable write/restoration
returns `mutation_outcome_uncertain` with `uuids` and requires inspection before
retrying. A confirmed refusal is distinct from an uncertain mutation. Responses
from the handler include the shared `outcome` metadata (dispatch/schema refusals
retain their existing error contract); successful `vias` rows contain observed
UUID, X and Y, with `source: "ipc_readback"`. Dry-run rows are planned destinations
with `source: "ipc_plan"`, not observations of a completed move.

No board is saved and no connected track is moved or stretched. Zone refill and
DRC remain caller-controlled steps after a completed batch; refill before DRC.
