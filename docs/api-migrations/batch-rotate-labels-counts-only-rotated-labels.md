### `batch_rotate_labels` counts only the labels it rotates (patch release)

A `labels` item missing `rotation`, or naming a label that is not at its
`x` and `y`, used to be counted in `rotated` and left as it was
([#724](https://github.com/Copperpour/Copperpour/issues/724)). An item missing
`net`, `x`, `y` or `rotation` is now refused as `invalid_argument` naming it,
such as `labels[1].rotation`, as `batch_add_junction` refuses a position
missing a coordinate. A label that is not there refuses the call naming the
item, and two items naming the same label are refused as `invalid_argument`.
In every case nothing is written: the batch is now one read and one write, as
its description says. A call whose labels are all found, each named once,
behaves as before.
