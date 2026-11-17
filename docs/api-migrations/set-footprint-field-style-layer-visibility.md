### `set_footprint_field_style` sets the layer, visibility and mirror flag (minor release)

`set_footprint_field_style` takes three more optional arguments, as KiCad's
Edit Text and Graphic Properties dialog sets them on a selection:

- `layer`: named as on a front-side footprint. A footprint on the back gets
  the flipped layer (`F.Fab` becomes `B.Fab`, `In1.Cu` becomes `In2.Cu` on four
  copper layers), as KiCad's Flip moves a footprint's own texts, so one call
  puts every Reference on its own side's Fab layer. A layer with no flipped
  counterpart on the board is refused as `invalid_argument` before anything is
  sent. Changing the layer keeps the mirror flag.
- `visible`: show or hide the field.
- `mirror`: `keep` (the default) or `auto`, which sets each field's mirrored
  flag from the side of the layer it ends on, as `edit_footprint_field` does.

All three need KiCad running with the board open. A call that names one of
them with no live KiCad holding the board is refused as `editor_unavailable`
and changes nothing; it does not fall back to the saved file.

Each field's `before` and `after` now also hold `layer`, `visible` and
`mirrored`, on both paths. On the file path `layer` is null for a field the
file stores no layer for.

Existing calls change only in those three added response keys.
