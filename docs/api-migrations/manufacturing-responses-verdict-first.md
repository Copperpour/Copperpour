### `validate_for_manufacturing` and `export_manufacturing_package` put the verdict first and bound their lists (minor release)

Both responses had their keys sorted, which put `validate_for_manufacturing`'s
`verdict` after every issue and the package's `summary` and `warnings` after
its placement evidence. Both grew with the board: one `issues` entry per DRC
error, and the JLCPCB placement evidence with an entry per placed part, carried
twice ([#69](https://github.com/Copperpour/Copperpour/issues/69)).

`validate_for_manufacturing`'s keys now come in this order: `verdict`,
`summary`, `fab_house`, `board_info`, `drc`, `issues_count`, `issues_shown`,
`issues_truncated`, then `issues`. `issues` holds the first `limit`, 50 unless
the call names another; `issues_count`, the verdict and `summary` cover them
all. The issues that are not a single DRC error, such as a DRC category
kicad-cli did not report, now come before the DRC errors, so the bound cannot
hide them. `output` names a file to receive the whole response, every issue
listed, as `run_drc` writes its report. A path that is the board, a directory,
or under something that is not a directory is refused before the board is
read; a write that fails after that check fails the call.

`export_manufacturing_package`'s keys now come in this order: `complete`,
`summary`, `warnings`, `next_steps`, `fab_house`, `output_dir`, `files`,
`files_generated`, `gerber_layers`, `position_units`, `position_side`,
`board_source`, `project_settings_source`, then `placement_orientation`. For a
JLCPCB assembly, the whole placement evidence, the object `placement_orientation`
held, is written to `CPL-<project>.orientation.json` beside the CPL. It is not
in `files`, which lists what to upload; a failed write is a warning, and the
package is incomplete. The file stays in `output_dir` with the other outputs,
and nothing removes it: a later export into the same directory replaces it
only when it writes evidence again, so with another fab house, without
assembly, or after a failed write, an earlier file stays there unnamed. Export
into a fresh directory, as for the CPL and BOM.

`placement_orientation` now holds `status`, `physical_validation`, `note`,
`evidence_path` (that file's path, null when it was not written),
`geometry_count`, `unmatched_footprints_count`, `unverified_footprints_count`,
`applied_corrections_count`, `applied_corrections_shown`,
`applied_corrections_truncated`, `policies`, and the first `limit`
`applied_corrections`, 50 unless the call names another. Read `geometry`,
`unmatched_footprints` and `unverified_footprints` from the file; `warnings`
still names every part they call to review. The pick-and-place entry of
`files_generated` no longer carries its own copy of `placement_orientation`.

`generate_jlcpcb_order_link` repeats the package's `placement_orientation`
without `evidence_path`, since its package is written to a temporary directory
removed when the call returns. Run `export_manufacturing_package` for the file.
A failed evidence write leaves its package incomplete, which it refuses to
upload, as it refuses any incomplete package. It takes no `limit`, and repeats
the first 50 applied corrections.

`validate_for_manufacturing` and `export_manufacturing_package` take `limit` as
`run_drc` does: a non-negative integer, `0` for counts only.
