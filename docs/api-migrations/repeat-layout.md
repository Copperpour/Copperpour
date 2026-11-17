### New `repeat_layout` places every other placement of a sheet as one is placed (minor release)

A sheet placed more than once took a `set_component_placements` batch whose
poses the caller worked out: each footprint's counterpart's pose, turned and
moved by the change between two anchor footprints.

`repeat_layout`, in the `placement` toolset, takes an `anchor_reference`, a
footprint on the placement already laid out. Every other footprint of the
anchor's symbol anchors a placement to lay out, or only those named in
`target_references`. Footprints are matched by symbol instance path over the
anchor's sheet and the sheets below it, and each takes its counterpart's pose,
turned by the two anchors' difference in rotation and moved onto its own
anchor, with the arithmetic of KiCad's Repeat Layout. A placement whose footprints
differ from the anchor's in number, library footprint, pad count or side is
refused as `plan_blocked`, with a reason for each footprint, and nothing
moves. Its arguments `dry_run` and `allow_copper_conflicts` and its response
are `set_component_placements`'. Routing, zones and other board items are not
copied. Issue: [#608](https://github.com/Copperpour/Copperpour/issues/608).

No existing call changes.
