### Add `preview_block_placement` (minor release)

Load `sch_readability` to preview supporting-part placement around an anchor on one saved
schematic. Select the anchor and movable members by placed UUID and unit, assign bypass
capacitors to a specific anchor supply pin, and identify fixed objects by UUID. Membership
suggestions report the matching and competing pins separately from the proposed edits.

The tool reads one saved revision. It does not write, read unsaved editor state, or run
KiCad. The result gives the saved revision, score comparison, blockers, and any plan.
A `grouping_adds_wire` plan is a wiring tradeoff, not a cleanup. A `null` plan means no
edits are proposed. Recheck the revision and actual result before accepting changes.

The anchor's symbol pose is fixed. Its visible Reference and Value may be moved when
they interfere with the layout, unless the anchor UUID is explicitly fixed. A note can
move only if selected by UUID. A connector must have a drawn path to its paired port;
the same rail name on separate drawn islands is not sufficient.

Plan poses and field positions are absolute, in millimetres and degrees. Replay field
moves as explicit placements rather than leaving KiCad's field autoplace enabled.
The plan identifies removals by UUID and specifies new wires and junctions by coordinates;
a caller that constructs a new sheet must allocate their UUIDs.
