### `connect_to_net` stubs refuse to join another net (patch release)

`connect_to_net`, and `batch_connect_to_net` with `stub_length` above 0, drew
the stub with no check of what it met. A stub ending on another pin, or
running over one, dotted it, so that pin joined the requested net and the call
still reported success
([#117](https://github.com/Copperpour/Copperpour/issues/117)).

A stub is now checked before it is written. It may join only what a label
named `net` on the pin would: the pin's own net, and every item already on
`net`. In `batch_connect_to_net` that includes every pin the call names.

- With `direction: "auto"`, a stub that would join another net turns across
  the pin instead, to either side, and never runs back over the symbol. A pin
  given by coordinates whose outward direction is unknown tries all four. The
  response's `direction` names the one drawn. A stub turned up or down keeps
  the outward label rotation, so its text still runs away from the symbol.
- When the named direction, or every `auto` one, would join another net, or a
  placed symbol's pins cannot be resolved so the check cannot see them,
  `connect_to_net` refuses with `plan_blocked` and writes nothing. `reasons`
  holds one entry per direction tried.
- `batch_connect_to_net` skips that pin, adds
  `"<reference>.<pin_number>: <reasons joined by '; '>"` to `errors`, and still
  writes the other pins.
- `batch_connect_to_net` now sets `isError` when it connected no pin and
  `errors` is not empty, as `batch_connect_pins` does. That also covers a call
  whose every pin failed to resolve, which used to report success.

A zero-length stub, a label on the pin, is unchanged.
