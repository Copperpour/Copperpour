### `score_placement` recognizes edge-mounted parts and measures connector distance from courtyard edges (minor release)

`score_placement` previously reported an `outside_outline` hard failure for any
footprint whose courtyard bbox crossed a rectangular `Edge.Cuts` outline, so
boards with castellated half-hole connectors or flush overhanging receptacles
could never reach `verdict: "pass"`. Separately, `connector_edges[].edge_distance_mm`
measured from the courtyard bbox centre instead of the nearest courtyard edge.

- Footprints with at least one pad or drill hole on or crossing a rectangular
  outline edge (within `0.01 mm`) are now automatically recognized as
  edge-mounted and exempted from `outside_outline`.
- New optional argument `edge_mounted` (array of footprint reference strings)
  covers footprints whose courtyard overhangs the outline with all pads inside
  (such as a flush USB-C receptacle). Unknown references are refused with
  `invalid_argument`; listed footprints whose courtyard does not touch or cross
  the outline boundary (within `0.01 mm`) are not echoed as edge-mounted and
  emit a warning — wholly off-board parts still fail `outside_outline`, and
  wholly interior parts remain unexempted. Same-side `courtyard_overlap` checks
  still apply to every edge-mounted part.
- New response field `edge_mounted` lists each recognized edge-mounted part
  with `reference`, `reason` (`"pad_on_outline"`, `"explicit"`, or
  `"pad_on_outline_and_explicit"`), `edge_pads`, `overhang_mm`, and
  `exempted_outside_outline`.
- `connector_edges[].edge_distance_mm` now measures the shortest distance from
  the connector's courtyard bbox edge to the outline boundary (`0.0` when
  touching or crossing the outline; Euclidean distance when wholly outside),
  and each entry now also carries `center_edge_distance_mm` and
  `edge_mounted`. `connector_edge` deductions are evaluated against this
  courtyard-edge distance.

Issue: [#144](https://github.com/Copperpour/Copperpour/issues/144).
