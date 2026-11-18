### `add_zone` and `add_copper_pour` refuse a vertex missing `x` or `y` (patch release)

A `points` item without `x` or `y` used to be left out of the outline, and a
zone of the remaining vertices was written and reported as added. Such a call
is now refused as `invalid_argument` naming the item and coordinate, such as
`points[3].y`, and nothing is written
([#611](https://github.com/Copperpour/Copperpour/issues/611)), as
`batch_add_junction` refuses a position missing either. A call whose vertices
all carry both coordinates behaves as before.
