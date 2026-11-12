### New `set_footprint_field_style` sets field text size and thickness on many footprints (minor release)

`update_footprints_from_library` with `reset_text_sizes_and_styles` gives
Reference and Value the library's text style. A style of the caller's own
choosing, such as a fabricator's minimum thickness above the library's or one
text size across the board, took one `edit_footprint_field` call per
footprint, with the board open in KiCad.

`set_footprint_field_style`, in the `pcb_text` toolset, sets `size_x`,
`size_y` and/or `thickness` on the Reference or Value field of the footprints
named in `references`, or of every footprint when it is omitted. With KiCad
holding the board it is one undo step, read back afterwards; with the board
closed it edits the saved file, only while the file still holds what it read.
Both paths answer with the same fields: `source` says which ran (`ipc` or
`file`), `undo` is set on the live path only, and `fallback_reason` and
`warning` (why the file was edited) are null on the live path. The response
lists every field's values before and after, and a whole-board run reports a
field it could not edit in `skipped`.

No existing call changes.
