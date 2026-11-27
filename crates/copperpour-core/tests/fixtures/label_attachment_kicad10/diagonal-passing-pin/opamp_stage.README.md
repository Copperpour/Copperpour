Adapted from `label-on-pin-exact-passing` through Copperpour: replaced
the orthogonal trunk with a 45° wire and its vertical lead, moved the
labels and J3, and placed J2 on the diagonal without a junction. Resaved
with `kicad-cli sch upgrade --force` in KiCad 10.0.8.

KiCad 10.0.6 and 10.0.8 export J2.1 on `/A` and J3.1 on `/B`.
Adding the proposed horizontal wire from `(20.32, 30.48)` to
`(33.02, 30.48)` creates a junction at J2.1. Both versions then export
both pins on `/A`. A routing guard must see the nets as separate before
the move and refuse that join.
