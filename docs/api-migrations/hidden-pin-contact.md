### Pin routing recognizes hidden pin-tip contact (patch release)

`connect_pins` now returns `already_connected: true` with `x` and `y` when
verified hidden pins already connect by contact on the saved schematic. It
does not write a wire. `batch_connect_pins` lists these pairs in
`already_connected`; `connected_count` still counts only drawn routes.

No caller changes are needed. Both tools still refuse coincident `no_connect`
and `unconnected` pins, including selected alternates. For visible and hidden
pins alike, they now refuse coincident contact when a saved placement angle is
missing, nonnumeric or unsupported, or when a placement or library pin has
multiple position records. Saved instance paths must have a reference and a
unit, and their units must match the placed unit. If references differ, both
requested references must match the same applicable sheet path. In that case
the tools refuse noncanonical path spellings rather than risk confusing
two records for the same path. KiCad annotates each reuse of a sheet separately.
