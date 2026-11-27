### Unexpected tool arguments include schema hints (patch release)

A schema refusal for an unexpected property can append a suggested field name
or list the fields declared at that point in the input schema. This applies to
direct calls and calls through `call_tool`. The structured error retains
`error.kind: invalid_argument` and identifies the rejected property in
`error.field`. Use those fields for programmatic handling and display
`error.reason` or `message` as diagnostic text.

For `create_footprint`, provide the destination in `output` and the footprint
name in `name`. For `delete_schematic_net_label`, provide the label name in
`net` and its position in `x` and `y`. For `add_component_annotation`, provide
one custom property as `key` and `value`.
