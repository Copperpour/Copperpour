### `check_schematic_overlaps` reports label and field text as `text_overlap` (minor release)

`check_schematic_overlaps` compared symbol drawings and pins, and labels only
where their anchors coincided, so two labels whose text ran into each other,
or a power symbol's Value lying over a label, reported clean
([#111](https://github.com/Copperpour/Copperpour/issues/111)). It now also
measures the text of every net, global and hierarchical label and of every
visible field of a placed symbol, and reports each that runs into another, or
into another symbol's body, as an entry of `"type": "text_overlap"`. `a` and
`b` each name their item: `{"kind": "label", "label_type", "net", "uuid",
"text"}`, `{"kind": "field", "reference", "unit", "symbol_uuid", "field",
"text"}` or `{"kind": "symbol_body", "reference", "unit", "symbol_uuid"}`.
Beside them are `overlap_x_mm`, `overlap_y_mm`, `bounds_a` and `bounds_b`, as
`component_overlap` has them. Text is compared at its ink, and a label's
outline or flag at its drawn line, as a body is, so outlines that only meet,
such as global labels stacked on the grid, are not reported; `bounds_*` and the
overlap depths keep the outline's stroke.
A symbol's own fields are not compared with its own body, and two labels of
different nets at one anchor stay a single `label_overlap`.

The text is placed and measured as eeschema draws it in KiCad's stroke font;
text in another font is measured as if it were. Free text, sheet fields and
sheet pins are not measured. `component_overlap` and `label_overlap` entries are
unchanged, but `overlap_count` now counts the text entries too, so a sheet that
reported 0 may report more. A caller that treats `overlap_count` as symbol
collisions only should count the entries whose `type` is `component_overlap`.
