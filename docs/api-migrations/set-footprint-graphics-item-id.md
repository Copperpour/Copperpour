### `set_footprint_graphics` accepts `selector.item_id` (minor release)

`set_footprint_graphics` previously accepted only `selector.layer`, so
`replace` and `delete` affected every supported graphic on that layer.

`selector` now accepts either `layer` or `item_id` (from
`get_footprint_info` with `include_graphics: true`). With `selector.item_id`:

- `replace` replaces that single graphic on its existing layer, preserving its
  `uuid` (or legacy `tstamp`) on a 1-for-1 replacement.
- `delete` removes that single graphic.
- `append` is refused with `invalid_argument`.
- Missing `item_id` returns `stale_target`; duplicate `item_id` or a graphic
  referenced by a `(group ...)` returns `conflict`.
- Responses include `"item_id"` alongside `"layer"` when selected by `item_id`.

Existing `selector.layer` calls are unchanged.
