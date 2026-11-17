### Runtime IPC failures return structured `recovery` guidance (minor release)

Non-error health diagnostics (`check_kicad_ui`, `open_project`) that report
`ipc_failure` now include an additive `ipc_failure.recovery` object, and IPC
error responses returned through the shared helpers (`ipc_failure_result`,
`ipc_target_error_result`, `ipc_uncertain_result`, `ipc_recovered_result`,
`ipc_unserved_result`, and `editor_unavailable` IPC boundaries) now include an
additive top-level `recovery` object alongside `message` and `error`.

Each `recovery` object contains:
- `cause`: stable machine-readable cause (`not_configured`, `ambiguous_endpoint`,
  `no_listener`, `access_denied`, `handshake_failed`, `kicad_not_ready`,
  `no_open_documents`, `wrong_document`, `ambiguous_document`,
  `unresolved_document_identities`, `stale_document`, `ipc_outcome_unknown`,
  `ipc_batch_recovered`, `no_pcb_editor_at_endpoint`, `preflight_refusal`, or
  `unknown` when unclassified);
- `observed` and `evidence`: what was actually observed without guessing from
  message text;
- `mutation_outcome`: `"none"`, `"unchanged"`, or `"unknown"`;
- `retry_safe` and `retry_prerequisites`: `retry_safe` remains `false` whenever
  prerequisites apply or outcome/safety is unknown;
- `next_steps`: ordered steps tagged with `actor` (`"agent"` or `"user"`) and
  optional `tool`;
- `troubleshooting_url`: link to the corresponding section in
  `docs/TROUBLESHOOTING.md`.

`ipc_failure.kind`, `ipc_failure.message` and `ipc_failure.note` are unchanged,
and so is `error.kind`, with these exceptions:

- `get_board_stackup`, `get_nets_list`, `set_placed_footprint_models` and the
  live-board synchronization behind DRC and zone refill reported an IPC request with an
  unknown outcome, or a dropped and restored batch, as `editor_unavailable`.
  They now report `ipc_outcome_unknown` and `ipc_batch_recovered`, as every
  other IPC caller does, because the board may have changed.
- Tools that returned an IPC failure as plain text with no `error` object
  (`set_footprint_field`, `set_pad_net`, `apply_copper`, `delete_trace`, the
  `pcb_components` and `pcb_routing` IPC tools) now return a structured error:
  `editor_unavailable` when KiCad or its board editor did not answer,
  `handler_error` when KiCad refused the request.
