### PCB component tool refusals carry structured `error.kind` envelopes (minor release)

Refusals from `place_component`, `move_component`, `rotate_component`,
`batch_place_components`, `flip_component`, `repair_corrupted_footprints`,
`get_component_pads`, `get_pad_position`, `place_component_array`, `align_components`,
`duplicate_component` (and sibling closed-board callers `place_decoupling_caps`,
`refine_placement_force_directed`, and `repeat_layout`) now return structured
JSON error bodies (`{"message": ..., "error": {"kind": ...}}`) instead of
plain-text `handler_error` messages (#741).

The existing human-readable refusal text is preserved verbatim in `message`
(except IPC-wrapped failures in `place_component_array` and `align_components`,
which now drop the `"IPC array error: "` / `"IPC align error: "` prefix and
route through structured envelopes with recovery guidance where applicable):

- `invalid_argument`: malformed `Library:Footprint` identifiers, missing
  footprints or pads (`get_component_pads`, `get_pad_position`), invalid or
  overflowing array dimensions and reference ranges (`place_component_array`),
  duplicate array target references (`place_component_array`), and invalid
  `references`/`axis` arguments (`align_components`).
- `file_not_found`: unresolved or unreadable library footprint files in
  `place_component`, `place_component_array`, and `duplicate_component`,
  including a source footprint whose saved id names no library
  (`duplicate_component`).
- `malformed_file`: unusable closed-board S-expressions (`ClosedBoardError::Unusable`),
  malformed library footprint files (`place_component`, `place_component_array`,
  `duplicate_component`), failed pre-6.0 `(module …)` upgrades
  (`place_component`), and pads with missing/invalid positions or unreadable
  definitions in saved boards (`get_component_pads`).
- `plan_blocked`: closed-board back-side (`B.Cu`) placement when the footprint
  cannot be mirrored without live KiCad (`place_component`).
- `editor_unavailable`: live-IPC-only refusals when KiCad is unreachable or
  does not hold the requested board (`repair_corrupted_footprints`,
  `place_component_array`, `align_components`).
- `conflict`: live KiCad reporting zero pads for a footprint when the saved
  board file has pads (`get_component_pads`).
- `wrong_document`, `wrong_project`, `ambiguous_open_board`,
  `ipc_outcome_unknown`, `ipc_batch_recovered`, and structured `handler_error`
  with recovery guidance: IPC target, rejection, and uncertain-outcome
  classifications in `place_component_array` and `align_components` via
  `ipc_failure_result`.

Callers inspecting tool error text should read the `message` field of the JSON
error body or branch on `error.kind`.
