### `transform_pcb_items` moves or turns board items and groups together (minor release)

Load `pcb_copper` for the new live-only `transform_pcb_items` tool. Give
`references` (footprints) and/or `uuids`, each entry nonempty and given once,
and either `offset: {x, y}` in millimetres or `rotation` in degrees
(counterclockwise as KiCad shows it) with `origin: {x, y}`.

It moves footprints, vias, straight and arc tracks, board texts, text boxes,
graphics, zones and rule areas as one rigid block, as KiCad's own move or turn
would. A group's UUID stands for every member, through the groups inside it;
groups need KiCad 10.0.7 or newer, which sends a group's lock. A footprint's
own pad or field given with its footprint moves with it. Nothing is stretched
or rerouted.

Refused before anything is written, every reason in one `plan_blocked`:
dimensions, barcodes, reference images, tables and other types, by name; a
footprint's child given without its footprint; a group moved only in part, a
locked group, and a group holding nothing or an item that is not moved; a
teardrop zone, which KiCad rebuilds; a tuning pattern and its tracks; a
graphic's hatch fill and a zone's hatch smoothing, which KiCad's API would
drop; a text box or rounded rectangle, or a footprint holding one, turned by
an angle that is not a multiple of 90°; missing, ambiguous, duplicate and
locked targets; a destination outside KiCad's coordinate range.

`dry_run` defaults to `true` and returns `footprints`, `vias`, `tracks`,
`arcs`, `texts`, `text_boxes`, `graphics`, `zones` and `groups`, each with its
count; `boundary_connections`, the unselected copper and pads the move leaves
behind on the same net (`boundary_connection_count` counts all, at most 100
rows); and `copper_check`, which refuses a pad landing on another net's via,
track or arc, whichever moved. Apply with `dry_run: false` and the dry run's
`plan_revision` as `expected_plan_revision`.

Apply uses one KiCad commit and undo step. Every target and touched group is
read back whole; the nets of tracks, vias and arcs are compared across the
board, and a net KiCad changed is put back by a second commit, reported as
`plan_blocked`; the board text is compared with the plan's. Anything else
unconfirmed is `mutation_outcome_uncertain` with `uuids` to inspect. Responses
carry the shared `outcome` metadata.

No board is saved, no zone refilled and no DRC run; a zone's fill is carried
as it was and is stale until refilled. Footprint children turned by an odd
multiple of 45° can land 1 nm from where earlier releases of the placement
tools put them, now where KiCad puts them. On KiCad before 10.0.8 a live
footprint update can drop the footprint's
[unit metadata](../TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit).
