### `route_specctra_dsn` reports progress and returns `last_activity` (minor release)

A `tools/call` that carries `_meta.progressToken` (a string or a number) now
receives MCP `notifications/progress` while `route_specctra_dsn` runs: the
routing pass with its unrouted connections and violations, then each optimizer
pass with its score, read from the job's own lines in Freerouting's log. An
unchanged job still reports once a minute. All of a call's notifications are
written before its response. Calls without a token get no notifications.

The result gains `last_activity`, the last state read from the log (for example
`"pass 17: 0 unrouted, 218 violations"`), or `null` when the log could not be
read. Callers need no change.

The log is `$FREEROUTING_LOG` when set, else
`$XDG_STATE_HOME/freerouting/logs/freerouting.log` (by default under
`~/.local/state`), where Freerouting writes it on Linux. Elsewhere, set
`FREEROUTING_LOG` to the file Freerouting writes; without it, progress carries
only the job state and pass number, and `last_activity` is `null`.
