### Footprint graphics keep their library line style, and an unchanged refresh plans `noop` (patch release)

`update_footprints_from_library`, `place_component` and the footprints
`update_pcb_from_schematic` adds over IPC used to send every footprint shape
with no line style. KiCad saved each one as `(type default)`, including the
`(type solid)` strokes of every stock footprint. Footprint user text was sent
without keep-upright, so KiCad saved it `(unlocked yes)`. So were the custom
properties the refresh writes, such as `KiLib_Generator`. Each shape now keeps
the library's `(stroke (type …))`. A stroke without a type, or a KiCad 6 bare
`(width …)`, is sent as solid, as KiCad 10.0.6 loads it. User text keeps
upright unless the library marks it `(unlocked yes)` or, in a KiCad 7 or older
file, `(at … unlocked)`. The refresh still refuses unlocked user text and
properties, so its text always keeps upright. `place_component`'s Reference
and Value fields are unchanged (#971).

Footprint text and refreshed properties are now sent at their angle as placed,
which KiCad turns upright itself, where they used to be turned by 180° past
90°. That flip showed `(unlocked yes)` text upside down from the library. A
rectangle turned by a multiple of 90° keeps its corners in rotated order, so
KiCad saves the library's own `start` and `end`.

`update_footprints_from_library` also stopped reporting changes that are only
KiCad's representation: the parent each child names, an SMD pad's zero-size
drill, chamfer and anchor settings a pad's shape does not use, and a pad angle
of 270° against −90°. A footprint that matches its library now plans `noop`,
and so does a dry run right after an apply. Before, every stock footprint
planned `pads`, `graphics` and `metadata` on every run. A footprint an earlier
Konnect refreshed plans `graphics` and `metadata` once more, because its text
was saved `(unlocked yes)` and turned; applying that plan settles it.

KiCad 10.0.6 reports a `(type default)` stroke as solid over IPC, so the
planner cannot see strokes that an earlier refresh turned into `default`, and
does not offer to restore them. Use KiCad's own Update Footprints from Library
for those. Arguments and response shapes are unchanged (#970).
