### `set_custom_rule` writes `bridged_mask` and `via_dangling` (minor release)

A constraint's `type` may now be `bridged_mask` or `via_dangling`
([#474](https://github.com/Copperpour/Copperpour/issues/474)). Neither takes a
min, opt or max: KiCad's DRC reads only the rule's severity, so with
`severity: "ignore"` a rule allows solder mask bridging or a dangling via on the
items its condition selects. The tool writes them bare, as
`(constraint bridged_mask)`, and refuses one given `min`, `opt` or `max` as
`invalid_argument` on that value's field, such as `constraints[0].min`,
writing nothing. Both types were refused before by the schema. `disallow`,
`zone_connection`, `min_resolved_spokes`, `assertion` and
`solder_paste_rel_margin` are still not written.
