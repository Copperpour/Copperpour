### `route_specctra_dsn` reports progress (minor release)

A `tools/call` that carries `_meta.progressToken` now receives MCP
`notifications/progress` while `route_specctra_dsn` runs: the routing pass with
its unrouted connections and violations, then each optimizer pass with its
score, read from the job's own lines in Freerouting's log. An unchanged job
still reports once a minute. Calls without a token behave as before, and the
tool's result is unchanged. A client that wants progress sends a
`progressToken`.
