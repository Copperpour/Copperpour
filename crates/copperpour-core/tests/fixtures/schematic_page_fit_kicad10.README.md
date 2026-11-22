# Drawing-area fixtures for schematic page fit

These sheets back the proposed read-only check for content outside a schematic's drawing
frame or inside its title block. Each `schematic_page_fit_*_kicad10.kicad_sch` sheet was
drawn by tool calls, not by hand (`create_schematic`, `add_schematic_text`, `add_wire`,
`add_schematic_component`, `add_schematic_net_label`, `add_no_connect`), then resaved with
`kicad-cli sch upgrade --force` (KiCad 10.0.8), so each carries `(generator "eeschema")`
and `(version 20260306)`. The resave kept every item, UUID, coordinate and property; it
added KiCad's default fields, R1's pin UUIDs and wrote R1's instance under the project
name `""`, as a save without a project does. KiCad 10.0.8's `kicad-cli sch export svg`
plotted each sheet with default settings, and the plotted drawing is the same before and
after the resave. The expected values below come from that plot, not from any code under
test. The two `.kicad_wks` files are not schematics. They are minimal edits of KiCad's
default drawing sheet description, written by hand in KiCad's own format, because
`kicad-cli` has no command that writes or resaves a drawing sheet. KiCad 10.0.8 loads and
plots both.

How the numbers were read from the SVG:

- The drawing sheet is the stroked `#840000` group. Its first three closed paths are the
  title block, the outer frame and the inner frame, in that order. Coordinates are path
  centrelines.
- A note's ink is the extent of its `<g class="stroked-text">` paths plus half the plotted
  stroke width (0.1524 mm) on each side.
- KiCad rounds the page to whole mils before it plots, so A4 is 297.0022 × 210.0072 mm and
  A3 is 419.989 × 297.0022 mm. The right and bottom edges move by that rounding.

## `schematic_page_fit_portrait_kicad10.kicad_sch`

A4 portrait, built-in drawing sheet. The outer frame centreline is `(10, 10)` to
`(200.0072, 287.0022)` mm. The inner frame spans `(12, 12)` to `(198.0072, 285.0022)`, and
the title block spans `(90.0072, 253.0022)` to `(198.0072, 285.0022)`.

The ink of `Past the left frame` starts at x = 11.3263 mm, 0.6737 mm left of the inner
frame. `Into the title block` spans `(95.3263, 273.2403)` to `(113.3189, 274.6627)`,
wholly in the title block. `Left of the title block` is at the same height and ends at x =
46.1008, clear of it. `Inside the frame` is clear of both.

## `schematic_page_fit_a3_landscape_kicad10.kicad_sch`

A3 landscape, built-in drawing sheet. The inner frame spans `(12, 12)` to
`(407.989, 285.0022)`, and the title block spans `(299.989, 253.0022)` to
`(407.989, 285.0022)`.

- `Past the right frame` ends at x = 414.8308, 6.8418 mm past the inner frame.
- `Into the title block` spans `(305.3263, 273.2403)` to `(323.3189, 274.6627)`.
- `Left of the title block` ends at x = 299.4808, 0.508 mm short of the title block at the
  same height. It must not be reported.
- The wire at y = 265.43 runs from x = 250.19 to 320.04, into the title block. The wire at
  y = 257.81 stops at x = 298.45, 1.539 mm short of it, and must not be reported.
- The vertical wire at x = 49.53 runs down to y = 289.56. Wires are checked at their
  centreline, so it is 4.5578 mm past the inner frame; its plotted ink reaches 289.6362.

## `schematic_page_fit_custom_sheet_kicad10.kicad_sch`, `page_fit_custom_*.kicad_wks`

A4 landscape. On the built-in sheet all three notes are clear: `Between the frames` starts
at x = 14.3263, and `Custom title block only` spans `(140.2659, 158.2403)` to
`(162.7335, 160.0861)`, left of the built-in title block at x = 177.0022.

`page_fit_custom_marked.kicad_wks` is a minimal drawing sheet in KiCad's own format with
15 mm margins, the default's repeated frame rectangle, and a rectangle from 150 × 40 mm to
2 × 2 mm in from the bottom-right corner (148 × 38 mm), marked
`rect around the title block`, as in KiCad's default sheet. Its inner frame is `(17, 17)`
to `(280.0022, 193.0072)` and its title block `(132.0022, 155.0072)` to
`(280.0022, 193.0072)`. Against it, `Between the frames` overflows the left frame by
2.6737 mm and `Custom title block only` is inside the title block.
`page_fit_custom_unmarked.kicad_wks` is the same sheet without that comment, so the title
block cannot be identified. No `.kicad_pro` is committed: a test that needs the custom
sheet writes a project file naming it, bare or as `${KIPRJMOD}/…`. To plot it directly:

```text
kicad-cli sch export svg --drawing-sheet page_fit_custom_marked.kicad_wks schematic_page_fit_custom_sheet_kicad10.kicad_sch
```

## `schematic_page_fit_coverage_kicad10.kicad_sch`

A4 landscape with `Device:R` as R1, a wire, the local label `SIG_A` and a no-connect.
Everything KiCad plots outside the drawing sheet lies within `(100.8333, 76.881)` to
`(106.7959, 93.3958)`, inside the frame and clear of the title block, so a check that
measures every item should report that the sheet fits.
