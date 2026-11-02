### Placements refuse to land a pad on another net's copper (minor release)

`set_component_placements`, `move_component` and `rotate_component` now check
a live board before they move anything
([#96](https://github.com/Copperpour/Copperpour/issues/96)). A pad dropped on
another net's via makes KiCad re-net the via to the pad's net, silently, and
DRC cannot see the change afterwards; a pad dropped on a track shorts it. For
each moved footprint the tools ask KiCad for its pads' copper outlines
(`GetPadShapeAsPolygon`) on every copper layer the pad has copper on
(`CheckPadstackPresenceOnLayers`), move them to the planned pose, and test them
against every via, track and arc of another net on that layer. Only overlap
counts: touching is a conflict, a near miss is not. Clearance, zones, and the
pads of other footprints, including two footprints of one batch, are not
checked. Copper with no net is never the same net as anything.

A move that would put a pad on such copper is now refused as `plan_blocked`
before any change: no commit is opened and nothing moves, and
`set_component_placements` refuses the whole batch. `error.reasons` names each
conflict in words, and `copper_check.conflicts` lists each as `reference`,
`pad`, `pad_net`, `layer`, `item_kind` (`via`, `track` or `arc`), `item_uuid`
and `item_net`. A check that cannot complete, for example because KiCad gives
no outline for a pad, refuses the call too, and nothing moves. A caller that
puts a part on such copper on purpose, to remove the copper next, passes
`allow_copper_conflicts: true`.

All three tools take two new optional booleans, both default `false`:

- `allow_copper_conflicts: true` applies the move anyway and still returns the
  conflicts, with a `warning`.
- `dry_run: true` moves nothing and returns the check, `dry_run: true`,
  `source`, and `count` and `placements`, each footprint's `reference` with its
  `current` and `target` pose (`x`, `y`, `rotation`).

Each success response of the three tools, and the `plan_blocked` refusal, now
carries `copper_check`. On a live board it holds `status` (`clear` or
`conflicts`), `conflict_count`, `conflicts`, and `coverage`: `pad_count`, the
moved pads tested; `layers`, the copper layers they were tested on; and
`via_count`, `track_count` and `arc_count`, the copper read from KiCad. The
other response fields are unchanged.

Holes are not checked. Only a pad's copper is tested and an NPTH pad has none,
so a footprint whose only pads are NPTH, such as `MountingHole_3.2mm_M3`,
reports `status: "clear"` with `pad_count: 0` and `layers: []`: a mounting
hole moved onto a track reads as clear. Read `clear` with a `pad_count` of 0
as nothing tested.

The closed-file path cannot run the check, because it needs KiCad's own pad
shapes. With no live KiCad the tools still edit the saved board as before, and
report `copper_check` as `{"status": "not_run", "reason": "no_live_board",
"detail": …}`: read that as no check, never as a pass. A `dry_run` there reads
the poses from the file and writes nothing. Open the board in KiCad to have
placements checked.
