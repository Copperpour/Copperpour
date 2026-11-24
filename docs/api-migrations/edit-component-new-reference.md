### `edit_component` renames board-owned footprints in place with `new_reference` and optional `kiid` (minor release)

`edit_component` now accepts optional `new_reference` and `kiid` arguments and reports `kiid` at the top level of its response, with `reference` now included in `before`, and in `changed` when it changes.

- `new_reference` renames a placed footprint in place in one KiCad undo step and proves the committed reference from a fresh readback by KIID.
- Renaming a footprint linked to a schematic symbol (non-empty `symbol_path`) is refused with `plan_blocked` so the board and schematic do not silently diverge; rename schematic-owned footprints via `edit_schematic_component` and `update_pcb_from_schematic`.
- A renamed footprint with no symbol link that takes a reference the schematic uses is adopted by the next `update_pcb_from_schematic`, which matches it by reference, as KiCad's Update PCB does.
- Renaming to a reference already carried by another footprint on the board is refused with `plan_blocked`.
- Optional `kiid` pins the exact footprint instance (and disambiguates duplicate board-only references such as `REF**`), refusing with `stale_target` if that KIID is absent or no longer carries `reference`.
- A `reference` several footprints carry, given without `kiid`, is refused with `ambiguous_target`; its `candidates` are their KIIDs and the message gives each one's position. It was a `handler_error` preflight refusal before.
- Existing callers that omit `new_reference` and `kiid` keep the same inputs and edit behavior.
