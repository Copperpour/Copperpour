### Schematic connection tools no longer draw zero-length wires (minor release)

When two visible, electrically connectable pins touch on the saved schematic,
`connect_pins` succeeds without writing. Check for `already_connected: true`
with `x` and `y` in millimetres instead of `connected` and `path`. If either pin
is hidden, electrically unconnected or cannot be verified, the tool refuses
with `plan_blocked`. Save any changes in KiCad before relying on this result.

`batch_connect_pins` reports those successes in `already_connected`, with
`pair`, `x` and `y` for each. `connected_count` still counts only drawn routes.
Read `errors` even on success: a batch can report an existing contact alongside
refusals. With no drawn routes, the batch writes nothing.

`add_schematic_connection` refuses a route if KiCad would read any leg as a
zero-length segment. Coincident coordinates alone do not prove a connection.

Separately, named-pin routes now refuse if the selected body style does not place
a requested pin at the resolved position, or if KiCad would read the proposed wire
end away from that pin. The latter can happen when the symbol origin and local pin
offset round differently from the final wire coordinate. The router does not move
either endpoint to compensate. Save the sheet in KiCad and inspect the pin positions
before retrying. Existing zero-length wires are not removed.
