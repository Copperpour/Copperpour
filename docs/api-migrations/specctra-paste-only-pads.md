### The Specctra round trip accepts pads on no copper layer (minor release)

`export_specctra_dsn` no longer refuses a footprint whose pads include one
on no copper layer, such as the unnumbered paste apertures over the exposed
pad or tab of KiCad's stock QFN, DFN, SO and TO footprints
([#458](https://github.com/Copperpour/Copperpour/issues/458)). An SMD pad
whose layers are all technical (`F.Paste`, `B.Mask`, ...) is left out of the
DSN image, as KiCad's own exporter leaves it out, so the router never sees
it. An unnumbered pad with copper, and an `np_thru_hole` pad, are still
refused.

The reverse manifest moves to schema 4. Each component lists such pads in
the new `non_copper_pads` array (`{"pad_number", "pad_uuid", "layers"}`;
`pad_number` may be empty); a component without them omits it.
`apply_specctra_ses` and `plan_specctra_ses_import` compare that list with
the live board's and refuse the session when a pad was added, removed,
renumbered or moved to other layers since the export. They still read a
schema 1 to 3 manifest, which never records one.
