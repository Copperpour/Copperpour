### `replace_component` switches sheet-local placements to the requested symbol (patch release)

`replace_component` now removes `lib_name` from each replaced placement. KiCad
uses the requested embedded definition instead of the previous sheet-local
copy. Other placements keep their own aliases. `removed_lib_symbol` can now
name the former sheet-local definition when it had no remaining users.
For sheets already affected by this bug, rerun the replacement on the affected
reference to clear the stale alias.

A selected placement with repeated `lib_id` fields returns `malformed_file`
without changing the sheet. Resave it in KiCad to normalize the fields before
retrying.
