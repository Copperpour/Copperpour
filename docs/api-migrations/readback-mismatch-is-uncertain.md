### A failed readback after a write is `mutation_outcome_uncertain` (patch release)

A file tool whose write replaced the file, but whose readback then found
something else there (another program saved it in between), now answers
`mutation_outcome_uncertain` with the tool's name as `operation`. It used to
answer as if nothing had been written: `conflict` from `set_footprint_metadata`,
`set_footprint_graphics`, `set_footprint_models` and `copy_routing_pattern`,
`stale_target` from `delete_schematic_component`, `batch_delete`,
`batch_delete_schematic_components`, `enrich_datasheets` and `fix_connectivity`,
and a plain or `handler_error` message, "write conflict: … changed since it was
read", from the others.

`conflict` and `stale_target` now mean nothing was written, so a retry is safe.
On `mutation_outcome_uncertain`, reload the file and inspect it before retrying.
