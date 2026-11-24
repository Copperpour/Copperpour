### Saved-snapshot provenance disclosure on Stage 4b analysis and rule-inspection tools (minor release)

`audit_manufacturing`, `validate_for_manufacturing`, `estimate_cost`, `check_3d_models`, `run_design_review`, `get_design_rules`, `get_predefined_sizes`, and `list_custom_rules` now report observed `sources` and `source_evidence` fields disclosing their saved-snapshot boundary ([#58](https://github.com/Copperpour/Copperpour/issues/58)):

- **No selector or refusal change:** These tools continue to read saved files on disk (and, where applicable, run `kicad-cli` against the saved `.kicad_pcb`) without consulting KiCad IPC. Existing response fields and error shapes are unchanged.
- **Additive `source_evidence`:** Every successful response adds `source_evidence` with `board_state` (`"saved_board"`, `"project_file"`, `"unavailable"` when `run_design_review` was given an unreadable board and DRC could not run, or `null` when `run_design_review` is called without `board`), `excludes_unsaved_editor_state: true`, `reason` (`"saved_snapshot_only"`, `"saved_snapshot_only_with_editor_lock"`, `"saved_snapshot_only_with_uninspectable_lock"`, `"saved_snapshot_only_after_live_observation"`, or `"no_board_supplied"`), and `detail`.
- **Additive per-domain `sources`:**
  - `audit_manufacturing`: `{ "board": "saved_board" }`
  - `check_3d_models`: `{ "footprints": "saved_board" }`
  - `estimate_cost`: `{ "board": "saved_board", "copper_layers": "saved_board" | "argument" | "unavailable", "cost_estimate": "derived" }`
  - `get_design_rules`: `{ "rules": "project_file" }`
  - `get_predefined_sizes`: `{ "track_widths": "project_file", "via_dimensions": "project_file" }`
  - `list_custom_rules`: `{ "rules": "custom_rules_file" | "unavailable", "user_layers": "saved_board" }`
  - `validate_for_manufacturing`: `{ "board": "saved_board", "drc": "kicad_cli" | "unavailable" }` (included in both the tool response and any `output` report file)
  - `run_design_review`: `{ "schematic": "saved_schematic" }` when `board` is omitted, or `{ "schematic": "saved_schematic", "board": "saved_board" | "unavailable", "drc": "kicad_cli" | "unavailable" }` when `board` is supplied
