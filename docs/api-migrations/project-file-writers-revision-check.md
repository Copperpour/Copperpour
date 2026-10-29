### The project-file writers refuse a `.kicad_pro` changed after their read (patch release)

`set_design_rules`, `set_predefined_sizes`, `create_netclass` and
`assign_net_to_class` now replace the board's `.kicad_pro` only while it still
holds the text they read. Until now each replaced it unconditionally, so a
project file that KiCad, another server or a concurrent HTTP call rewrote in
between was put back to the older read, and that change was lost while both
calls reported success
([#153](https://github.com/Copperpour/Copperpour/issues/153)).

A file changed before the write now returns `conflict`, with the project file
in `paths`, and is left as the other writer left it. Retry the call to apply
the change to the file as it is now. A file that no longer holds the change
when read back after the write returns `mutation_outcome_uncertain`; read the
project file before retrying. Arguments and successful responses do not change.

The four writers also ask for KiCad's project lock again at the write, so a
KiCad program that opened the project after the first check is refused with
`unsafe_file_fallback`, as before the read. Board file fallbacks and the
symbol-library writers now share the same revision-checked write: their
`conflict` message reads "The board '…' changed after Copperpour read it, …
Reload it and retry.", and a symbol library's `mutation_outcome_uncertain`
`reason` names "the symbol library" instead of "the library". Kinds and fields
are unchanged.
