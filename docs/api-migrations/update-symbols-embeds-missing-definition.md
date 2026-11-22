### `update_symbols_from_library` embeds a definition the sheet lacks (minor release)

A placed symbol repointed at another library symbol has no copy of the new
definition in `lib_symbols`, and `update_symbols_from_library` refused it with
"has no embedded definition to update"
([#46](https://github.com/Copperpour/Copperpour/issues/46)). It now embeds the
library's definition and lists the lib_id in a new `added` array, with
`added_count`. `dry_run` reports it without writing. Arguments are unchanged.

These cases are still listed in `errors`, and nothing is embedded for them:

- a lib_id that no library resolves. The error includes the lookup message and
  suggestions `add_schematic_component` gives;
- a library symbol that lacks a unit placed on the sheet by any part drawn
  from it, selected or not, as `replace_component` refuses;
- a lib_id the selected parts draw only from sheet-local copies named by
  `(lib_name …)`, which this tool does not refresh. Its error text changed.

The pin-move check compares an embedded copy with the library, so it does not
apply to an added definition. KiCad draws a symbol whose definition is missing
without pins, so embedding one orphans nothing, but its pins may land on wires
drawn for the previous symbol. A response with a non-empty `added` carries a
`hint` to check those connections, for example with ERC.

The step that embeds a definition, shared with `replace_component`, found
`lib_symbols` by its text, so a title or property quoting "(lib_symbols" before
the section received the definition instead. It now finds the section itself,
and reports a definition as embedded only when the sheet holds it, leaving the
sheet unchanged otherwise. `update_symbols_from_library` lists a symbol it
resolves but cannot place in `errors` as such.
