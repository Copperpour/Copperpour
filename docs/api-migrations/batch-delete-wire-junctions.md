### `batch_delete` prunes redundant wire junctions (patch release)

When `batch_delete` deletes wires by UUID, it now checks the junctions those wires
touched against the sheet after the requested deletions. The same rules as
`batch_delete_schematic_wire` apply: redundant junctions are removed, while needed
and unrelated junctions stay.

The wire and junction deletions are saved together. No arguments or response keys
changed. The existing `junctions_pruned_count` and `junctions_pruned_uuids` now also
include junctions pruned because of wire deletion. `deleted_item_uuids` still lists
only explicitly selected items. Callers no longer need a second call to remove
junctions the wire deletion made redundant.
