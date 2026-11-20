### Schematic placement and rotation normalize orientation to KiCad's canonical pair (patch release)

`add_schematic_component`, `batch_place_components`, `add_power_symbol`, and `rotate_schematic_component` now normalize each symbol's `(rotation, mirror)` pair to the canonical form eeschema writes (`180° x` -> `0° y`, `180° y` -> `0° x`, `90° y` -> `270° x`, `270° y` -> `90° x`, and quarter-turn angles folded into `[0, 360)`), matching `mirror_schematic_component`.

The drawn body, pins, and field text are unchanged, and KiCad's next save no longer rewrites the stored pair. Callers that chain `rotate_schematic_component` should read the stored `rotation`, `mirror_x`, and `mirror_y` from the tool response rather than assuming the requested pair was stored verbatim.
