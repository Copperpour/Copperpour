### `report_issue` and `discard_pending_reports` meta-tools and `[issue_reporting]` config (minor release)

Added optional Sentry issue reporting behind the default-on `issue-reporting` Cargo feature (omitted from `--no-default-features` offline builds):

- New meta-tools `report_issue` and `discard_pending_reports` (available via `call_tool` only when `call_tool_allow_writes = true`).
- New `[issue_reporting]` config keys (`enabled`, `capture`, `automatic`) and env overrides `COPPERPOUR_ISSUE_REPORTING=0` and `DO_NOT_TRACK=1`.
- Automatic sending is off by default; panics, `handler_error`, and `mutation_outcome_uncertain` results are scrubbed and stored locally as pending reports and surfaced via `pending_reports` in tool results and `initialize` instructions.
