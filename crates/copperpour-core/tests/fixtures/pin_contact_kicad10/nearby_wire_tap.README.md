# Nearby wire tap

`nearby_wire_tap.kicad_sch` was resaved by KiCad 10.0.8 using
`kicad-cli sch upgrade --force nearby_wire_tap.kicad_sch`. The input kept R30
and R33 from `legacy_route_elbows.kicad_sch` at their saved quarter turns.
A wire labelled `FOREIGN` runs from (20.32, 25.405) to a junction at
(25.4, 25.405).

The `kicad-cli` resave ran without a neighboring `.kicad_pro`, so the instance
blocks contain `(project "")`.

The proposed route from R30.1 to R33.1 starts at (25.4, 25.4). The junction
would connect it to `FOREIGN` 0.005 mm farther along the route. That is an
unintended join, not a tap at the requested start.
