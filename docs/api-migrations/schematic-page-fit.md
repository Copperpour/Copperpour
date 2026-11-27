### Check schematic page fit (minor release)

Load `sch_readability` and call `check_schematic_page_fit` with `schematic`, a path to
one saved `.kicad_sch`. It does not read unsaved editor changes, move items, or
resize the page.

- `drawing_sheet` identifies the built-in or project-configured worksheet. Custom
  worksheet names can be relative to the project or start with `${KIPRJMOD}/`.
  An unresolved setting never falls back to the built-in worksheet.
- `drawing_area` gives the inner frame and title-block rectangles in millimetres.
  For a custom sheet, Copperpour needs the conventional repeated frame and a
  rectangle marked `rect around the title block`. If the area cannot be established,
  `drawing_area_unavailable` explains why. Neither rectangle is used for findings
  unless both can be recognized.
- `findings` gives each conflicting item's UUID, kind, bounds, `frame_overflow_mm`
  by side, and `title_block_intrusion`. The frame is measured at the stroke
  centreline. Wires and buses use centrelines; other items use saved geometry
  and stroke-font bounds. An enclosing box can prove that a curve or symbol
  shape clears the title block. If the box overlaps but the check cannot
  establish whether the ink does, the item is reported as unmeasured.
- `fits` is `false` when any finding exists, `true` only after a complete check
  with no findings, and `null` otherwise. See `coverage_complete`, `measured_count`,
  and `unmeasured` for missing coverage. A partly measured item can appear in
  both `findings` and `unmeasured`. Its reported bounds cover the parts that
  were measured.

This is not a KiCad renderer. It does not resolve intersheet references, wrapped
text boxes, library-symbol graphic text, decorated pins, pin alternates, stacked
pin numbers, local-power icons or DNP overlays. Text estimates do not cover all
project fonts, pen widths, label or overbar ratios, or custom multi-unit
suffixes. Where netclasses have differing wire widths, a junction or bus-entry
stroke may need connectivity information to determine its plotted width.
Custom paper sizes, images and unsupported text styles, variables and glyphs
also prevent a full check. A reused sheet is not rendered separately for each
placement; differing References and units are flagged as unmeasured.

`get_schematic_layout`, `check_schematic_overlaps` and `set_schematic_page` are
unchanged.
