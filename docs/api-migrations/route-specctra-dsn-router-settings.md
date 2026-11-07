### `route_specctra_dsn` takes `router_settings` and reports `settings_not_applied` (minor release)

New optional argument `router_settings` (object): any of Freerouting's router
settings, in the JSON shape its `get_effective_settings` returns (for example
`{"scoring": {"via_costs": 80}, "hole_clearance_um": 200}`), passed through as
given. The named arguments (`max_passes`, `optimizer_max_passes`,
`plane_nets`...) are deep-merged on top and win. Its improvement threshold,
`router_settings.optimizer.improvement_threshold`, is refused above 0 and
below 0.1 like `optimizer_improvement_threshold`, when no
`optimizer_improvement_threshold` replaces it.

Freerouting accepts keys it does not know and drops them without a word. After
the job settings are sent, they are read back from `get_effective_settings`,
and the result has two new keys:

- `settings_not_applied` (array of strings): each setting sent that Freerouting
  does not have, or holds with another value, as `"path: not a Freerouting
  setting"` or `"path: sent X, effective Y"`. Empty when everything took, when
  nothing was sent, or when the Freerouting server has no
  `get_effective_settings` to read them back from.
- `settings_warnings` (array): Freerouting's own `validation_warnings` for the
  effective settings.

The routing still runs when a setting did not take. Calls without
`router_settings` behave as before, apart from the two new keys in the result.
