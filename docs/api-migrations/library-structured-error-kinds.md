### Library tool refusals carry structured `error.kind` envelopes (minor release)

Refusals from `list_library_footprints`, `get_footprint_info`,
`get_symbol_info`, `create_symbol`, `list_footprint_libraries`,
`list_symbol_libraries`, `register_footprint_library`, and
`register_symbol_library` now return structured JSON error bodies
(`{"message": ..., "error": {"kind": ...}}`) instead of plain-text
`handler_error` messages (#741).

The existing human-readable refusal text is preserved verbatim in `message`:

- `invalid_argument`: non-directory existing paths passed to
  `list_library_footprints`, malformed `lib_id` syntax or missing symbols in a
  resolved library (`get_symbol_info`), invalid `graphics` / `units[].graphics`
  / `pins` / `units[].pins` / `power_pins` arguments in `create_symbol`, and
  missing or unusable `project` paths in `register_footprint_library` and
  `register_symbol_library`.
- `file_not_found`: absent `library_path` directories
  (`list_library_footprints`), unresolved footprint paths or
  `Library:Footprint` identifiers (`get_footprint_info`), unresolved
  `Library:Symbol` libraries (`get_symbol_info`), and unreadable global or
  project `fp-lib-table` / `sym-lib-table` files (`list_footprint_libraries`,
  `list_symbol_libraries`). Unresolved `Library:Footprint` and
  `Library:Symbol` identifiers leave `absolute_path` empty (`""`).
- `malformed_file`: pads with missing or non-numeric `(at ...)` positions or
  unreadable definitions when `get_footprint_info` is called with
  `include_pads: true`.
- `conflict` and `stale_target`: `FootprintGraphicsError::Conflict` and
  `FootprintGraphicsError::StaleTarget` errors surfaced by `get_footprint_info`
  when `include_graphics: true` or `graphics_layer` is supplied.

Callers inspecting tool error text should read the `message` field of the JSON
error body or branch on `error.kind`.
