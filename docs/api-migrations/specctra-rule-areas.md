### The Specctra round trip keeps rule areas as keep-outs (minor release)

`export_specctra_dsn` no longer refuses a board with rule areas
([#81](https://github.com/Copperpour/Copperpour/issues/81), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Each supported board
rule area is written as its outline per copper layer it covers:

- `tracks` and `vias` not allowed -> `keepout`
- `tracks` not allowed, `vias` allowed -> `wire_keepout`
- `vias` not allowed, `tracks` allowed -> `via_keepout`

What Specctra DSN keep-outs cannot express without approximation is still
refused, each with its own message: a rule area that allows both tracks and vias
(pour-, pad- or footprint-only keep-outs, which KiCad's own exporter turns into
full `keepout`s), a rule area with room `placement` enabled, a rule area with a
net, an outline with a cutout or non-straight edge, a rule area off the copper
stack, and a zone inside a footprint. With the KiCad 10 native bridge, KiCad's
keep-outs must match the exported kinds, layers and closed outlines in any
order, or the native DSN is refused.

The reverse manifest moves to schema 5. `supported_profile.rule_areas` is `true`
when the board has rule areas, and the new `rule_areas` list records each one's
`kind` (`keepout`, `wire_keepout`, `via_keepout`), `layers` and `outline_um`
points in canonical order; a board without rule areas omits it.
`apply_specctra_ses` and `plan_specctra_ses_import` refuse a manifest whose
rule-area inventory differs from the live board's, still read a schema 1 to 4
manifest of a board without rule areas, and reject any SES track, arc or via
whose copper overlaps an active rule-area keep-out on a covered layer.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_both_sides_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes_rule_areas_as_keepouts`,
and `capabilities.rule_areas` is new: the exported board's rule-area count.
