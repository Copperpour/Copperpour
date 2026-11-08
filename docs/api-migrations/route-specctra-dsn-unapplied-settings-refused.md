### `route_specctra_dsn` refuses settings Freerouting did not apply, before routing (minor release)

The job settings are read back from `get_effective_settings` before the job is
started. A setting Freerouting does not have or holds with another value (an
unknown key, a typo, a value it replaced) was listed in `settings_not_applied`
once the whole run was over, and the board was routed without it. The call now
fails at that point, before anything is routed, with an `invalid_argument`
error on `field: "router_settings"`. Its `reason` names each such setting as
`"path: not a Freerouting setting"` or `"path: sent X, effective Y"`, joined by
`"; "`. No SES is written.

`settings_not_applied` stays in the result, for callers that read it, and is
now always empty. Fix the settings named in the error and call again. A
Freerouting server without `get_effective_settings` is not checked, as before:
the routing runs and `settings_checked` is `false`.
