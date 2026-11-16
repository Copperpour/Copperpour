### `check_schematic_overlaps` reports a wire through a symbol's body as `wire_overlap` (minor release)

`check_schematic_overlaps` compared symbol drawings and pins, labels and text,
and nothing looked at wires, so a wire drawn straight through a symbol's body
reported clean. A wire whose ends sit on a crystal's two pin tips, drawn through
its body, joins both pins in KiCad and reported clean. It now reports each schematic
wire that passes through the drawn body of a placed symbol as an entry of
`"type": "wire_overlap"`. `a` names the wire, `{"kind": "wire", "uuid", "x1",
"y1", "x2", "y2"}` as `list_schematic_wires` writes its ends, and `b` the symbol,
`{"kind": "symbol_body", "reference", "unit", "symbol_uuid"}` as `text_overlap`
does, with `bounds_b` the body's bounds. Wires in any direction are checked.

The body is the symbol's drawings without its pins, the box `text_overlap`
compares text against. A wire counts when the middle of its run inside that box
lies strictly within it, so a wire along an edge, one that only touches an edge,
and one ending on a pin tip outside the body are not reported. Nor is a
wire with an end on one of the symbol's own pin tips that does not head back
toward the pin's root, because a symbol's drawing can reach past a pin's tip,
as KiCad's stock `Device:LED` does with its emission arrows. A wire from the
tip toward the root is reported, and a zero-length pin, such as a power
symbol's, exempts nothing, so a wire from its tip into its own drawing is
reported. A wire leaving a tip square to the pin is exempt even where it crosses the drawing that reaches past the tip, as one from the LED's cathode tip down across an emission arrow would; that is the limit of the rule. Buses and graphic lines are not checked, and a symbol whose geometry is not resolved is
skipped, as it is for `text_overlap`; it is still named in `bounds_unresolved`.

Entries are counted in `overlap_count` and listed in `overlaps` with the others,
so a sheet that reported clean may now report. `component_overlap`,
`label_overlap` and `text_overlap` entries are unchanged. Issue:
[#504](https://github.com/Copperpour/Copperpour/issues/504).
