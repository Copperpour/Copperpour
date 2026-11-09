### `route_specctra_dsn` keeps the routed board when its diagnostics log cannot be written (minor release)

`route_specctra_dsn` refuses to replace `<ses stem>.freerouting.log`, and it
already refuses before routing when that file exists. A file of that name
that appeared during the run made the call fail with only "Freerouting
diagnostics output already exists" once routing was over: the routed board was
left behind as a hidden temporary SES next to the requested output, and a
failed run's own cause was not reported.

The routing now decides the call:

- A routed board is written to `ses_output_path` as usual, no temporary SES
  stays behind, and the existing log is left untouched.
  `artifacts.diagnostics_path` is `null` and the new
  `artifacts.diagnostics_error` says why Freerouting's stderr was not saved.
  It is `null` whenever the log was written or there was nothing to write.
- A failed run reports its own cause first, then "(Freerouting stderr not
  saved: …)", then the end of Freerouting's stderr as before.

Callers that treated the log error as a routing failure should read `success`
and, if they need the log, `artifacts.diagnostics_error`.
