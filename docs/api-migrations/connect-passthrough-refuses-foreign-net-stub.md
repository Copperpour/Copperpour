### `connect_passthrough` refuses a stub that joins another net (patch release)

`connect_passthrough` now checks its 2.54 mm stub with `clear_stub_direction`
before writing, matching `connect_to_net`:

- With `direction: "auto"` (the default), a stub that would end on or run over a
  terminal of another net turns across instead of joining it.
- With a named `direction`, or with `direction: "auto"` when every direction
  would join another net (or when placed symbol pin geometry cannot be
  resolved), `connect_passthrough` refuses with `plan_blocked` and writes
  nothing.
- A stub that meets a pin or label already on the requested net still succeeds.
