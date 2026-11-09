### `apply_specctra_ses` checks the candidate under the project's rules (minor release)

The candidate DRC of `apply_specctra_ses` now applies the source board's
project: the netclasses of its `.kicad_pro`, the custom rules of its
`.kicad_dru`, and its schematic for the parity check. Before, kicad-cli read
the project from files named like the candidate, which a new candidate never
has, so the candidate was checked against KiCad's defaults and `drc.clean` could
be `true` on a board whose project DRC fails (#312).

kicad-cli runs on a temporary view of the project directory (symbolic links)
in which the board under the source board's name is the candidate; nothing in
the project is written. `drc` adds `project_rules_applied`: `true` when the
project's rules applied, `false` only without symbolic links (some Windows
setups), where the candidate is checked on its own as before.

Callers may see violations, unconnected items or schematic parity differences
that the previous candidate DRC did not report. They are the ones `run_drc` on
the source project reports for the same board.
