### a move or a wire delete judges a junction dot by its exits (patch release)

The tools that re-judge junction dots where pins moved or went away
(`move_schematic_component`, `rotate_schematic_component`,
`mirror_schematic_component`, `move_region`, `replace_component`,
`delete_schematic_component`, `bulk_move_schematic_components`,
`batch_place_components`, `batch_delete` and
`batch_delete_schematic_components`) and the wire deletes
(`delete_schematic_wire`, `batch_delete_schematic_wire`) used to keep every dot
with two or more wires under it, and the first added one only where a pin
landed mid-span on exactly one wire. A pin moved onto a wire corner got no dot,
and a T whose branch was deleted kept its dot at the corner left.

Now each dot at a point they touch is judged by the count of exits the wire
tools write dots by (see *wire tools dot a T built from wire ends*)
([#136](https://github.com/Copperpour/Copperpour/issues/136)). A dot is kept at
three or more exits and goes below, and a pin that makes three gets one. So a
pin landing on a corner gets a dot, and a dot at a plain corner, at two wire
ends meeting in line, or at one wire ending on a pin goes. Where a wire passes
mid-span and another wire is at the point, as at a crossing, a collinear
overlap or a T whose wire was not split, only a dot (or a label over them)
joins them, so none is added there and none removed. A pin under a no-connect
gets no dot, and bus points are left alone. When a sheet's pins cannot be
resolved, a dot with a wire under it is kept. Schemas and response shapes are
unchanged. A caller that added a dot after such a move, or removed one after
such a delete, can drop that step.
