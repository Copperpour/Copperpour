### `score_placement` reads an outline that a footprint draws (patch release)

A board whose only Edge.Cuts graphics belong to a footprint, as an imported
mechanical footprint's do, used to score as having no outline, and a courtyard
on that footprint's outline overlapped every part. When a board draws no
Edge.Cuts of its own, `score_placement` now reads the outline from footprints
as KiCad builds it: a footprint whose graphics close with a pad outside them,
or into more than one outline, cuts holes; the rest chain end to nearest end
within 0.01 mm into contours, classified as `outline_shape` always was. A
footprint with no electrical pads that is the outline is not scored as a part.

New response fields: `outline_source` (`"board"`, `"footprint"`, or `null`
when no outline is read), `outline_owners` (`reference`, `electrical_pads`,
`primitives`, `role`, `scored`), `outline_contours` (`role`, `references`,
`primitives`, `bbox`) and `warnings`.
When the footprints' graphics make no outline (an open end, a curve, cut-outs
only), the outline stays missing and a warning says why. The board is not
changed; moving the graphics to board graphics stays a manual step. Issue:
[#27](https://github.com/Copperpour/Copperpour/issues/27).
