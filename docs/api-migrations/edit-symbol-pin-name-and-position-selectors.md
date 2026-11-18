### `edit_symbol_pin` chooses between pins that share a number (minor release)

`edit_symbol_pin` chose its pin by `pin_number`, narrowed by `unit` and
`body_style`, and refused two pins with one number in the same unit and body
style as pins it "cannot tell apart". Such pins are the duplicates KiCad's
symbol checker reports, and renumbering one of them is the fix.

- New selectors, which only choose the pin and change nothing: `pin_name`, the
  pin's current name, and `pin_x` with `pin_y`, its current connection point
  in mm. `name`, `x` and `y` still set new values.
- Pins sharing a number in one unit and body style are now an
  `ambiguous_target` whose candidates give each pin's name and connection
  point, so a caller can pick one. Before, this was a `handler_error`.
- Pins that share number, name and connection point stay refused as before.
- A selector that matches no pin is `invalid_argument` on `pin_name` or
  `pin_x`; `pin_x` without `pin_y`, or the reverse, is `invalid_argument` on
  the one missing.
- The answer also carries `pin_name`, the chosen pin's name before the edit.
