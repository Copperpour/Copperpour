### `update_pcb_from_schematic` replaces a footprint the schematic names differently (minor release)

A footprint whose symbol names another library footprint stopped the whole
sync with a `footprint_id_changed` conflict. The sync now replaces it in place,
as KiCad's Update PCB does (`BOARD_NETLIST_UPDATER::replaceFootprint`):

- The planned change is an `update` with
  `replace_footprint: { "from": "<old Library:Footprint>", "to": "<new>" }`.
  The key is absent from every other update.
- The replaced footprint keeps its KIID, position, rotation, side, lock and
  symbol link, and each pad's net and schematic pin by pad number. It takes
  the new library footprint's pads, artwork, texts, custom properties,
  attributes, its own clearance, zone connection and solder mask and paste
  margins, and 3D models, and Reference and Value take the library's
  position and style when the library footprint states them as properties
  (KiCad 8 and later); a footprint in the older `fp_text reference` form
  leaves the placed text as it is. The schematic's value, fields, DNP and BOM exclusion are
  then set as for any update; every symbol field is set again, because the
  old custom properties are gone.
- A locked footprint is not replaced. Its other differences are still
  planned, and the plan carries a `footprint_locked_not_replaced` diagnostic
  that refuses nothing, as KiCad's Update PCB warns.
- The dry run builds each replacement once, so a planned replacement is one
  the apply can build. One it cannot build is reported when the new footprint
  does not resolve (`footprint_library_resolution_failed`), cannot be read
  (`footprint_library_read_failed`) or cannot be rebuilt losslessly
  (`unsupported_library_footprint`), when it lacks a pad the schematic
  connects (`footprint_pad_missing`), or when it would drop a pad that carries
  a net on the board (`footprint_replacement_drops_connected_pad`). Its part
  is skipped and listed under `skipped_footprints`; see
  `update-pcb-skips-unbuildable-parts.md`.

`footprint_id_changed` is no longer reported. A caller that read it as "this
part needs a manual footprint change" can review the planned replacement in
the dry run and apply it.
