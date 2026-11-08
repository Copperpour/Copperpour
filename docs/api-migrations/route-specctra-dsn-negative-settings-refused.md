### `route_specctra_dsn` refuses negative `router_settings` numbers, before Java starts (minor release)

Freerouting takes a negative cost, count or distance without a warning and
routes with it, and none of its settings has a meaning below zero. A negative
number in `router_settings` was sent as given. The call now fails with
`invalid_argument` before the JAR is looked up, so no Java process starts and
no SES is written. `field` is the path of the first negative value, such as
`router_settings.scoring.via_costs`, and the message names every one, such as
`router_settings.scoring.via_costs is -5`.

Only the values that would be sent are checked: a `router_settings` value that
a named argument replaces (`max_passes` over `autorouter.max_passes`, for
example) is not sent and is not refused. Remove the sign, or the setting, and
call again.
