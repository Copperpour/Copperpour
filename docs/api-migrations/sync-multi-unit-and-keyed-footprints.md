### `update_pcb_from_schematic` matches every unit of a multi-unit symbol and syncs keyed footprints (minor release)

Three cases that made the dry run a `conflict` with `changes_count: 0`, so that
nothing else was synced, are handled as KiCad's Update PCB handles them:

- **A footprint linked to a unit other than the first.** A multi-unit symbol has
  one UUID per placed unit, and KiCad links a footprint to any of them. Only the
  first was read, so the footprint looked like another symbol's
  (`reference_identity_conflict`). It is matched now, and keeps its link. Two
  footprints linked to two units of one symbol are a `duplicate_board_identity`
  conflict that names both.
- **A pin KiCad left unconnected, on a footprint that has no pad for it**, such as
  a keyed header whose pin at the key has a no-connect flag. Such a pin is alone on
  KiCad's `unconnected-…` net, or has a no-connect pin type, and connects nothing.
  It no longer counts as a missing pad (`footprint_pad_missing`), for a footprint
  on the board, an added one or a replacement, and the change's `pad_nets` name
  only the pads the footprint has.
- **A footprint on the board without a pad a connected pin needs.** It is skipped
  and listed in `skipped_footprints` with its `footprint_pad_missing` diagnostic,
  as an added part that lacks a pad already was, and every other part is synced.
  With nothing else to apply, the dry run is still a conflict with that
  diagnostic.

Callers that relinked such footprints in KiCad, or synced with KiCad's Update PCB
(F8) instead, can sync with the tool. A caller that read a `footprint_pad_missing`
diagnostic for a footprint on the board finds it under `skipped_footprints` now.
