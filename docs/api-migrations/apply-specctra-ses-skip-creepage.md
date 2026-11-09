### `apply_specctra_ses` takes `skip_creepage` (minor release)

`apply_specctra_ses` accepts the optional boolean `skip_creepage` that
`run_drc` already takes (default `false`). The candidate DRC now reads the
source project's custom rules (#312), creepage constraints included, and
KiCad checks creepage single-threaded over every net pair: on a large board
that one DRC can take minutes to hours. When `true`, the candidate is checked
against the project's `.kicad_dru` without its `creepage` constraints; every
other rule, the netclasses and the schematic parity check still apply, and the
project directory is never written.

`drc` adds `creepage_skipped`, `true` only when constraints were actually left
out: `false` when `skip_creepage` is off, when the project has no creepage
constraint, and without symbolic links (some Windows setups), where the
candidate is checked on its own. A non-boolean `skip_creepage` is refused as
`invalid_argument` before KiCad is asked. Calls that leave `skip_creepage` out
are unchanged; run the full DRC before manufacturing.
