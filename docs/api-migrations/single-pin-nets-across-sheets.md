### `find_single_pin_nets` checks global and power nets across the project (minor release)

A net named by a global label or a power symbol is now checked on every sheet
of the project's saved hierarchy, read from the root sheet named like the
`.kicad_pro` beside the given sheet. When other sheets give it a second pin it
is no longer listed in `nets`; its name is in the new `cross_sheet_resolved`
array. A listed net that was checked this way reports
`cross_sheet_unverified: false` unless a hierarchical label also names it. The
new `sheets_checked` gives the number of sheets read. With no project file, or
a hierarchy that cannot be walked, the response is as before and carries
neither new key. `single_pin_net_count` counts the nets still listed.
