### The Specctra round trip takes 2 to 32 copper layers (minor release)

`export_specctra_dsn` no longer refuses a board whose copper is not exactly
`F.Cu` and `B.Cu` ([#430](https://github.com/Copperpour/Copperpour/issues/430),
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Any stack KiCad
gives a board is exported: `F.Cu`, `In1.Cu`..`In<N-2>.Cu` and `B.Cu`, N even,
written top to bottom as DSN signal layers 0 to N-1 whatever the order of the
board's layer table. A copper set with a gap, a repeat or a missing outer
layer is still refused. Through-hole pads must cover every copper layer
(`*.Cu`, or every layer listed); a through-hole pad on `F.Cu` and `B.Cu` only of
a four-layer board is refused. A locked via must still be a through via, which
KiCad writes as `F.Cu` to `B.Cu` on any stack; its DSN padstack, like each
routing via's, has a shape on every copper layer. Component side, zones and
custom rules are unchanged.

The reverse manifest keeps schema 2: `supported_profile.copper_layers` is the
board's copper layer count instead of always `2`, and `layers` maps each of
them. `apply_specctra_ses` accepts such a manifest when its layer mappings are
exactly the stack of that count, indexed top to bottom; a manifest written by
an earlier release, with two layers, is read as before. A session wire on a
layer the manifest does not map is still refused.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`;
it was `two_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`.
`capabilities.copper_layers` is new: the exported board's copper layer count.
Callers that compared the profile string to the old value should read
`copper_layers` instead.
