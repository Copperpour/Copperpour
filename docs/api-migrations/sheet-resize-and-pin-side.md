### `edit_sheet` and `edit_sheet_pin` keep pins on their edges (patch release)

Resizing a sheet with `edit_sheet` now moves the pins on its right and bottom
edges and its `Sheetfile` caption with those edges, and pulls a pin past the end
of a shrunken edge back to the corner. KiCad used to relocate those pins on load.

`edit_sheet_pin` given `x` and `y` on another edge, without `side`, now turns the
pin to that edge, and `changed_fields` includes `side`. Callers needn't change
anything; a position on no edge behaves as before.
