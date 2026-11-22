### Named-pin power symbols get a wire stub by default (minor release)

`add_power_symbol` with `reference` and `pin_number` now adds a straight 2.54 mm stub and
puts the power symbol at its end. Set `stub_length` to a multiple of 1.27 mm to change
the length, or to `0` to keep the old placement and automatic rotation on the pin tip.
An explicit `rotation` still wins. Coordinate placement with `x` and `y` is unchanged.
Only named pins accept a nonzero `stub_length`.

Without `rotation`, the symbol keeps its library direction where clear and faces away
from the pin's body when it would otherwise collide or point back at the symbol.
Successful by-pin placements report `wire` (or `null` at zero length), `orientation`
and `text_overlaps`. The overlap list includes collisions involving the new power value
or stub, and any neighbouring pin's stub corridor the symbol crosses. An automatic
fallback or a remaining corridor crossing comes with an `orientation_reason`. An empty
`text_overlaps` means no findings among the geometry that could be checked.
`bounds_unresolved` lists symbols whose drawing bounds could not be checked. Missing
drawing bounds do not block a stub when all pin positions are known. A pin already
connected to a net-naming power symbol still returns `already_connected` without writing.

`batch_add_power_symbol` keeps the shape and orientation of accepted bars. Pin tips
must line up exactly for a bar; a nearly aligned row uses separate stubs so none of its
pins are left disconnected. The obstacle check also catches undotted crossings and
directive labels. An unjoined pin gets its own
stub and the same automatic rotation as `add_power_symbol`. Its clearance reports
are in `groups[].symbol.text_overlaps` and `groups[].symbol.bounds_unresolved`.
An optional `rotation` in each `pins` entry applies to its own stub. Joined bars list
pins whose overrides were ignored in `groups[].ignored_rotations`. Look for
`wire_uuids` even when `joined` is false; that still means
the pin does not share a bar. `stub_length` must still be positive for a batch.

A blocked stub returns `plan_blocked` before writing that stub. In a batch, check
`errors` for blocked pins: other pins may have been placed. If no new stub can be
placed, the batch returns an error without writing. Inspect the reported obstacle and
change the layout or stub length before retrying only the blocked pins.
To place a single power symbol on an already-wired tip without adding another wire, use
`stub_length: 0`.

If any symbol on the sheet has no usable pin geometry, no new stub can be checked.
`plan_blocked` names the unresolved symbol. A batch lists each affected pin in `errors`
and writes nothing if all are blocked. Restore the embedded definition or explicitly
use `add_power_symbol` with `stub_length: 0` on a pin that can be resolved.
