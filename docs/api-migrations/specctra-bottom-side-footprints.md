### The Specctra round trip takes bottom-side footprints (minor release)

`export_specctra_dsn` no longer refuses a footprint on `B.Cu`
([#430](https://github.com/Copperpour/Copperpour/issues/430), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). A bottom
footprint is written as KiCad's own exporter writes it: its front image, the
SMD pads on `F.Cu` and each pin's own turn mirrored, placed `back` at KiCad's
rotation plus 180 (normalized to 0..360). The router mirrors that image back
onto `B.Cu`. A bottom footprint's SMD pad must be on `B.Cu`, as a front
footprint's must be on `F.Cu`; any other layer is still refused. With the
KiCad 10 native bridge, KiCad's DSN must give every bottom footprint the same
side, rotation, pins and padstacks, or it is refused.

The reverse manifest keeps its schema. Each component's `side` is now the
side its footprint is on (`front` or `back`, it was always `front`), and its
`rotation_degrees` is the DSN `place` rotation. `supported_profile.component_side`
is derived from the footprints: `front`, `back` or `both`.
`apply_specctra_ses` and `plan_specctra_ses_import` refuse a manifest whose
component side differs from its footprint on the live board, or whose profile
side does not match its components, and still refuse a session that changes a
footprint's side, in every mode.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_both_sides_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes`,
and `capabilities.component_side` is new: `front`, `back` or `both`, from the
exported footprints.
