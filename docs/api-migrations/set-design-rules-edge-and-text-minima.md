### `set_design_rules` sets copper-to-edge clearance and silkscreen text minima (minor release)

`set_design_rules` accepts three optional board-wide minima, in mm:
`min_copper_edge_clearance` (-0.01 to 25), `min_text_height` (0 to 100) and
`min_text_thickness` (0 to 25). `get_design_rules` reports them under
`rules`, beside the existing five. The ranges are the ones KiCad 10.0.6
loads. It replaces a value outside them with its default when it opens the
project, so Copperpour refuses such a value as `invalid_argument`, naming the
field, and writes nothing.

The `set_design_rules` response also carries `rules`, the eight minima as
re-read from the written project. A rule that already holds the requested
value, compared in nm as KiCad stores it, is no longer rewritten or listed in
`changed`, and a call that changes nothing leaves the project file untouched.
If the written file cannot be re-read, or does not hold a requested value,
the call returns `mutation_outcome_uncertain`.
