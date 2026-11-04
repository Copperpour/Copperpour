# Sheet-resize fixture ([#285](https://github.com/Copperpour/Copperpour/issues/285))

`sheet_pin_resize.kicad_sch` and `child.kicad_sch` hold one hierarchical sheet
with a pin on each edge, beside a minimal `sheet_pin_resize.kicad_pro`, so `edit_sheet` resizes and `edit_sheet_pin` moves can
be checked against the coordinates KiCad itself keeps.

## Provenance

1. Built through the Copperpour MCP server over stdio (`create_project`,
   `add_hierarchical_sheet` with `x` 50.8, `y` 50.8, `width` 81.28,
   `height` 50.8, then four `add_sheet_pin` calls with `side`).
2. Both files force-resaved by KiCad 10.0.7:

   ```text
   kicad-cli sch upgrade --force sheet_pin_resize.kicad_sch
   kicad-cli sch upgrade --force child.kicad_sch
   ```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`. Resaving the committed pair again, with
`sheet_pin_resize.kicad_pro` beside it, reproduces the committed bytes
exactly. Without the project file KiCad does not know the project name and
rewrites the sheet's instance as `(project "")`, so keep the three files
together when resaving.

| Pin | Edge | `at` |
|---|---|---|
| `R` | right | (132.08, 71.12, 0) |
| `L` | left | (50.8, 71.12, 180) |
| `T` | top | (71.12, 50.8, 90) |
| `B` | bottom | (91.44, 101.6, 270) |

The `Sheetfile` caption is at (50.8, 102.235), 0.635 below the bottom edge.

## KiCad's own answer

Each case below was run through the server on a copy of the three files, then
that copy was force-resaved by KiCad 10.0.7. In every case KiCad kept
every pin and caption position the tool wrote; the resave only reorders each
pin's `uuid` and writes `(fields_autoplaced yes)`.

| Call | Written by the tool, kept by KiCad |
|---|---|
| `edit_sheet` `width` 101.6, `height` 63.5 | `R` (152.4, 71.12, 0), `B` (91.44, 114.3, 270), `L` and `T` unchanged, `Sheetfile` (50.8, 114.935) |
| `edit_sheet` `width` 30.48, `height` 17.78 | `R` (81.28, 68.58, 0), `L` (50.8, 68.58, 180), `B` (81.28, 68.58, 270), `T` unchanged, `Sheetfile` (50.8, 69.215) |
| `edit_sheet_pin` `L` to (132.08, 81.28) | (132.08, 81.28, 0): now on the right edge |
| `edit_sheet_pin` `R` to (132.08, 86.36) | (132.08, 86.36, 0): along its own edge |
| `edit_sheet_pin` `T` to (132.08, 50.8) | (132.08, 50.8, 90): the top-right corner, still a top pin |
| `edit_sheet_pin` `B` to (50.8, 101.6) | (50.8, 101.6, 270): the bottom-left corner, still a bottom pin |
| `edit_sheet_pin` `L` to (132.08, 50.8) | (132.08, 50.8, 0): the top-right corner, not on the left edge, so the first of right and top |

The regressions in `sch_hierarchy.rs` (`sheet_pin_resize_tests`) assert these
literal values.
