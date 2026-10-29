### Schema errors name the alternative the caller chose (patch release)

When an argument matches none of a `oneOf` or `anyOf`'s alternatives, the
`invalid_argument` refusal now reports the alternative the value names. An
alternative that rejects the value's JSON type, or whose `const` tag (such as
`type`) the value fails, is passed over.

- A `graphics` item of `set_footprint_graphics` or `create_symbol` of type
  `rect` without `fill` was refused as `graphics[0].type: "line" was
  expected`; it is now `graphics[0].fill: "fill" is a required property`.
- An unknown type such as `rectangle` is now `graphics[0].type: "rectangle" is
  not one of "line", "arc", "rect", "circle", "poly"`, where it used to name
  the first type or an unrelated field.
- A `drill` object of `create_footprint` or `edit_footprint_pad` was told it
  is not a number; it now hears about its own fields, such as
  `drill.shape: "round" is not one of "oval"`.
- `load_toolset` with `name: [5]` now reports `name[0]` instead of `name`.

The same calls are refused. `error.field`, `error.reason` and the `message`
built from them change
([#134](https://github.com/Copperpour/Copperpour/issues/134)).
