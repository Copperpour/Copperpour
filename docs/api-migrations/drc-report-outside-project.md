### DRC no longer writes or deletes `<board>.drc.json` (patch release)

`run_drc`, `get_drc_violations`, `design_review`,
`validate_for_manufacturing`, `pcb_board_outline` and the candidate DRC of
`apply_specctra_ses` now have `kicad-cli` write its report to a temporary
directory of their own. An existing `<board>.drc.json` beside the board is left
untouched; before, the DRC overwrote it and then deleted it, or, through a
project view (`skip_creepage`, the SES candidate), overwrote the project's copy
with the view's report.

`apply_specctra_ses` no longer refuses with `conflict` when
`<candidate>.drc.json` exists, and no longer deletes that file on rollback; it
still refuses when the candidate board itself exists. Nothing for callers to
change.
