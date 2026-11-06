### `route_specctra_dsn` settings now take effect; optimizer passes and threshold (minor release)

`max_passes` and `job_timeout_seconds` used to reach Freerouting as `maxPasses`
and `jobTimeoutString`. Freerouting 2.5 accepts both keys and ignores them: its
`get_effective_settings` still showed unlimited routing passes and a 12-hour job
limit. They are now sent as `autorouter.max_passes` and `job_timeout`, the names
Freerouting reads, so both now apply. A caller that passed them gets the
behavior it asked for: routing stops after `max_passes`, and a job that reaches
`job_timeout_seconds` ends `TIMED_OUT`, without output.

Two new optional arguments:

- `optimizer_max_passes` (1 to 100): at most this many optimizer passes
  (Freerouting's `optimizer.max_passes`, default 100).
- `optimizer_improvement_threshold` (0 to 100, percent): the optimizer stops
  after a pass that improves its score by less (Freerouting's
  `optimizer.improvement_threshold`, default 2.5). Values above 0 and below
  0.1 are refused: Freerouting reads them as fractions and multiplies them by
  100, so 0.05 would run as 5%. 0 is sent as given.

Values out of range are refused before Java starts. Calls without these
arguments behave as before.
