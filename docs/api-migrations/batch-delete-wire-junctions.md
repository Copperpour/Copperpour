### Wire deletion prunes redundant junctions consistently (patch release)

When `batch_delete` deletes wires by UUID, it now checks the junctions those wires
touched against the sheet after the requested deletions. The same rules as
`batch_delete_schematic_wire` apply: redundant junctions are removed, while needed
and unrelated junctions stay.

All three wire-delete tools now find touched junctions using saved coordinates
on horizontal and vertical wires and at diagonal wire ends. A mixed component
and wire deletion leaves dots merely near a removed wire or pin alone and
does not add dots. Wire deletion keeps dots that connect surviving items
mid-span, even close to a wire end, and conservatively keeps dots inside
surviving diagonal wires.

The wire and junction deletions are saved together. No arguments or response keys
changed. The existing `junctions_pruned_count` and `junctions_pruned_uuids` now also
include junctions pruned because of wire deletion. `deleted_item_uuids` still lists
only explicitly selected items. Callers no longer need a second call to remove
junctions the wire deletion made redundant.
