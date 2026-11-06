### `route_specctra_dsn` accepts `optimize_seconds` (minor release)

New optional argument `optimize_seconds` (0 to 86400). Once routing is complete
and Freerouting's optimizer has started (`optimizer_enabled`), the call lets it
run that many seconds, then keeps the best board found so far, stops the job and
writes that board to `ses_output_path`. The result then has `final_state`
`OPTIMIZATION_STOPPED`. The board is complete: Freerouting republishes its best
board at the start and end of every optimizer pass, and works candidates on
copies.

The optimizer is seen in Freerouting's log, read as for progress, or in the
job's `stage`. When neither shows it, the optimizer runs to its end as before.

Until now the only bounds were `overall_timeout_seconds`, which kills the job
and keeps nothing, and `job_timeout_seconds`, whose `TIMED_OUT` job has no
output in Freerouting. Its description now says so. Calls without
`optimize_seconds` behave as before.
