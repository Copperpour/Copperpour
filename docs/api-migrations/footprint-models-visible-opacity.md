### `set_footprint_models` keeps a model's visibility and opacity, and writes KiCad's layout (minor release)

[#715](https://github.com/Copperpour/Copperpour/issues/715). `set_footprint_models`
had no field for a model's `(hide yes)` or `(opacity …)`, and `replace` rewrote
every model of the footprint, so it dropped both without saying so.

- **Arguments:** each model takes `visible` (default true) and `opacity` (0 to 1,
  default 1), named as `set_placed_footprint_models` names them. They are written
  as KiCad writes them: `(hide yes)` only when hidden, `(opacity …)` with four
  decimals only when not 1, both before `(offset …)`.
- **`replace`:** a new model with the path of a replaced one that omits
  `visible` or `opacity` keeps that model's value (each replaced model lends its
  values once). A replaced model that was hidden or transparent and whose path
  no new model has is reported in the new `warnings`.
- **Layout:** the block is written as KiCad lays it out, each transform and its
  `(xyz …)` on its own line, in the file's own indentation, so a KiCad save no
  longer rewrites it. `kicad-cli fp upgrade --force` of a KiCad 10 file the tool
  changed gives it back unchanged.
- **Answer:** `warnings`, always present, lists what `replace` dropped.
- **Refused** as `invalid_argument` on `models`: an `opacity` outside 0 to 1 or
  not a number, a `visible` that is not a boolean.

`get_footprint_info` now lists `models`: each model's `path`, `offset`, `scale`,
`rotate`, `visible` and `opacity`, read as KiCad reads them (a bare `hide`, and
a legacy `(at (xyz …))` offset in inches, included).
