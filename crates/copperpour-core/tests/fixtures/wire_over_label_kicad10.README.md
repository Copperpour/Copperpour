# Wire crossings over label text

`wire_over_label_kicad10.kicad_sch` is an A4 sheet with three net labels and the wires
that cross or miss their text. It has no symbols and no design data. It tests
`wire_label_overlap` findings in `check_schematic_overlaps`.

## Provenance

Drawn by tool calls, not by hand (`create_schematic`, `batch_add_wire`,
`add_schematic_net_label`), then saved by KiCad 10.0 (`(generator "eeschema")`,
`(version 20260306)`). The file does not record the KiCad patch release that saved it.
Resaving it with `kicad-cli sch upgrade --force` (KiCad 10.0.8) leaves it byte for byte
unchanged.

## Expected results

The expected crossings come from KiCad 10.0.8's SVG plot: each wire's centre line against
the visible `<g class="stroked-text">` paths of each label, in mm.

```text
kicad-cli sch export svg --exclude-drawing-sheet -o . wire_over_label_kicad10.kicad_sch
```

| Wire | KiCad plot | Expected |
| --- | --- | --- |
| Vertical at x = 55.88, y = 45.72–49.53 | Crosses a stroke of `SIGNAL_INPUT` at y ≈ 49.08 | `wire_label_overlap` |
| Vertical at x = 104.14, y = 48.26–53.34 | Crosses strokes of `GLOBAL_SIGNAL` at y ≈ 50.93 and 51.37 | `wire_label_overlap` |
| Vertical at x = 102.87, y = 48.26–53.34 | Crosses the pointed outline but sits before the text of `GLOBAL_SIGNAL` | No finding |
| Vertical at x = 55.88, y = 71.12–73.66 | Stops before any stroke of `SIGNAL_CLEAR` | No finding |
| Horizontal wires that attach to the three label anchors | Sit outside the lettering | No finding |

`kicad-cli sch export netlist` (10.0.8) loads the sheet and lists no nets, since it has no
component. `kicad-cli sch erc --severity-all` reports 4 `wire_dangling` and 3
`label_dangling` errors and 11 `unconnected_wire_endpoint` warnings. The 4 `wire_dangling`
are the four vertical wires: the two crossing wires and the two controls at x = 102.87 and
below `SIGNAL_CLEAR`. A wire through label text does not connect to the label, and nothing
in ERC is about text, so ERC cannot tell a crossing from a clear wire.
