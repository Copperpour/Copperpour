### `find_single_pin_nets` checks global and power nets across the project (minor release)

A net named by a global label or a power symbol is now checked across the
saved hierarchy of the project that owns the given sheet, as
`resolve_schematic_ownership` establishes it, over every top-level sheet the
project lists. Pins are counted once per sheet instance, as KiCad's netlist
counts them: a pin on a sheet placed twice is two pins, and this includes the
given sheet. When the design gives such a net a second pin, it is no longer
listed in `nets`; its name is in the new `cross_sheet_resolved` array.

`cross_sheet_unverified: false` on a listed net now means it was checked
project-wide, unless a hierarchical label also names it. The new
`sheets_checked` gives the number of sheet files read.

With no owning project, ambiguous or unproven ownership (for example two
`.kicad_pro` files that both could own the sheet), or a hierarchy that cannot
be walked, the response is as before and carries neither new key.
`single_pin_net_count` counts the nets still listed.
