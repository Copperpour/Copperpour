### `plan_schematic_split` measures a block's sheet interface (minor release)

Load `sch_readability` to call `plan_schematic_split` directly, or use `call_tool`.
Pass the saved `schematic` path and `symbols`, an array of `{"uuid": "...", "unit": 1}`
selectors for placed parts. Include every placed unit of a selected part. The reply
identifies the saved sheet instance and its file `revision`, excluding unsaved edits.
Unrelated unannotated references such as `R?` do not block a plan, but a selected
reference with multiple placements of the same unit still does.

The report counts each net in the saved sheet's graph once. A sheet pin, hierarchical
label or global name marks a declared exit or scope, not a verified connection through
another sheet. The report lists internal nets, boundary pins and scoped aliases, and
separates signal ports, rails and project-wide labels. A pin's `ordinal` is its
zero-based index in the resolved pin list for that placed unit, so stacked pins
with the same pin number can be distinguished. Confirm every suggested pin name
and direction. Suggested names preserve KiCad's active-low overbar (e.g. `~{SRCLR}`).
Keep existing global scope or stop; do not promote
a local signal to a global label. `recommendation` is `keep` when the block hides no
nets or exposes more signals than it hides. `split` is readability advice only.

`supporting_parts` lists two-terminal passive parts outside the selection that share
drawn wiring with it. `stranded_parts` identifies those without a nearby retained peer
or sheet port on their drawn islands. Distance is measured between pin endpoints.
A shared name alone never makes a part a candidate. Review `boundary_wires`,
`boundary_labels` and `boundary_power_symbols` before splitting. Wires include
the whole crossing island, not just the segments you might cut. Power symbols
on that island are listed separately from block-only power symbols.

The tool refuses reused or ambiguous sheet instances, even when different
placements share one path. It also refuses unresolved geometry or scopes,
buses, and conflicting port names. Pin names and direction use the saved active
alternate. The tool refuses a power name that requires text-variable expansion.
It does not run KiCad, resolve project-wide connectivity, or read the PCB.
No project files are written. Compare the finished design separately if you proceed.
