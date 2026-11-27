### Schematic connectivity uses saved coordinates (minor release)

The shared schematic net graph no longer uses the 0.01 mm tolerance for
drawn labels on horizontal or vertical wires. It checks the wire and label
after rounding to KiCad's saved schematic coordinates (0.0001 mm units),
including KiCad's one-unit allowance for label attachment. For example, a
label at `y = 88.901` does not name a horizontal wire at `y = 88.9`, but a
label at `y = 88.9001` still attaches.
Labels on separate drawn islands can still join those islands by name.

Junctions must land exactly on a horizontal or vertical wire in saved
coordinates.

Wire endpoints, pins, and query points now use 0.1 µm node identity rather
than 1 µm. `trace_from_point` can return `net: null` one saved unit past a
wire end even when it lists the nearby wire. `compare_schematic_layout` can
now block a candidate that leaves a pin just short of a wire endpoint.

`trace_from_point` can still list a nearby label in `labels_here` within the
requested proximity tolerance. That list does not mean the label names the
wire. `get_connected_items` and the other net queries may now omit that label,
its name, or pins and wires formerly attributed to it. Net audits, the built-in
netlist summary, and schematic readability checks use the corrected membership
too. An unnamed net may be reported by its pin-derived name by tools that
already supply one. `check_schematic_wire_runs` uses the electrical net name
without adding a drawn cut at an off-wire label.

Routing, region moves, layout comparisons, label renames, and template wiring
verification use the corrected graph for their electrical checks.
When connection routing chooses a straight wire, it refuses endpoints that would
save as a diagonal. Align the endpoints before retrying. Route guards also refuse to
clear routes on sheets containing an almost horizontal or vertical diagonal wire,
because the saved-coordinate graph cannot verify those joins.
Pin-targeted power placement uses the saved pin tip to check for an existing
rail connection and to place a new symbol or stub.
Review a refusal before retrying: an edit that moves an off-wire label onto a
wire can change the net. To restore an intended connection, snap the label's
anchor onto the wire, save the schematic, and re-run the check. No arguments
or result keys change. Proximity-only checks keep their existing tolerance.
