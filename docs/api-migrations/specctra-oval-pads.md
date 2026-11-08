### `export_specctra_dsn` exports oval pads (minor release)

A pad with shape `oval`, as most through-hole connectors have, was refused with
`pad <reference>-<number> has shape 'oval'`
([#384](https://github.com/Copperpour/Copperpour/issues/384)). It is now
exported as KiCad's own Specctra exporter writes it: a `path` whose width is the
pad's shorter side, along its longer axis, on each copper layer of the pad. An
equal-sided oval is a path of zero length. An oval drill (a slot) is still
refused.

What changes for a caller:

- The response's `capabilities.supported_profile` is now
  `two_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`;
  it was `two_layer_front_side_circle_rect_roundrect_no_existing_routing_or_zones`.
- In the reverse manifest, `supported_profile.pad_shapes` lists `oval`, and an
  oval padstack has `"shape": "oval"`.
- With `native_bridge_mode` `prefer` or `require`, KiCad's oval padstacks are
  matched to Copperpour's. A stock footprint's native DSN still fails to parse
  on its courtyard polygon, so it isn't adopted
  ([#98](https://github.com/Copperpour/Copperpour/issues/98)).
