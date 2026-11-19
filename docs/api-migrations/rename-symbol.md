### New tool `rename_symbol` in `library_maintenance` (minor release)

`rename_symbol(library_path, symbol_name, new_name)` renames a symbol in its
`.kicad_sym` library, as KiCad's symbol editor does, and as agreed on
[#634](https://github.com/Copperpour/Copperpour/issues/634).

- **Write:** the symbol and its unit blocks (`NAME_<unit>_<body style>`) take
  the new name, symbols derived from it extend the new name, and its Value
  changes when it equals the old name. The name is stored as KiCad stores it
  from its rename dialog: `\ < > : "` escaped as `{backslash}`, `{lt}`,
  `{gt}`, `{colon}`, `{dblquote}`, line breaks dropped. A library in KiCad's
  save order (inheritance depth, then name) keeps it: the symbol moves to its
  place. A library in another order keeps that order.
- **Refused** as `invalid_argument` on `new_name`, with nothing written: a
  name already in the library, the symbol's own name, an empty name, one with
  spaces at either end, or one with a tab. `symbol_name` not in the library is
  refused on `symbol_name`. A file that is not a `.kicad_sym` is a handler
  error, as is a unit block not named after its symbol; a missing file is
  `file_not_found`.
- **Answer:** `previous_name`, `name` (as stored), `escaped` when the stored
  name differs from `new_name`, `value` (`previous` and `current`) when the
  Value changed, `derived_symbols` whose `extends` changed, `moved` when the
  symbol moved, and `kept_references`: symbols placed in schematics keep the
  old library id.
