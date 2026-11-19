### `edit_symbol_pin` shows or hides a pin (minor release)

`edit_symbol_pin` takes `hidden`: `true` hides the pin and `false` shows it,
as KiCad's pin dialog does. Without it, a pin's visibility is left as it is.

- **Write:** every hide marker of the pin is removed, and a hidden pin gets
  one where KiCad writes it, after `(length …)`: `(hide yes)`, or a bare
  `hide` in a library older than format 20241004, which KiCad 8 can still
  open. A pin already as asked is left as written.
- **Read:** as KiCad reads a pin, a bare `hide` hides it and the last
  `(hide yes|no)` decides.
- **Answer:** `changes.hidden` gives the previous and current visibility when
  it changed.
- **Refused** as `invalid_argument` on `hidden`: anything but `true` or
  `false`.

A hidden pin stacked on another, such as an exposed pad's extra pin, is drawn
over that pin once shown; move it with `x` and `y` in the same call.
