### `score_placement`, `get_placement_overview`, `check_clearance`, `get_component_pads`, and `get_pad_position` adopt the shared `board_source` contract and provenance (minor release)

`score_placement`, `get_placement_overview`, `check_clearance`, `get_component_pads`, and `get_pad_position` now resolve their board state through the shared `board_source` seam ([#58](https://github.com/Copperpour/Copperpour/issues/58)):

- **New argument:** Each tool accepts optional `board_source` (`"auto"` default, `"live"`, `"saved"`). `"auto"` prefers the exact board open in KiCad and falls back to the saved `.kicad_pcb` file only when no live board was positively identified and the board was not previously observed live during the current server session; `"live"` requires the exact board open in a reachable KiCad; `"saved"` intentionally inspects the last-saved file without consulting KiCad and discloses that unsaved editor state is excluded.
- **Refusal and staleness policy:** Once the exact board has been identified in KiCad, a failed live query returns `editor_unavailable` with recovery guidance instead of an untyped error or a silent saved-file answer. If a board was observed live earlier in the session and KiCad is later lost or closed, `board_source: "auto"` refuses with `unsafe_file_fallback` rather than reporting the saved file as current; pass `board_source: "saved"` to inspect the saved snapshot explicitly. When reading from disk, `check_clearance` now shares `read_consistent` with `score_placement` and `get_placement_overview` and returns structured `file_not_found` if the board file does not exist.
- **Provenance fields:** Every successful response retains its existing `source` field (`"ipc"` or `"saved_file"` on `score_placement`, `get_placement_overview`, and `check_clearance`; `"ipc"` or `"file"` on `get_component_pads` and `get_pad_position`) and adds `sources` and `source_evidence`:
  - `score_placement`: `sources: { "placement": "ipc" | "saved_board" }`
  - `get_placement_overview`: `sources: { "placement": "ipc" | "saved_board", "net_groups": "project_file" | "argument" | "unavailable", "groups": "derived" }`
  - `check_clearance`: `sources: { "clearance": "ipc" | "saved_board" }`
  - `get_component_pads`: `sources: { "pads": "ipc" | "saved_board" }`
  - `get_pad_position`: `sources: { "pad": "ipc" | "saved_board" }`
