### Schematic component tool refusals carry structured `error.kind` envelopes (minor release)

Refusals from `add_schematic_component`, `edit_schematic_component`,
`get_schematic_component`, `get_schematic_pin_locations`,
`add_component_annotation`, `group_components`, `update_symbols_from_library`,
`replace_component`, and `move_connected` now return structured JSON error
bodies (`{"message": ..., "error": {"kind": ...}}`) instead of plain-text
`handler_error` messages (#741). In `batch_place_components`, out-of-range
`unit` failures now report `"invalid_argument"` in `failures[i].kind` instead
of `"handler_error"`.

The existing human-readable refusal text is preserved verbatim in `message`:

- `invalid_argument`: out-of-range `unit` in `add_schematic_component`,
  `batch_place_components`, and `replace_component`; built-in property names
  passed to `add_component_annotation`; empty `references` in
  `group_components`; `unit` overrides on multi-unit components in
  `replace_component`; and invalid `fields`, `field_placements`, or missing
  multi-unit `unit` arguments in `edit_schematic_component`.
- `stale_target`: missing component references in `get_schematic_component`,
  `get_schematic_pin_locations`, `update_symbols_from_library`, and
  `replace_component`, plus missing target fields or units in
  `edit_schematic_component`.
- `malformed_file`: missing embedded `lib_symbols` definitions or unflattened
  `(extends ...)` stubs in `get_schematic_pin_locations`; malformed or missing
  `lib_id` blocks or inconsistent unit `lib_id`s in `replace_component`; and
  malformed symbol, property, or `(instances ...)` blocks in
  `add_component_annotation` and `edit_schematic_component`.
- `plan_blocked`: replacing a multi-unit component with a symbol that lacks one
  of its placed units in `replace_component`, adding a custom property to
  missing units when existing units mix private and public copies in
  `add_component_annotation` or `edit_schematic_component`, and zero-effect
  field edits in `edit_schematic_component`.
- `unsupported_capability`: calling unimplemented `move_connected`.

Callers inspecting tool error text should read the `message` field of the JSON
error body or branch on `error.kind`.
