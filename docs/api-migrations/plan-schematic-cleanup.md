### Add `plan_schematic_cleanup` (minor release)

Load `sch_readability` and call `plan_schematic_cleanup` with `schematic` and opposite
region corners `x1`, `y1`, `x2`, `y2` in millimetres. The optional `movable` list
defaults to empty. The tool considers branch-free pin-to-pin runs. It may move a
listed symbol along with power symbols and no-connect markers on its pin tips, or
move a listed power symbol onto a short stub. It can remove a listed redundant
power symbol only if the same rail is already driven on the drawn node.
An isolated power lead can also be rerouted without moving its power symbol,
including a `PWR_FLAG`. For a listed power symbol, the stub competes with the reroutes.
The region selects runs wholly inside the box and power contacts inside it. After
each step, the planner selects again with the same box on the in-memory sheet.
A carried power contact that enters the box can get a stub in the same preview.
A checked edit can extend beyond the box; those steps have
`extends_outside_region: true` and report the proposed positions.

The response is a preview of the saved sheet, not unsaved editor state. It returns
the same `revision` token as `list_schematic_texts`. Steps are ordered and include
the wire UUIDs to remove, old and new symbol and no-connect positions, the proposed
route, junction changes, any removed power symbol UUID, and the run length before
and after. `skipped` explains why eligible runs or listed power symbols stayed put.
Replay the steps in order. A later step can replace a wire introduced by an
earlier step, which lists the wire's ID in `wire_uuids_added`.
The preview stops when no further step qualifies. If more cleanup is possible
after 256 steps, it returns `plan_blocked` without a partial plan. Select a smaller
region and try again.

A new pin-to-pin run must be at least 2.54 mm long. Moving a power symbol onto a
stub uses the by-pin `add_power_symbol` rotation rule. If facing away from the
connected symbol would take a quarter turn, cleanup keeps the library direction
when it clears the surrounding drawing and pin corridors. It turns the power
symbol away when the library direction is blocked or would face back across the
connected symbol.

Each step must earn `improved` from `compare_schematic_layout` and pass its
preservation gate. Better score tiers take priority over shorter wire length.

The preview writes nothing and needs no KiCad process. It does not rename nets,
add labels or add sheets. Applying a plan is separate.
