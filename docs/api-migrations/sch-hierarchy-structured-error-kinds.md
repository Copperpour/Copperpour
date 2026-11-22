### Schematic hierarchy tool refusals carry structured `error.kind` envelopes (minor release)

Refusals from `add_hierarchical_sheet`, `edit_sheet`, `move_sheet`,
`delete_sheet`, `duplicate_sheet`, `get_sheet_hierarchy`,
`renumber_sheet_pages`, `import_sheet_pins`, `add_sheet_pin`,
`edit_sheet_pin`, `delete_sheet_pin`, and `validate_sheet_pins` now return
structured JSON error bodies (`{"message": ..., "error": {"kind": ...}}`)
instead of plain-text `handler_error` messages (#741).

The existing human-readable refusal text is preserved verbatim in `message`
(and in `error.reason` for `invalid_argument` refusals):

- `invalid_argument`: missing or duplicate sheet/pin names, invalid `pin_type`
  or relative `sheet_file`/`new_file` paths, parent-referencing or cyclic
  sheet placements, and empty field-change requests (`field: "fields"`).
- `file_not_found`: missing root, source, or child `.kicad_sch` files on disk.
- `conflict`: existing target files in `duplicate_sheet`, unreadable or
  over-depth descendant files during hierarchy relinking, and exhausted page
  numbers.
- `malformed_file`: a sheet missing its required `Sheetname` or `Sheetfile`
  property during `edit_sheet`.
- `plan_blocked`: `import_sheet_pins` when an edge cannot fit all pins without
  placing one past a sheet corner.

Callers inspecting tool error text should read the `message` field of the JSON
error body or branch on `error.kind`.
