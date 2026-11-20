### Live placement and Update PCB place footprints with custom pads (minor release)

As agreed on [#679](https://github.com/Copperpour/Copperpour/issues/679), part 1:

- **Now placed:** footprints with custom-shape pads, by `place_component`, `place_component_array` and `duplicate_component` while the board is open in KiCad, and by `update_pcb_from_schematic` when it adds a part. Each pad's anchor and primitives are built as KiCad builds them from the library. Before, all of these refused such a footprint ("custom-shape pads are not supported by KiCad 10's typed placement path").
- **Still refused:** a custom pad with `(clearance convexhull)` refuses the footprint as before, now naming the pad ("custom pad '1': its (clearance convexhull) has no field in KiCad 10's pad message ..."), with `unsupported_library_footprint` in Update PCB. So does a primitive the shared model does not read, such as a polygon with arc segments in its outline.
- **No longer in `not_carried`:** live placement no longer lists a custom pad's shape, options or primitives there.
- **Unchanged until part 2:** `update_footprints_from_library`, and Update PCB's footprint replacement, still refuse custom pads by name ("pad shape 'custom' is not supported by typed library refresh").

Callers that refused or worked around such footprints can now place them directly.
