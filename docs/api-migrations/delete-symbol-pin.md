### New tool `delete_symbol_pin` (minor release)

[#716](https://github.com/Copperpour/Copperpour/issues/716). No tool could remove
one pin of a library symbol: a duplicate pin, as KiCad's symbol checker reports
it, could only be fixed by deleting the whole symbol and re-entering it, losing
its graphics, field positions and UUIDs.

`delete_symbol_pin` (toolset `library_maintenance`, as `library` is at its
size cap) removes one pin, chosen as
`edit_symbol_pin` chooses one: `pin_number`; `unit` and `body_style` when the
number is in more than one unit or body style; `pin_name`, or `pin_x` and
`pin_y`, when pins share a number in one unit and body style. The pin's block
and its line go; everything else in the library stays as written.

- **Answer:** `removed` gives the pin's number, name, type, style, unit, body
  style, position, angle, length and visibility. `derived_symbols` names the
  symbols that extend the symbol, directly or through other derived symbols:
  their pins are its, so they lose the pin too.
- **Refused before writing:** a choice that fits more than one pin
  (`ambiguous_target`, naming the candidates), a number the symbol does not have,
  a selector that matches none, and a derived symbol (`invalid_argument` on
  `symbol_name`, naming its root, whose pins it shows).

`add_symbol_pin` follows in a separate change, as agreed on #716.
