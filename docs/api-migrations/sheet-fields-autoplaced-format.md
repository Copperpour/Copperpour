### Hierarchical sheet writers use KiCad's autoplacement spelling (patch release)

Tools that rewrite hierarchical sheet items, including `edit_sheet`,
`move_sheet`, `renumber_sheet_pages`, `move_region` and sheet pin edits, now
write an enabled autoplacement flag as `(fields_autoplaced yes)` after the
sheet attributes and before the stroke and fill. Copperpour still reads the
legacy bare `(fields_autoplaced)` form. Callers that inspect raw schematic
text should accept both forms.
