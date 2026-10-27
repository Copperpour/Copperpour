### `set_design_rules` sets via annular width and hole clearance minima (minor release)

`set_design_rules` accepts two more optional board-wide minima, in mm:
`min_via_annular_width` (0 to 25) and `min_hole_clearance` (0 to 100), the
ranges KiCad 10.0.6 loads. `min_via_annular_width` applies to plated
through-hole pads as well as vias. `get_design_rules` and the
`set_design_rules` response report both under `rules`, which now holds ten
minima. As with `min_copper_edge_clearance`, `min_text_height` and
`min_text_thickness`, a value outside the range is refused as
`invalid_argument`, naming the field, and nothing is written.
