### New `pcb_text` toolset lists and edits placed board text and footprint fields in place (minor release)

No tool could change a piece of text already on the board without deleting
and re-adding it, and none could change a placed footprint's Reference or
Value text beyond its position. Four tools make up a new `pcb_text` toolset
(`load_toolset("pcb_text")`). All need the board open in KiCad; each edit is
one undo step, is read back from KiCad afterwards, and keeps every property it
was not asked to change.

- `list_board_texts` lists board-level text items with their UUID, raw text
  (`${VARIABLE}` kept), position, angle, size, thickness, font, layer,
  alignment and KiCad's bounding box.
- `edit_board_text` moves, rotates, resizes, re-layers, re-words or restyles
  one of them by UUID: `mirrored` (or `mirror: "auto"` to match its layer
  side), `horizontal_alignment`, `vertical_alignment`, `bold`, `italic`,
  `keep_upright`, `knockout`, `locked` and `font`.
- `list_footprint_fields` lists the Reference, Value and other fields of one or
  every placed footprint, with the same attributes.
- `edit_footprint_field` makes the same edits to one placed footprint's
  Reference or Value field, and can show or hide it.

No existing call changes. `pcb_board`, already at the 20-tool cap, keeps its
tools.
