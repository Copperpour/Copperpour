### Live placement carries pad properties, zone settings and reference points, and reports `not_carried` (minor release)

Live IPC footprint placement (`place_component`, `place_component_array`, `duplicate_component`, `add_mounting_hole`, `repair_corrupted_footprints`, and added footprints in `update_pcb_from_schematic`) now carries pad zone connections, thermal relief settings, and (on KiCad 10.0.7+) pad fabrication properties and reference points.

When a library footprint includes clauses or children that typed IPC placement cannot carry (or requires KiCad 10.0.7+ on an older KiCad), placement still succeeds (`isError: false`) and reports the omitted items:

- Placement and repair tools include a non-empty `not_carried` array of strings in the JSON response.
- `update_pcb_from_schematic` emits non-blocking `not_carried` entries in `diagnostics`, replacing `pad_properties_not_carried`.
