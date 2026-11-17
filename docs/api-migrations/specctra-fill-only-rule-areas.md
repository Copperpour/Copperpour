### `export_specctra_dsn` accepts fill-only rule areas (minor release)

`export_specctra_dsn` no longer refuses a board with a rule area that allows both
tracks and vias, where only `copperpour`, `pads` and/or `footprints` are
`not_allowed` (part of [#81](https://github.com/Copperpour/Copperpour/issues/81)).
Such an area puts no constraint on routing: a router places no copper fill, and
pads and footprints are placement and DRC rules. It is not written to the DSN.
The omission is explicit:

- The schema-5 manifest records it in `rule_areas` with the new `kind`
  `fill_only`, its `layers` and `outline_um`, like the other kinds. The manifest
  schema version does not change. `plan_specctra_ses_import` and
  `apply_specctra_ses` still refuse a manifest whose inventory differs from the
  live board, and accept SES copper across a `fill_only` area.
- The `export_specctra_dsn` response has two new fields: `rule_areas_not_exported`,
  one `{label, layers, reason}` per fill-only area, and `native_keepouts_dropped`.
  `capabilities.rule_areas` still counts every rule area, fill-only ones included.
- With `native_bridge_mode` `prefer` or `require`, KiCad 10.0.8 and older write each
  fill-only area as a full `keepout` per layer (KiCad#25959). Adoption removes
  exactly those keep-outs (same layer and outline) from the native DSN before
  correlation and counts them in `native_keepouts_dropped`. Any other keep-out
  difference is still refused.

Refill and DRC enforce the area after apply: refill keeps copper fills out of it,
and DRC checks the pad and footprint flags. Rule areas with room `placement`
enabled, a net, a cutout or non-straight edge, or off-grid vertices remain
refused.
