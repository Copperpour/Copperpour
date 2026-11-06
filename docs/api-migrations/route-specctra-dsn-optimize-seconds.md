### `route_specctra_dsn` accepts `optimize_seconds` (minor release)

New optional argument `optimize_seconds` (0 to 86400). Once routing is complete
and Freerouting's optimizer has started (`optimizer_enabled`), the call lets it
run that many seconds, then keeps the best board found so far, stops the job and
writes that board to `ses_output_path`. The result then has `final_state`
`OPTIMIZATION_STOPPED`. The board is the best one so far: Freerouting
republishes it at the start and end of every optimizer pass, and works
candidates on copies. It is complete only if the router finished: the result's
`last_activity` gives its unrouted count.

`overall_timeout_seconds` (900 by default) still bounds the whole call, and a
call that reaches it keeps no board. Raise it above the routing time plus
`optimize_seconds`.

The optimizer is seen in Freerouting's log, read as for progress, or in the
job's `stage`. When neither shows it, the optimizer runs to its end as before.

Until now the only bounds were `overall_timeout_seconds`, which kills the job
and keeps nothing, and `job_timeout_seconds`, whose `TIMED_OUT` job has no
output in Freerouting. Its description now says so. Calls without
`optimize_seconds` behave as before.
