### New tool `set_symbol_graphics`; `get_symbol_info` lists graphics (minor release)

[#867](https://github.com/Copperpour/Copperpour/issues/867). No tool could change
the drawing of an existing library symbol: a body rectangle that had to grow with
its pins could only be redrawn in KiCad's symbol editor, and `get_symbol_info`
did not list a symbol's graphics.

**`get_symbol_info`** answers two new keys:

- `graphics`: each item the symbol draws, unit block by unit block in file order,
  with its `unit`, `body_style`, `index` within them and `item` in
  `create_symbol`'s vocabulary (`rect`, `line`, `poly`, `arc`, `circle`; also
  `bezier`, `text` and `text_box`, which that vocabulary cannot write). Beside the
  item, `private`, `stroke_type` (other than `default`), `stroke_color`,
  `fill_color`, the `fill` of a line or arc, and a `fill` the vocabulary cannot
  write (`color`, `hatch`, `reverse_hatch`, `cross_hatch`; the item's own `fill`
  then reads `none`), only when the file has them. So a listed `rect`, `line`,
  `poly`, `arc` or `circle` can always be written back. A derived symbol lists
  its root's graphics.
- `revision`: the library file's revision.

**`set_symbol_graphics`** (toolset `library_maintenance`) takes `library_path`,
`symbol_name`, `revision`, `unit`, `body_style` and `mode`:

- `append` with `graphics` writes the items after the unit's last shape, and
  writes the unit block when the symbol has none for that unit and body style
  (a unit the symbol has, or 0; body style 1 or one it has).
- `replace` with `index` and one item writes that item in place of the graphic.
- `delete` with `index` removes the graphic.

Items are written in KiCad 10's layout, and every other byte of the file stays as
it was. KiCad's next save may order a unit's items by position.

- **Answer:** `added`, or `previous` and `current`, or `removed`; `graphics`, the
  unit's list after the edit; the new `revision`; `derived_symbols`, the symbols
  that show the change; `warnings` for a point off the 1.27 mm grid (rectangle
  corners, line and polygon points, arc ends, circle centres) and for what a
  replaced graphic had that the new item does not carry.
- **Refused before writing:** a revision the file no longer has
  (`stale_target`), a derived symbol (`invalid_argument` on `symbol_name`, naming
  its root), an index the unit does not have, a unit or body style the symbol
  does not have (for `append`: a unit beyond its units, a body style it lacks),
  an item outside the vocabulary, and arguments the mode does not take.
