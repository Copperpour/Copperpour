### Whole-sheet writes pack `(pts …)` points as eeschema does (patch release)

A tool that rewrites a whole `.kicad_sch` used to put each `(xy …)` point of a
wire, bus or polyline on a line of its own. It now keeps a run of points on one
line while the line is shorter than 99 characters, a tab counting one, as
eeschema writes them, so the `(pts …)` blocks it did not edit come back as they
were. A sheet saved with one point per line is repacked on its next write. No
tool argument or response changes. Issue:
[#20](https://github.com/Copperpour/Copperpour/issues/20).
