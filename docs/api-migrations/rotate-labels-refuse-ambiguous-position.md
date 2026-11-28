### Label rotation refuses ambiguous positions (patch release)

`rotate_schematic_label` and `batch_rotate_labels` now return `ambiguous_target`
when several labels of the requested name share the requested position.
Nothing is written, including earlier entries in a batch. Choose the intended
UUID from `error.candidates` and retry with `uuid` instead of `net`, `x` and `y`.
