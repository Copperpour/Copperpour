### `update_pcb_from_schematic` skips a part it cannot build instead of refusing the sync (minor release)

One part the dry run found it could not build made the whole sync a conflict,
with nothing planned for the parts that could be built. Such a part is now
skipped and every other part is synced, as KiCad's Update PCB reports its
errors and still commits the rest (`BOARD_NETLIST_UPDATER`). KiCad handles
some of these parts differently: it adds a footprint that lacks a connected
pad and reports the pad, and it keeps the old footprint of a replacement it
cannot build. The sync leaves the whole part as it is instead.

- A part is skipped when its library footprint does not resolve, cannot be
  read or cannot be placed (`footprint_library_resolution_failed`,
  `footprint_library_read_failed`, `unsupported_library_footprint`), when its
  footprint lacks a pad the schematic connects (`footprint_pad_missing` for an
  added part), when its update would drop groups, points or variants the
  footprint owns (`footprint_children_not_carried`), or when its replacement
  footprint cannot be built (the replacement codes of
  `update-pcb-replaces-footprints.md`, including
  `footprint_replacement_drops_connected_pad`).
- New response key `skipped_footprints`: each skipped part's diagnostic, with
  the same fields and codes the conflict carried (`code`, `message`,
  `reference`, `references`, `footprint_id`). One diagnostic can name several
  parts that share a footprint, and one part can have several diagnostics.
  The apply answer carries it too.
- New count `coverage.skipped_footprint` (`planned`, `applied`): how many parts
  were skipped, by the dry run and by the apply (as `unassigned_footprint`
  counts parts left out). The other counts no longer include them, nor the pad
  net changes of a skipped update.
- The status is `ready` when anything else is left to apply, and the apply
  applies only that. The skipped parts are part of the plan revision.
- A note about a skipped part, such as `footprint_locked_not_replaced`, is
  dropped from `diagnostics`. So is `routed_pad_net_change_acknowledged`: an
  acknowledged routed pad net change (`acknowledged_net_changes`) goes with its
  part: the apply neither makes it nor runs the post-apply copper and DRC
  check for it.

Unchanged:

- With nothing else to apply, the dry run is a `conflict` as before, with the
  same diagnostics in `diagnostics`, counted in `coverage.conflicts`, and
  `skipped_footprints` empty. So is a plan whose updates cannot be checked
  because the board KiCad wrote cannot be read or lacks a planned footprint:
  that is not the part's fault, so nothing is skipped for it.
- The planner's own conflicts still refuse the whole sync: a duplicate
  reference, a pad net change on routed copper that `acknowledged_net_changes`
  does not name, an acknowledgement that matches no change, a pad a placed
  footprint lacks (without a replacement).

A caller that read a `conflict` as "nothing can be synced until every part is
fixed" should check `skipped_footprints` on a `ready` plan: those parts stay as
they are on the board until the cause is fixed and the sync is run again.
