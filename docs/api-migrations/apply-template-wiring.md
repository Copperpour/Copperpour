### `apply_template` executes its connection map (minor release)

`apply_template` previously only placed components despite promising wiring. It now
connects resolvable map entries using short wires and local net labels, with the
same collision checks as `batch_connect_to_net`. Component spacing allows room
below tall symbols; the response reports the resulting positions.

Inspect `connections_made` (verified against saved connectivity),
`connections_unresolved` (entries or pins that could not be wired), and
`unconnected_pins` (placed pins not wired by this call). An unresolved entry does
not prevent independent entries from being connected. `joined_existing_nets`
names verified joins to nets present before placement, including generated
internal names. Applied explicit mappings acknowledge their destination names.
Shared-pin chains are normalized into one net; a connected explicit name takes
precedence over generated names. Generated names use the actual reference and
pin number, so distinct pins with the same library name stay separate. Malformed
`net_mappings` and blank destination names now produce `invalid_argument` before
placement instead of silently using the original net.

`connections_to_wire` remains as a deprecated copy of the mapped connection guide
for compatibility; it is **not** a list of outstanding work. Use the new result
fields before deciding what still needs wiring. Pins absent from a connection
map remain unconnected; this tool does not infer missing circuitry or add
no-connect flags.

Placement and each net are separate revision-checked writes. A failure after
placement reports `mutation_outcome_uncertain` with placed references and any nets
already written. Inspect the saved sheet and finish remaining connections; do
not reapply the template, which would place another copy.
