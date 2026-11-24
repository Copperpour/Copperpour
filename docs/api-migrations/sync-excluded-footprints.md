### `update_pcb_from_schematic` treats the footprint of a symbol excluded from the board as one with no symbol (minor release)

A symbol marked **Exclude from board** (`on_board no`) whose footprint was
still on the board made the sync refuse everything as
`on_board_exclusion_conflict`. The sync no longer returns that code. As
KiCad's Update PCB does, it treats that footprint like one whose symbol was
removed, and syncs the rest of the schematic:

- It is kept, counted in `coverage.board_only_preserved` and listed in
  `footprints_without_symbols`.
- With `delete_footprints_without_symbols: true` it is deleted, under the same
  rules as any other footprint without a symbol: never a board-only one, and a
  locked one kept with a `footprint_locked_not_deleted` note.
- Its link to the schematic is left as it is, as for a footprint whose symbol
  was removed.

The excluded symbol claims a footprint by its identity, or by reference only
when the footprint has no identity, as a symbol on the board does.

Each `footprints_without_symbols` entry and each `delete` change has a new
`reason` field: `symbol_removed` or `excluded_from_board`. Callers that
handled `on_board_exclusion_conflict` by deleting the footprint with
`delete_component` can drop that step, or pass
`delete_footprints_without_symbols: true`.
