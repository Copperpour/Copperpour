### Add `find_pin_contacts` (minor release)

Load `sch_readability` and call `find_pin_contacts` with `schematic` to examine one saved
sheet without KiCad. `contacts` lists coincident visible pins of distinct placed symbols
and the wires, junctions and labels at each contact. These wires are geometric
witnesses, not a statement that the net graph joins them to the pins.
`short_wires` lists unbranched
pin-to-pin runs shorter than `min_wire_mm` (default `2.54` mm when omitted or `null`;
`0` reports contacts only). Each run includes its endpoints, total length and wire UUIDs.
The check compares the length rounded to six decimal places in millimetres to the
supplied minimum.

Each contact is centered on a pin tip. All its members are within 0.01 mm of that
tip in each axis; a chain of nearby tips does not extend the group beyond that
tolerance. Wire joints and pin-to-wire attachments use the net graph's endpoint
identity, not the contact tolerance. A zero-length wire only counts as a run if its
position is shared by visible tips of different symbols. Hidden pins, including
hidden pins on older power symbols, do not make contacts or affect runs.
The response also includes `contact_count` and `short_wire_count`.

These are drawing-style findings, not overlap or electrical errors. No file changes;
other checks and tool calls are unchanged.
