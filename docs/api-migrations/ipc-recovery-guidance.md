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

Existing `error.kind`, `ipc_failure.kind`, `ipc_failure.message`, and
`ipc_failure.note` fields remain unchanged.
