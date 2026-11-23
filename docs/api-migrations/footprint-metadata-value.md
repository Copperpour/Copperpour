### `set_footprint_metadata` sets a footprint's Value (minor release)

`set_footprint_metadata` takes `value`, the new text of the footprint's Value
field ([#713](https://github.com/Copperpour/Copperpour/issues/713)). No library
tool could change it before: `rename_footprint` changes it only when it equals
the old name, so a footprint renamed in KiCad or by hand kept its old Value.

- **Write:** only the text changes, as KiCad's `FOOTPRINT::SetValue` changes
  it; the field's position, layer, effects and UUID stay. Both forms are read:
  `(property "Value" …)` and an older file's `(fp_text value …)`.
- **Answer:** `changes.value` gives the previous and current text when it
  changed.
- **Refused** as `invalid_argument` on `value`: a footprint without a Value
  field (the field is never created), and a `value` that is not a string.
