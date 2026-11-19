### `repeat_layout` copies the reference placement's routing (minor release)

`repeat_layout` placed footprints only; a target placement's tracks and vias
were the caller's to draw.

It now copies the reference placement's tracks, arcs and vias onto each target
placement, turned and moved as its footprints are. It copies the routing on the
placement's own nets, those with a pad on it and none elsewhere; KiCad's Repeat
Layout copies what lies in a rule area instead. Each copy takes the net of the
target's pads matched to that net's pads, and the copies replace the target's
own routing on those nets. A net whose routing is copied and that does not
become one net of the target's own is refused as `plan_blocked`, with a reason
for each, and nothing moves. The response adds `routing`: `copied_count`,
`removed_count` and, for each target, `target_reference`, its counts and the
`nets` it mapped. On a live board the routing joins the placements' one KiCad
commit, the copper check leaves out the routing that commit removes, and a copy
not confirmed on its net after the commit is listed in `routing.unconfirmed`
with a `warning`. On a closed board whose tracks name their net by number, as
boards before KiCad 10 do, the call is refused unless `copy_routing` is false.

`copy_routing: false` keeps the footprint-only behaviour. Issue:
[#608](https://github.com/Copperpour/Copperpour/issues/608).
