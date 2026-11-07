### `renumber_sheet_pages` numbers each placement of a reused sheet file (minor release)

`renumber_sheet_pages` failed with `duplicate transaction target` on a
hierarchy that places a sheet file holding sheets more than once and changes
the pages of those sheets, since it prepared a write of that file at each
placement ([#351](https://github.com/Copperpour/Copperpour/issues/351)). It
now writes each file once, with a page record for every
placement of each sheet in it, as eeschema keeps them; the walk order and the
numbering are unchanged. Each entry of `pages` carries `path`, the sheet
instance's own hierarchy path (its record's path plus its uuid), since one
`sheet_name` and `file` now appear once per placement.
