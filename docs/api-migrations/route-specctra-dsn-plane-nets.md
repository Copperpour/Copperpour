### `route_specctra_dsn` takes `plane_nets` and `plane_as_obstacle` (minor release)

Two new optional arguments, passed to Freerouting's job settings under its own
names:

- `plane_nets` (array of net names): nets Freerouting treats as power-plane
  nets, with plane routing mode and cheaper vias to the plane. A `(plane ...)`
  declared in the DSN already marks its net the same way, so this matters for a
  DSN whose planes are not declared.
- `plane_as_obstacle` (boolean): whether copper pours block foreign traces
  (Freerouting's default is false).

Calls without these arguments behave as before: nothing is sent for them.
