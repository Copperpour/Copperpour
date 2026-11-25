### New tool `edit_symbol_pins` (minor release)

[#868](https://github.com/Copperpour/Copperpour/issues/868). `edit_symbol_pin`
changes one pin per call, each call a revision-checked read and write of the
whole library, so a change to many pins of a symbol was many writes, and a
failure halfway left the symbol with some pins changed. A new length also kept
the connection point, so a longer pin grew into the body unless the caller
moved it outward by the difference, computed per pin from its angle.

`edit_symbol_pins` (toolset `library_maintenance`) takes `library_path`,
`symbol_name` and `pins`, a list of entries each with `edit_symbol_pin`'s
arguments for one pin: its selectors and the parts to set.

- **All or nothing:** every entry is checked before anything is written, in one
  read and one revision-checked write; a refused entry writes nothing, and the
  refusal names it (`pins[1].new_number`, or a message starting `pins[1]: `).
  Two entries choosing the same pin are refused.
- **As read before the call:** entries choose pins as the library reads before
  the call, so two pins can swap numbers. A new number is refused when another
  pin stands for it once the whole call is applied.
- **`keep`:** `"connection_point"`, the default, as `edit_symbol_pin` does, or
  `"body"`, with `length`: the connection point moves along the pin by the old
  length less the new one, so the end at the body stays where it is. It cannot
  go with `x`, `y` or `angle`.
- **Answer:** `pins`, in entry order, each with the pin's number as asked, name,
  unit, body style and `changes`, as `edit_symbol_pin` reports them; off-grid
  warnings in `warnings`.

`edit_symbol_pin` is unchanged.
