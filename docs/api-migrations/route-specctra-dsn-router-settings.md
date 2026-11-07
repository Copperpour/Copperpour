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
and the result has three new keys:

- `settings_checked` (boolean): `true` when the settings sent were read back
  and compared. `false` when nothing was sent, or when the Freerouting server
  has no `get_effective_settings` or its answer could not be read: then
  `settings_not_applied` is empty because nothing was compared.
- `settings_not_applied` (array of strings): each setting sent that Freerouting
  does not have, or holds with another value, as `"path: not a Freerouting
  setting"` or `"path: sent X, effective Y"`. Arrays are compared element by
  element, with the index in the path (`layers[0].bend_cost`): what an element
  leaves out, and elements that were not sent, are Freerouting's defaults and
  not reported, while a sent element missing from the effective array is
  reported as `"path[i]: sent X, not in the effective settings"`. Empty when
  everything took, or when `settings_checked` is `false`.
- `settings_warnings` (array): Freerouting's own `validation_warnings` for the
  effective settings.

The routing still runs when a setting did not take. Calls without
`router_settings` behave as before, apart from the three new keys in the result.
