### `route_specctra_dsn` takes `neck_to_min_track_width` (minor release)

A Specctra DSN only carries the net class widths, so Freerouting never routes
a trace narrower than its class, even where the board's minimum track width
would let a connection through. The new optional boolean
`neck_to_min_track_width` (default false) sends the KiCad project's
`min_track_width` as Freerouting's `neck_width_um`: a connection that fails at
its class width is retried at that width.

- Freerouting narrows the whole connection, on any net, power and mains
  classes included, and KiCad's DRC does not flag a track narrower than its
  net class. Check the widths of power nets after routing.
- The width is sent as if the caller had set `router_settings.neck_width_um`:
  if Freerouting does not apply it, the run is refused before routing, like
  any other requested setting, rather than listed in
  `project_rules.not_applied`.
- An explicit `router_settings.neck_width_um` wins, and nothing is then read
  from the project.
- With the argument set, a call is refused with `invalid_argument` on
  `neck_to_min_track_width`, before routing, when no project is read (none or
  several found, see `project_path`) or when its `min_track_width` is absent
  or not above 0.

The answer's `project_rules` object has a new `neck_width_um`: the width sent
from the project, in um, or null.

`min_track_width` is not sent by default: calls without the new argument route
as before.
