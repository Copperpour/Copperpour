### `check_connector_pin_signals` reports unnamed connector signals (minor release)

Load `sch_readability` and pass `schematic`, the path to one saved `.kicad_sch` file.
Evidence comes from that sheet alone. The tool does not read unsaved editor state or
modify the sheet.
The check refuses if any placed symbol on the sheet has unresolved pin geometry, even
if that symbol was not selected. Its pins could affect the reported connections.
It checks symbols whose reference prefix is exactly `J` by default. An explicit
`references` array replaces that rule. An empty array selects nothing. Power symbols
are excluded even when requested; references that select nothing appear in
`unmatched_references`.

`connectors` lists the selected symbols with their units, selection rule, and pins,
in natural reference and pin-number order. Each pin has a `status`, `evidence`, and
`context`. Only `unidentified` pins appear in `findings` and `unidentified_count`.
`no_connect` and `unconnected` are reported separately; use `find_orphan_items` or
ERC to look for bare pins.

Evidence can include `label`, `power_symbol`, `sheet_pin`, and the connector's own
meaningful, visible `pin_name`. The `context` includes the nearest label or power
symbol on the pin's own wire run, if present, with a straight-line `distance_mm`.
An evidence source elsewhere on the merged net has no nearest item on this run,
so its `nearest_label` or `nearest_power_symbol` is `null`.
The tool applies no distance threshold. For an unidentified pin, `context` and the
finding include the net name when available and `other_pin_names` on that net.
These names explain what is connected, but do not identify the connector pin on
the drawing. A wire stub with no other pin can be `unidentified` even when KiCad
names its net `unconnected-(…)`.

To spell net names derived from pins, the tool looks up the owning project's unit
notation. `unit_notation_source` is `project` when project settings are available,
or `kicad_defaults` otherwise. Project lookup does not add evidence from other sheets.

Existing connectivity tools and their responses are unchanged.
