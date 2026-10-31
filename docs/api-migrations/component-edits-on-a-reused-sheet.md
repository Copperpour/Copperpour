### Component edits accept a sheet placed more than once (patch release)

`edit_schematic_component`, `move_schematic_component`,
`rotate_schematic_component`, `mirror_schematic_component`,
`add_component_annotation`, `group_components` and `replace_component` refused
every symbol on a sheet placed more than once with distinct references as
`stale_target`, before writing
([#234](https://github.com/Copperpour/Copperpour/issues/234)). eeschema
writes a symbol's `Reference` property from one instance only, so the other
instances' designators differ from it by design. These tools now accept such a
symbol when one of its instance records names the property and every record
keeps its prefix, the rule placement already applies; with one record that is
still equality. Their readback holds the set of designators a unit's records
carry to what the file held before the edit. On such a sheet a unit's
`instance_references` in the response lists that set, sorted.

`new_reference` on such a sheet is refused as `ambiguous_target`, listing the
designators, because a rename would reach only one instance; set designators
per instance with `annotate_schematic`. A property that no record names, or a
record of another prefix, is still refused as `stale_target`. Arguments do not
change, and a caller that re-annotated or edited the file to get past the
refusal no longer needs to.

#### Known limitations

Without reading the hierarchy, a record for a path that is no longer a sheet
passes when another record names the property; before, it was refused when its
designator differed. A symbol whose unit differs between instances is still
refused as `ambiguous_target`.
