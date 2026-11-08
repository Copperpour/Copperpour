### `route_specctra_dsn` reads Freerouting's log incrementally (minor release)

Progress and `unrouted_connections` come from Freerouting's own log. Each poll
used to read only the last 256 KB of it, while one optimizer pass can write
over a megabyte of DEBUG lines between two lines that say where the job
stands. On a long optimizer run, progress froze on an old pass, and
`unrouted_connections` came back `null` on a fully routed board, so the call
could not tell a complete board from an incomplete one.

Each poll now reads what the log gained since the previous one, keeps a line
cut by a read until it is complete, and remembers the latest activity and
unrouted count. The first read still starts 256 KB from the end; a log that
becomes shorter than what was read is read again from its tail. Callers need
no change: on long runs, progress and `unrouted_connections` now follow the
job to its end.
