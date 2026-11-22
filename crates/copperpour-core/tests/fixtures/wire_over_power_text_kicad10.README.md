# Wire crossings over power symbol and hierarchical label text

`wire_over_power_text_kicad10.kicad_sch` holds one `power:+3V3` symbol, one hierarchical
label and the wires that cross or miss their text. It has no other symbols and no design
data. It tests `wire_label_overlap` findings for hierarchical labels and power symbols,
whose Value text shows the net name.

## Provenance

Drawn by tool calls, not by hand (`create_schematic`, `add_power_symbol`,
`add_schematic_net_label`, `batch_add_wire`, `delete_schematic_wire`, `add_wire`), then
resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8). The resave kept every item,
UUID, coordinate and property. It added KiCad's default fields and the power pin's UUID,
and wrote the symbol instance under the project name `""`, as a save without a project
does. KiCad 10.0.8 gives the same netlist, ERC and plotted drawing before and after the
resave.

## Expected results

The expected crossings come from KiCad 10.0.8's SVG plot: each wire's centre line against
the visible `<g class="stroked-text">` paths, in mm. Glyph ink: `+3V3` x 48.56–53.10, y
46.55–47.82; `SIG_H` x 78.00–82.84, y 50.11–51.50.

```text
kicad-cli sch export svg --exclude-drawing-sheet -o . wire_over_power_text_kicad10.kicad_sch
```

| Wire | KiCad plot | Expected |
| --- | --- | --- |
| Horizontal at y = 46.99, x = 45.72–55.88 | Crosses every glyph of the `+3V3` Value text | `wire_label_overlap` |
| Vertical at x = 80.01, y = 48.26–53.34 | Crosses the `G` of `SIG_H` at y ≈ 50.1–51.4 | `wire_label_overlap` |
| Vertical at x = 50.8, y = 50.8–55.88, from the power pin | Leaves the pin away from the symbol and its text | No finding |
| Horizontal at y = 44.45, x = 45.72–55.88 | Passes 2.1 mm above the `+3V3` text | No finding |
| Horizontal at y = 50.8, x = 68.58–76.2, to the label's anchor | Ends 1.8 mm before the `SIG_H` text | No finding |

`kicad-cli sch export netlist` (10.0.8) lists no nets: the only symbol is the power
symbol. `kicad-cli sch erc --severity-all` reports the two crossing wires among 3
`wire_dangling` errors, with 1 `label_dangling`, 2 `pin_not_connected` and 1
`power_pin_not_driven` error and 8 `unconnected_wire_endpoint` warnings. A wire through
the text does not connect to the symbol or label. ERC counts leave out `lib_symbol_issues`
and `footprint_link_issues`, which depend on the machine's KiCad library tables.
