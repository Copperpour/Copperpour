### `add_schematic_shapes`, `list_schematic_shapes`; `edit_schematic_text` moves and resizes (minor release)

Two new `sch_batch` tools draw and read a sheet's graphic shapes, which carry
no electrical meaning
([#228](https://github.com/Copperpour/Copperpour/issues/228)); `sch_batch`'s
description now names them.

`add_schematic_shapes(schematic, shapes, revision?)` takes the shape types and
geometry keys `set_footprint_graphics` and `create_symbol` share: each shape is a
`line` (`start`, `end`), a `poly` (`points`, three or more, open unless its
last point repeats its first), a `rect` (`start`, `end`) or a `circle`
(`center`, `radius_mm`), with points as `{x, y}` in millimetres. Every shape
takes `stroke_width_mm` (default 0, the sheet's width), `stroke_type`
(`default`, `solid`, `dash`, `dot`, `dash_dot`, `dash_dot_dot`) and
`stroke_color`; a `poly`, `rect` or `circle` also takes `fill` (`none`,
`background`, or `color` with `fill_color`). A sheet has no `outline` fill
(KiCad 10 saves one as a colour fill in the stroke's colour), and KiCad's hatch
fills are listed but not written. Colours are `[r, g, b]` or
`[r, g, b, a]`, as `add_schematic_text` takes them. Coordinates are rounded to
KiCad's 0.0001 mm. Each shape's block is written as KiCad 10 saves it, so a
save in KiCad keeps its bytes, though KiCad may reorder the sheet's items.
Shapes go in the order given, before the sheet's first placed symbol, where
`add_schematic_text` writes a text, or before KiCad's trailing instance tables
on a sheet without one. A line or poly is a `(polyline …)`, as KiCad writes a graphic
line. Text is not a shape: `add_schematic_text` writes it.

All shapes are prepared as one `SchematicCommand` and committed through the
guard `edit_schematic_text` uses. One bad shape is `invalid_argument` naming
its field, for example `shapes[2].fill_color`, and nothing is written. A
`revision` the sheet no longer has, or a prepared sheet that changes anything
but the new shapes, is `stale_target`; `conflict` while KiCad holds the sheet
or its project's root schematic open, when the sheet changed after it was
read, or when its project cannot be told; `unsupported_capability` for a
sheet outside its project's directory. The sheet is read back: the response
holds `added_count`, each shape as listed from the file with its `uuid`, the
sheet's new `revision` and `source: "file"`; a file that is not the text
written is `mutation_outcome_uncertain`.

`list_schematic_shapes(schematic)` returns `count`, `shapes` and the sheet's
`revision` (the one `list_schematic_texts` returns), with `source: "file"`.
Each shape has `type`, `uuid`, its geometry, `stroke_width_mm`,
`stroke_type`, `stroke_color` and, but for a line, `fill` and `fill_color`,
a colour as `[r, g, b, a]` (whole-number channels) or `null`. A two-point
polyline is a `line`; an `arc` (`start`, `mid`, `end`) and a `bezier` (four
`points`) are listed though not written. Wires, buses, text and the shapes
inside library symbols are not listed. A listed line, poly, rect or circle
whose fill is `none`, `background` or `color`, its `uuid` and `null` values
dropped, can be passed back to `add_schematic_shapes`. A sheet holding a shape
that cannot be read, such as a polyline of one point, is `invalid_argument`
for both tools, as a sheet with an unreadable text item is for
`list_schematic_texts`. `batch_delete` removes a
shape by `uuid`, as it always has, without a revision or group check.

`edit_schematic_text` now also moves and resizes a text item, the rest of
#228's edit: `text` is optional, and the tool takes `x` and `y` (together,
rounded to KiCad's 0.0001 mm) and `size`, the font's height and width in
millimetres, both set to it, from 0.001 to 250 (KiCad loads a size outside
those as the nearest one). What is not given stays as written: the edit
replaces only the string, the `x y` of `(at …)` and the `height width` of the
font's `(size …)`, so the rotation, thickness, bold, italic, justification and
every other byte stay. A call with none of them, `x` without `y` or `y`
without `x`, a size outside that range, or one that would leave the item as it
reads is `invalid_argument`, and the prepared text must change nothing but
those tokens, to the values asked, or it is `stale_target`. A call that passes
only `text` behaves as before. The response adds `size` (`{height, width}`)
and the item's `old_x`, `old_y` and `old_size` to the fields it returned, and
`list_schematic_texts` adds each item's `size`, `null` for a font without one.

`sch_texts`'s write path is now shared with `add_schematic_shapes`.
