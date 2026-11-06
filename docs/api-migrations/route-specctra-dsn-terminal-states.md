### `route_specctra_dsn` fails at once on `TIMED_OUT`, `TERMINATED` and `INVALID` (minor release)

A Freerouting job that ends `TIMED_OUT` (its `job_timeout_seconds` reached),
`TERMINATED` or `INVALID` now fails the call at the next poll, with the state in
the error. These states were not seen as terminal: the call polled on until
`overall_timeout_seconds` and then failed with a timeout. Freerouting refuses
the output of all three, so no board was ever returned.

The error says to bound the optimizer with `optimize_seconds` rather than
`job_timeout_seconds`. Calls whose job completes, fails or is cancelled behave
as before.
