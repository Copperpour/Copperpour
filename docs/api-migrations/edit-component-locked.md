### `edit_component` sets and reports a footprint's lock (minor release)

`edit_component` takes `locked`, optional: `true` locks a placed footprint and
`false` unlocks it, as KiCad's own lock. Before, no tool set or cleared the
lock, though `auto_place_from_schematic` and `refine_placement_force_directed`
hold a footprint KiCad marks locked. As with the other properties, the edit is
one KiCad undo step and the answer is a fresh read.

The answer adds `locked`, at the top level and in `before`, and `changed` can
name it. A call naming no property reports the lock too. Issue:
[#39](https://github.com/Copperpour/Copperpour/issues/39).
