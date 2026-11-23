### `edit_footprint_pad` changes a pad's layers (minor release)

`edit_footprint_pad` takes `layers`, the pad's new layers
([#710](https://github.com/Copperpour/Copperpour/issues/710)), for example to
give an SMD pad on `F.Cu F.Mask` back its `F.Paste` stencil opening. Before,
the only way was to rebuild the footprint with `create_footprint`, which gave
every item a new UUID.

- **Names:** the ones `create_footprint` accepts for a pad (`F.Cu`, `B.Cu`,
  `F.Paste`, `B.Paste`, `F.Mask`, `B.Mask`, `*.Cu`, `*.Mask`), checked by the
  same code.
- **Write:** the pad's `(layers …)` is replaced in place, so the pad keeps its
  UUID and every other child, and written as KiCad's footprint writer orders
  it: `*.Cu` for front and back copper, `*.Paste` and `*.Mask` for both sides,
  then the rest as `F.Cu`, `F.Mask`, `B.Cu`, `B.Mask`, `F.Paste`, `B.Paste`. A
  name given twice is written once. With `match_all`, every pad with the number
  changes.
- **Answer:** `layers` gives the previous and current layers as written; one
  object when every edited pad agrees, a list of the distinct changes otherwise.
- **Refused** as `invalid_argument` on `layers`: an empty list, a value that is
  not a list of strings, and any other layer name.
