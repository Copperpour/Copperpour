### Schematic net names prefer global over local power drivers (patch release)

When a saved net carries both a global and a local power symbol, schematic tools now
report the global power name even if the local name sorts first. A global label still
outranks both power drivers. Arguments, response keys, pin membership and authored
aliases are unchanged. Use the reported name for display, not as an electrical identity.
You can still look up a net by either authored alias.

This affects `get_pin_net_name`, `get_pin_connections`, `get_component_nets`,
`trace_from_point`, `get_connected_items`, `check_schematic_wire_runs`, and the per-pin
names in `export_netlist_summary`. The export's top-level `nets` list and
`list_schematic_nets` still list authored names. `audit_decoupling`, `audit_connections`,
`audit_power_rails`, and `run_design_review` use the corrected name in affected findings.

`compare_schematic_layout` still blocks changes to a local alias and reports which alias
changed. It no longer also reports a primary rename or a change to the retained global
alias when that global power driver remains the winner. Existing routing refusals may
name the conflicting net differently; the rule for what a route may join is unchanged.

Hierarchy reports from `audit_power_rails` and `run_design_review` pool only globally
named rails across sheets. A power pin crossing a wire without a junction does not name
the wire, and a power symbol's selected passive pin alternate is not a power driver.

Where hidden `power_in` pins on ordinary symbols already name nets, they now outrank
local power symbols, matching KiCad's `SCH_PIN::IsGlobalPower`; this affects
`compare_schematic_layout`, `plan_schematic_cleanup`, `preview_block_placement`, and
`check_connector_pin_signals`, plus the per-sheet decoupling checks in
`audit_decoupling` and `run_design_review`.
