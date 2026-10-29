### `set_design_rules` refuses the five older minima outside the range KiCad loads (patch release)

`set_design_rules` wrote `min_clearance`, `min_trace_width`, `min_via_drill`,
`min_via_size` and `min_hole_to_hole` without a range check, and KiCad 10
replaces a value outside the range it loads with its default when it opens the
project, so the call reported success and DRC ran with something else
([#152](https://github.com/Copperpour/Copperpour/issues/152)). They now take
the same check as the other five minima: `min_hole_to_hole` 0 to 10 mm, the
other four 0 to 25 mm. A value outside its range is refused as
`invalid_argument`, naming the field, and nothing is written; the schema
advertises each range as `minimum` and `maximum`. A caller that sent such a
value should send one inside the range.
