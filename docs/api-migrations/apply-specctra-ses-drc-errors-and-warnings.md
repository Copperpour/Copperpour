### `apply_specctra_ses` reports DRC `errors`, `warnings`, and `findings` separately (minor release)

`apply_specctra_ses` previously set `drc.clean` to `false` whenever `drc.violations` was non-zero, even if the only finding was a warning (such as `via_dangling`), and omitted the findings list.

`drc` now separates errors from warnings and lists the findings (in the same `rule`, `severity`, `description`, `pos`, `items` shape as `run_drc`), ordered with errors before warnings and bounded to 50 entries:

- `drc.errors`: count of `error`-severity findings across `violations`, `unconnected_items`, and `schematic_parity`.
- `drc.warnings`: count of `warning`-severity findings across those categories.
- `drc.clean`: `true` when `drc.errors == 0`, `unconnected_items == 0`, and `schematic_parity_violations == 0` (warnings alone no longer mark `clean` as `false`).
- `drc.findings`, `drc.shown`, `drc.truncated`: bounded list of findings and truncation metadata.
- `drc.violations` remains the total count of entries in KiCad's `violations` array for backwards compatibility.

Callers gating on `drc.clean` will now see `true` when an import has only warnings; callers that want to inspect or fail on warnings should check `drc.warnings` and `drc.findings`.
