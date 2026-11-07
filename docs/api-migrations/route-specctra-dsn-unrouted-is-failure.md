### `route_specctra_dsn` answers `success: false` when connections stay unrouted (minor release)

Freerouting ends a job `COMPLETED` when `max_passes` runs out or no pass makes
progress, whatever is left unrouted, and `route_specctra_dsn` answered
`success: true` for it. It now reads the job's last unrouted count from
Freerouting's log and, when it is not zero, answers `success: false` with a
`message` naming the count. The SES is still written, for inspection.

Two fields are added: `unrouted_connections` (the count, or `null` when the log
could not be read) and `message` (`null` on success). An unknown count leaves
`success: true`, as before. Callers that took `success: true` as "the board is
routed" now get the stricter answer; callers that want a partial SES anyway can
still read it from `artifacts.ses_output_path`.
