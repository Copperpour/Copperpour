### `update_footprints_from_library` refreshes footprints with pad fabrication properties or a reference point on KiCad 10.0.7+ (minor release)

Typed library refresh refused a library footprint whose pads have a
fabrication property, `(property pad_prop_…)`, or that has a reference
point, `(point …)`, as `unsupported_library_footprint`
([#283](https://github.com/Copperpour/Copperpour/issues/283)). 1,364 of
KiCad 10.0.8's stock footprints have pad properties (heatsink, BGA,
mechanical, fiducial) and 617 have a reference point, among them the DIP
packages.

On KiCad 10.0.7 and later, refresh now reads both:

- Each pad's property is sent as the pad's fabrication property, as KiCad
  loads it from the file. A pad whose property differs from its library
  pad plans `pads`.
- Each point is sent as a child of the footprint, placed, turned and
  mirrored to the back as KiCad places it. A point that differs from the
  library's, or that one side has and the other doesn't, plans `graphics`.

KiCad before 10.0.7 can't take either over its API, so there refresh still
refuses these footprints as `unsupported_library_footprint`, with a message
that names KiCad 10.0.7. Footprints with neither clause refresh as before on
every version.

Many of these footprints have other clauses refresh still refuses, most
often a pad's `zone_connect`
([#300](https://github.com/Copperpour/Copperpour/issues/300)). They are still
refused, and the message names that clause.
