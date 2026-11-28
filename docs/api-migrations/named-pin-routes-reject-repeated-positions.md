### Named-pin routes reject repeated symbol positions (patch release)

`connect_pins` and `batch_connect_pins` refuse to draw a route if any placed
symbol on the saved sheet has more than one direct `(at ...)` record. KiCad
rejects a malformed second record and uses the last record when both are
valid; Copperpour's pin lookups use the first.

The refusal names the symbol in `plan_blocked` or the batch `errors` list
and does not write. Restore a loadable sheet or verify the position in KiCad
and resave it before retrying.
