### `edit_sheet` shows and hides a sheet's name and file fields (minor release)

`edit_sheet` takes two optional booleans, `hide_name` and `hide_file`, for the
`Sheetname` and `Sheetfile` text of a sheet, the two Show checkboxes of KiCad's
Sheet Properties dialog
([#343](https://github.com/Copperpour/Copperpour/issues/343)). `true` hides
the field, `false` shows it, and a field left out stays as it is; either one
alone is enough for a call. The flag is written as KiCad 10 writes it, as
`(hide yes)` after the field's `(at …)`.

A restated visibility is an honest no-op, as for the other fields:
`requested_fields` names `hide_name` or `hide_file` and `changed_fields` leaves
it out. A value that is not a boolean is `invalid_argument`. The sheet object
in the replies of `add_hierarchical_sheet`, `edit_sheet`, `duplicate_sheet` and
`get_sheet_hierarchy` gains `hide_name` and `hide_file`, `null` for a sheet
without that field. A hidden sheet is still found by its `sheet_name`. No
existing call changes.
