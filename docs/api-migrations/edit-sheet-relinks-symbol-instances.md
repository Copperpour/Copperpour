### `edit_sheet` relinks symbol instances and resolves owning `project_name` (patch release)

`edit_sheet` with `new_file` previously changed only the parent sheet's
`Sheetfile` property, leaving symbol instance paths in the old and new child
files stale so subsequent writes failed with `stale_target`. It now validates
that `new_file` is an existing relative `.kicad_sch` path whose subtree is
readable and acyclic, updates symbol instance paths across both the old and new
subtrees in one journaled transaction (leaving an unplaced old file
untouched), and verifies readback on every relinked file. Its answer gains
`relinked_symbol_instances`, the number of symbol instance records it changed
in those files (0 when `Sheetfile` is unchanged).

`edit_sheet`, `get_sheet_hierarchy`, and `renumber_sheet_pages` also now
default `project_name` to the owning project instead of the schematic file's
stem, so nested parents report and renumber page entries under the owning
project and its hierarchy path. `edit_sheet` and `renumber_sheet_pages` refuse a
`project_name` that disagrees with the owning project, and
`renumber_sheet_pages` refuses a parent schematic placed more than once.
