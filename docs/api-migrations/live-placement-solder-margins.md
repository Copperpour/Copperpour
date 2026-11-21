### Live placement and Update PCB carry pad and footprint solder margins (patch release)

[#723](https://github.com/Copperpour/Copperpour/issues/723):

- **Now carried:** a library footprint's own `solder_mask_margin`, `solder_paste_margin` and `solder_paste_margin_ratio` (or the older `solder_paste_ratio`), and each pad's own, by `place_component`, `place_component_array`, `duplicate_component` and `add_mounting_hole` while the board is open in KiCad, and by `update_pcb_from_schematic` when it adds a part. `repair_corrupted_footprints` gives the pads it rebuilds their library margins and keeps the board's footprint margins, as before. Before, the board's values applied and each clause was listed in `not_carried`.
- **No longer in `not_carried`:** those clauses.
- **Now refused:** a margin clause that is not one number, which KiCad's parser does not load either, refuses the footprint in each of these tools, as a malformed `zone_connect` does; Update PCB reports it as `unsupported_library_footprint`. Before, it was listed in `not_carried`.
