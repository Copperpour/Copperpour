### Embedded symbol definitions on replacement and library refresh (patch release)

`replace_component` removes the old embedded definition from the edited sheet when the
last placement stops using it. The successful response adds `removed_lib_symbol` with
the removed definition's name, or `null` when the call removed no definition. A
placement that draws from the old definition through a sheet-local `lib_name` keeps it
in use. Existing definitions are kept on sheets with multiple `lib_symbols` sections.

`replace_component` and `update_symbols_from_library` write new or refreshed embedded
definitions with KiCad-style nested layout. For callers inspecting the file, match
definitions structurally so files written by older releases still work. An unchanged
definition can retain its previous layout.
