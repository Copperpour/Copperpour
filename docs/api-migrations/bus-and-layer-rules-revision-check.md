### The bus tools and `set_layer_constraints` refuse a file changed after their read (patch release)

`add_bus`, `batch_add_bus`, `add_bus_entry` and `connect_pins_to_bus` now
replace the `.kicad_sch` only while it still holds the text they read, and
`set_layer_constraints` does the same for the board's `.kicad_dru`. Until now
each replaced its file unconditionally, so a file that KiCad, another server or
a concurrent HTTP call saved in between was put back to the older read, and that
change was lost while both calls reported success
([#200](https://github.com/Copperpour/Copperpour/issues/200)).

A file changed before the write now returns `conflict`, with the file in
`paths`, and is left as the other writer left it. Retry the call to apply the
change to the file as it is now. When there was no `.kicad_dru` at the read,
`set_layer_constraints` creates one without replacing anything, so a rules file
that appeared in between is also refused as `conflict`. A file that no longer
holds the change when read back after the write returns
`mutation_outcome_uncertain`; read the file before retrying. Arguments and
successful responses do not change.

`batch_add_bus` and `connect_pins_to_bus` no longer rewrite the sheet when
every item was rejected; they used to add a blank line. A file removed or
renamed after the read now returns `conflict` from these tools and from the
other revision-checked writers (the board, project-file and symbol-library
writers of [#153](https://github.com/Copperpour/Copperpour/issues/153)),
instead of an unstructured `handler_error`.
