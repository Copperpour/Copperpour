### `get_schematic_layout` can report note bounds (minor release)

Pass `include_notes: true` to get `notes` and `note_count` for the saved sheet. Each entry
identifies a top-level note by UUID and kind. Supported stroke-font notes at 0° and 90°
return estimated ink bounds in millimetres. Blank notes return `no_ink`. Text boxes,
other rotations, and unsupported styles or content (including text variables and
expressions) return `unmeasured` with a reason.
Both `no_ink` and `unmeasured` have `bounds: null`.

Omit `include_notes` or pass `false` to keep the previous response. Other values,
including `null`, are rejected. `bounding_box` still covers symbols and their pins only.
The estimate uses KiCad's default stroke font and plot settings. It does not account
for project-specific font or pen settings. Small rounding differences can accumulate
on long lines. The bounds are not a page-fit verdict.
