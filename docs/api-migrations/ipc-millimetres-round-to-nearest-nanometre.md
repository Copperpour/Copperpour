### Coordinates and sizes sent to KiCad round to the nearest nanometre (patch release)

A tool writing to a board KiCad holds open converted millimetres to KiCad's
nanometres by truncating toward zero, so a value binary cannot hold exactly,
such as `4.1` or `128.2 - 5.9`, reached KiCad 1 nm toward zero, and KiCad kept
it ([#84](https://github.com/Copperpour/Copperpour/issues/84)). Read-back
showed `122.299999`, the position file and Gerbers carried the same
nanometre, and an `R_0805_2012Metric` pad of 1.025 mm arrived as 1.024999,
which DRC reports as `lib_footprint_mismatch`
([#238](https://github.com/Copperpour/Copperpour/issues/238)). The conversion
now rounds to the nearest nanometre, a half-way value away from zero. Anchors,
pads, tracks, vias, zones, graphics and text from `place_component`,
`move_component`, `set_component_placements` and every other live board write
move by at most 1 nm and now land on the value asked for; values that were
exact are unchanged. File writes, schematic grid snapping and JSON responses
are not affected. A caller that compensated for the lost nanometre should
stop.
