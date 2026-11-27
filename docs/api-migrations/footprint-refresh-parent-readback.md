### `update_footprints_from_library` verifies target identity, `parent` KIIDs, and preserved state from post-commit readback (patch release)

[#85](https://github.com/Copperpour/Copperpour/issues/85), part B:

- **Always reads back after apply:** `update_footprints_from_library` with
  `dry_run: false` now performs a fresh `GetItems(KOT_PCB_FOOTPRINT)` readback after
  every commit (not only when resetting text effects or changing net-tie pad groups) and
  derives each applied change's `preserved` flags from that observed readback rather than
  the request payload.
- **Parent-field compatibility (KiCad 10.0.6+):** observed on KiCad 10.0.8 (KiCad 10.0.6
  and 10.0.7 follow `board_types.proto` `Since: 10.0.6` and remain unobserved validation
  debt), KiCad preserves `FootprintInstance.parent` (the board KIID) and regenerates
  every rebuilt child's read-only `parent` KIID to the parent footprint's KIID. Apply
  verifies that every updated footprint is present by its exact KIID, kept its
  `FootprintInstance.parent`, has all child `parent` KIIDs pointing to it (and present on
  KiCad 10.0.6+ or whenever pre-update readback carried child parents), and preserved its
  planned instance state; any mismatch or failed readback returns
  `mutation_outcome_uncertain` instead of `status: "applied"`. On KiCad `< 10.0.6` when
  pre-update readback has no child parents, absent `parent` fields remain accepted while
  wrong non-empty child `parent` KIIDs are still flagged.
- **Pre-write refusal:** a board footprint missing its KIID is refused with
  `status: "conflict"` (`footprint_update_conflict`) before any IPC write.
