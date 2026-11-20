### `add_hierarchical_sheet` and `duplicate_sheet` allocate project-wide free page numbers (patch release)

`add_hierarchical_sheet` and `duplicate_sheet` now walk the owning project's
entire sheet hierarchy (across all top-level sheets and every placement of
reused child sheets) and assign the lowest unused positive page number `>= 2`
(page 1 remains reserved for the root), instead of scanning only the immediate
parent schematic. Previously, adding or duplicating a sheet inside a child
schematic could collide with page numbers already used elsewhere in the project.

If the hierarchy contains a reference cycle or an unreadable top-level sheet,
both tools now refuse the write with `conflict`, naming that file, before modifying any file. Callers that relied
on parent-local numbering or expected gaps not to be reused should inspect the
committed `sheet.page` in the tool response or run `renumber_sheet_pages` to
impose depth-first sequential numbering.
