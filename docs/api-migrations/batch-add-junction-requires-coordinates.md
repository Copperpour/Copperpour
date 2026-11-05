### `batch_add_junction` refuses a position missing `x` or `y` (patch release)

A `positions` item without `x` or `y` used to become a dot at 0 on that axis,
reported as added. Such a call is now refused as `invalid_argument` naming the
item and coordinate, such as `positions[0].y`, and nothing is written
([#282](https://github.com/Copperpour/Copperpour/issues/282)), as
`batch_add_wire` refuses a wire missing an endpoint. A call whose positions all
carry both coordinates behaves as before.
