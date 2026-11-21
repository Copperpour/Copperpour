### `create_symbol` and `edit_symbol_pin` warn when a pin is off the 1.27 mm grid (patch release)

`create_symbol` now adds a per-unit entry to `warnings` when any resolved pin
connection point `(x, y)` does not lie on KiCad's 1.27 mm schematic grid, and
`edit_symbol_pin` now returns a `warnings` array when a position edit (`x` or
`y`) leaves the pin off the 1.27 mm grid. Both tools still write the requested
coordinates; callers that expect a clean `warnings`-free response should pass
multiples of 1.27 mm (or omit `x`/`y` on `create_symbol` for automatic
grid-aligned placement).
