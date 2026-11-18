### Footprint refresh refuses pads and graphics on missing inner copper layers (patch release)

`update_footprints_from_library` reports `footprint_update_conflict` if a selected footprint has a library pad, graphic, custom property, or reference point on an inner copper layer that the live board does not have. This now applies to front-side footprints as well as back-side footprints. No footprints in the batch are written. Correct the library layers or the board stack as appropriate, then run a new dry run before applying.

`update_pcb_from_schematic` reports `unsupported_library_footprint` for the same conflict when replacing an existing footprint. New footprint placement is unchanged.
