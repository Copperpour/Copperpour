### Footprint unit-mapping warnings name the affected KiCad versions (patch release)

The descriptions for `edit_component`, `edit_footprint_field`, and `set_footprint_field_style`, and the live warnings from `edit_component` and `set_footprint_field_style`, now explain that whole-footprint IPC writes can drop symbol-unit mapping on KiCad 10 before 10.0.8. The warning is still returned after the same live writes; its text is not a report that the mapping was lost. Tool inputs, response fields, and edit behavior are unchanged. Callers need not change their calls.

The live warnings link to [Troubleshooting](https://github.com/Copperpour/Copperpour/blob/main/docs/TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit) for checking a board edited on an affected version and recovering missing mappings. Upgrading does not restore missing mappings.
