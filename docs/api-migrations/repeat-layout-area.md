### `repeat_layout` copies the routing in the reference placement's area (minor release)

`repeat_layout` copied the routing on the reference placement's own nets, those
with a pad on it and none elsewhere, and replaced a target's routing on the nets
the copies took.

It now copies the tracks, arcs and vias that lie in the reference placement's
area, as KiCad's Repeat Layout copies what lies in a rule area. The area is the
outline KiCad's Generate Placement Rule Areas draws: the boxes of the
placement's footprints, of every item in the groups they are in, and of the
routing on its own nets, 0.1 mm out, around their convex hull in horizontal and
vertical edges. Routing on a net the placement shares, such as GND or a net that
reaches another sheet, is now copied when it lies in the area. A copy takes the
net of the target's matched pads, or keeps its own when no pad of the placement
is on it. The copies replace the routing that lies in the target's own area or
in the reference's area carried onto the target, whatever its net; an item the
reference copies is never removed. A net local to the reference must still
become one net local to the target, and every copied net one net there, or the
call is refused as `plan_blocked`. A closed board whose area needs the box of an
item the saved file cannot measure, such as a text in a group, is refused as
`plan_blocked` naming it. `routing.targets[].nets` lists every net a copy goes
on.

`copy_routing: false` keeps the footprint-only behaviour. Issue:
[#608](https://github.com/Copperpour/Copperpour/issues/608).
