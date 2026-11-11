### `export_specctra_dsn` accepts two-edge (D-shaped) outlines (minor release)

The Rust exporter used to refuse an `Edge.Cuts` loop of fewer than three lines
or arcs, so a D-shaped board (one line and one arc) failed with "need at least
three Edge.Cuts lines or arcs" although KiCad exports it. Two edges now form a
valid loop when it encloses an area. Two straight lines between the same two
points, or one arc drawn twice, are refused with "two Edge.Cuts edges enclose
no area". A board without any `Edge.Cuts` line or arc is refused with "no
Edge.Cuts lines or arcs". A lone full `gr_circle` outline is still refused.
