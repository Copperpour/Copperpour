# Zone outline element fixture provenance

`zone_outline_elements.kicad_pcb` is a purpose-built KiCad 10 board for the
lossless zone-outline scanner. It contains two arc-only outlines and one mixed
`xy`/`arc` outline while avoiding a 3.6 MB copy of the full source demo.

## Arc-only source

- Installed corpus: KiCad 10.0.5 for Windows
- Demo path: `royalblue54L_feather/RoyalBlue54L-Feather.kicad_pcb`
- Source file format: `20241229`
- Source generator/version: `pcbnew` / `9.0`
- Source SHA-256:
  `5F54A63B4640F7D88A1019A0974FF16A775D282E0E141E3EA19D79895452F6F1`
- `VSYS`, `B.Cu`: zone UUID `5735a8ec-4c09-4701-8363-571f8dfc4353`
- `+BATT`, `In2.Cu`: zone UUID `3fbf99d0-27e9-4110-a352-2139dde74d30`

The two zones retain their exact ordered polygon elements and coordinates from
that source file. No filled polygons or unrelated demo-board items are kept.

## Mixed outline and KiCad save path

The mixed zone is UUID `9a05f3f6-3b48-4653-80ad-b87c8f0311e2`, named
`mixed_xy_arc`. It has three straight vertices and one exact arc copied from
the `VSYS` source outline. It was constructed with KiCad 10.0.5's `pcbnew`
geometry API (`SHAPE_LINE_CHAIN`, `SHAPE_ARC`, `SHAPE_POLY_SET`, and `ZONE`).

The reduced board was loaded and force-resaved by
`kicad-cli pcb upgrade --force`, then the mixed zone was added and the complete
board saved by KiCad 10.0.5's `pcbnew.SaveBoard`. The committed serialization
is therefore KiCad-authored output rather than a hand-written test wrapper.
