### PCB board, zone, outline, and live-gate refusals carry structured `error.kind` envelopes (minor release)

Refusals from PCB board tools (`set_board_size`, `get_board_extents`,
`get_layer_list`, `add_layer`, `add_mounting_hole`, `add_zone`,
`add_copper_pour`, `import_svg_logo`), zone tools (`list_zones`,
`delete_zone`), `replace_board_outline`, file-only board writers guarded
against live KiCad (`set_board_stackup`, `set_design_rules`,
`set_predefined_sizes`, `copy_routing_pattern`, `place_decoupling_caps`,
`refine_placement_force_directed`, and netclass writers), and live-IPC board
writers (such as `set_component_placements`, `place_component`,
`flip_component`, `set_title_block`, and footprint/graphic writers) now return
structured JSON error bodies (`{"message": ..., "error": {"kind": ...}}`)
instead of plain-text `handler_error` messages (#741).

The existing human-readable refusal text is preserved verbatim in `message`:

- `invalid_argument`: non-canonical `layer_name` in `add_layer`, invalid
  `pad_connection`, missing or under-length `points`, and undeclared `net_name`
  on legacy boards in `add_zone` / `add_copper_pour`, SVGs without fillable
  `<path>` elements in `import_svg_logo`, missing/non-string `board` paths in
  `list_zones` / `delete_zone`, and live preflight argument refusals (missing
  or duplicate references, invalid rotation).
- `file_not_found`: missing board files in `get_board_extents`,
  `get_layer_list`, `add_mounting_hole` (file path), `list_zones`, and
  `delete_zone`.
- `malformed_file`: malformed board S-expressions in `get_board_extents`,
  `get_layer_list`, `add_layer` (missing or unbalanced `(layers)` block), and
  `add_mounting_hole` (file path or malformed resolved `.kicad_mod` library
  footprint).
- `plan_blocked`: non-segment `Edge.Cuts` outlines in `set_board_size`,
  exhausted inner copper layer IDs `1..=30` in `add_layer`, live `Edge.Cuts`
  shapes lacking UUIDs in `replace_board_outline`, and incomplete live
  preflight checks.
- `unsupported_capability`: live preflight refusals for unsupported footprint
  or board item types.
- `conflict`: file-only PCB writers (`set_board_stackup`, `set_design_rules`,
  `set_predefined_sizes`, `copy_routing_pattern`, `place_decoupling_caps`,
  `refine_placement_force_directed`, and netclass writers) refused when KiCad
  currently holds the exact board open, and concurrent revision conflicts in
  `add_mounting_hole`.
- `editor_unavailable`: live-IPC write refusals when KiCad's endpoint has no
  PCB editor open or does not implement `GetOpenDocuments`, with structured
  `recovery` guidance.
- `mutation_outcome_uncertain`: post-commit readback failures, missing
  footprints, or ambiguous references in `add_mounting_hole` (file path).
- `handler_error` with structured `recovery` guidance: live-IPC write
  rejections when KiCad answered with an error status or failed the live write
  (writers keep `handler_error` rather than `editor_unavailable`, matching
  other IPC write paths, because the editor was reachable and the mutation
  outcome is unknown).

Callers inspecting tool error text should read the `message` field of the JSON
error body or branch on `error.kind`.
