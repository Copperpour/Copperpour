### Schematic symbol writers require quarter turns (minor release)

Use an exact multiple of 90 degrees for `rotation` in `add_schematic_component`,
`batch_place_components`, `rotate_schematic_component`, and `add_power_symbol`
with either selector. Values such as `90.5` and `45` no longer write a symbol.
Equivalent turns such as `-90` and `450` still work. The response reports what was
saved, including the canonical angle and mirror pair for a reflected symbol.
Field text angles are separate and unchanged.

- The `x` + `y` handler for `add_power_symbol` now returns `invalid_argument`
  on `rotation` for non-numeric values it previously ignored on direct calls.
  The MCP schema already refused those values.
- `batch_place_components` lists rejected entries as `invalid_argument` in `failures`
  and can save the valid entries. Correct and retry only the failed entries.
- `batch_add_power_symbol` refuses the call with `invalid_argument` naming
  `pins[i].rotation` if it would use an invalid override to place a symbol. Joined
  bars still list unused overrides in `ignored_rotations`. When stacked pins share
  an individual stub, their overrides must be valid and agree.
- `connect_pins` and `batch_connect_pins` also refuse a route that would join an
  unrelated pin at the position KiCad reads from a legacy saved orientation.
  They still try the other elbow when only one is blocked. If the saved sheet
  contains a symbol angle KiCad cannot load, repair the sheet before routing.
  Verified pin contact on a loadable sheet still returns `already_connected`
  without writing.
- `rotate_schematic_component` and `mirror_schematic_component` return
  `plan_blocked` without writing if a selected unit already has a non-quarter-turn
  angle. Open the sheet in KiCad and correct or resave its orientation before retrying.
  If KiCad cannot open the sheet, restore a loadable copy first.
