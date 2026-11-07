### `route_specctra_dsn` sizes its default overall timeout for `optimize_seconds`, and refuses one that leaves no room for it (minor release)

The optimizer window opens only once routing is done, but the overall timeout
defaulted to a flat 900 s: with `optimize_seconds` above that, the call was
killed while the optimizer was still inside its window, and no board was kept.
Without `overall_timeout_seconds`, the default is now 900 plus
`optimize_seconds`, at most 86400. A call without `optimize_seconds` keeps
900.

A call that gives `optimize_seconds` with an `overall_timeout_seconds` that
does not exceed it now fails with `invalid_argument` and `field`
`overall_timeout_seconds` before the JAR is looked up, so no Java process
starts and no SES is written. Raise `overall_timeout_seconds` above the
routing time plus `optimize_seconds`, or drop it to take the default.

The largest `optimize_seconds` is now 86399, in the schema and in the
tool's own check: `overall_timeout_seconds` is at most 86400 and must exceed
it, so a window of 86400 could never run. A call with `optimize_seconds`
86400 now fails at the schema with `invalid_argument` and `field`
`optimize_seconds`: lower it to 86399 at most.

The `overall_timeout_seconds` schema no longer carries `"default": 900`; its
description states the default.
