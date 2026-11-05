### `edit_sheet` and `edit_sheet_pin` keep pins on their edges (patch release)

Resizing a sheet with `edit_sheet` now moves the pins on its right and bottom
edges and its `Sheetfile` caption with those edges, and pulls a pin past the end
of a shrunken edge back to the corner. Before, the pins were left behind and
KiCad relocated them on load
([#285](https://github.com/Copperpour/Copperpour/issues/285)).

`edit_sheet_pin` given `x` and `y` on another edge, without `side`, now turns the
pin to that edge, and `changed_fields` includes `side`. A position on the edge
the pin already names, including either of its corners, keeps the pin's side,
and `changed_fields` stays `["position"]`. A corner of two edges the pin is not
on takes the first of `right`, `left`, `top`, `bottom`. Callers needn't change
anything; a position on no edge behaves as before.
