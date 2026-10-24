### `update_footprints_from_library` can reset Reference and Value text (minor release)

`update_footprints_from_library` accepts two optional booleans, both
`false` by default, named after KiCad 10's own Update Footprints options:

- `reset_text_sizes_and_styles` gives each refreshed footprint's Reference and
  Value the library's text attributes: size, thickness, angle, justification,
  mirroring and style.
- `reset_text_positions` moves them to the library's position.

Either keeps the fields' text, layer and visibility. A library Reference or
Value with no position, layer or effects is refused for that footprint when a
reset is asked for. With neither flag, Reference and Value keep their placed
effects as before, and the plan revision is unchanged.

A plan that resets a field reports `text_effects` in `changed_domains`. Each
change then lists `text_effects`: the field, and its `before` and `after`
position, angle, size, thickness (`null` for KiCad's default of one eighth of
the height), style, mirroring, keep-upright and alignment. After the commit
the board is read back, and a field that does not hold its reset returns
`mutation_outcome_uncertain`.
