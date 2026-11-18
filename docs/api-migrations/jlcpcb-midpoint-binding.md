### JLCPCB export preserves unserved board binding reasons (minor release)

When `GetOpenDocuments` returns `AS_UNHANDLED` or `AS_UNIMPLEMENTED`, `export_manufacturing_package` with JLCPCB assembly and `generate_jlcpcb_order_link` now report `editor_unavailable` with `recovery.mutation_outcome: "none"` instead of a generic `handler_error`. Neither refusal starts an export. Tool arguments and successful responses are unchanged.

`AS_UNHANDLED` reports `no_pcb_editor_at_endpoint`. `AS_UNIMPLEMENTED` does not establish whether a PCB editor is open. The shared binding formatter includes both statuses in `recovery.evidence.status`, including for other callers that already returned these refusals. The export message now identifies the binding problem and says that no package was exported or uploaded. `error.reason` no longer has the `native_midpoint_unavailable:` prefix for these two cases.
