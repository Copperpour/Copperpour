### `rotate_schematic_label` and `batch_rotate_labels` take a label's `uuid` (minor release)

`rotate_schematic_label`, and each `batch_rotate_labels` item, names its label
by `uuid` alone, as `list_schematic_labels` reports it, or by `net` with `x`
and `y` as before, as `rename_schematic_net_label` names one label. Of two
labels of one name on one anchor, a position finds only the first; each can now
be rotated by its uuid. The schema now requires only `rotation` beside
`schematic`; the handler checks the rest.

A `uuid` given with `net`, `x` or `y` is `invalid_argument` on `uuid`; neither
`uuid` nor `net` is `invalid_argument` on `net`, `give uuid, or net with x and
y`; `net` with neither `x` nor `y` is `invalid_argument` on `net`, `goes with x
and y`; a missing `x` or `y` is `missing or not a number` on it, where the
schema gate refused a missing `net`, `x` or `y` as a required property. A batch
item's refusal names its field, such as `labels[1].uuid`. A uuid that is no
label's on the sheet is refused, `No label with uuid '…' in this schematic`,
in a batch with `labels[i]:` before it, and nothing is written. Two items
finding one label, by uuid, by position or one of each, are refused as two
found by position are. A call that names its labels by net and position, each
with its `x` and `y`, behaves as before.

Issue: [#747](https://github.com/Copperpour/Copperpour/issues/747).
