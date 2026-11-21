# edge_mounted_kicad10

A 60 × 45 mm two-layer KiCad 10.0.8 board (`(0, 0)..(60, 45)` rectangular
`Edge.Cuts` of four segments) exercising edge-mounted footprints and
courtyard-edge connector distance (issue #144), built from stock KiCad 10
libraries with `pcbnew` Python (`SaveBoard`, then reloaded with `LoadBoard` for
the oracle table below).

## Cases

- `J1` (`Connector_USB:USB_C_Receptacle_GCT_USB4105-xx-A_16P_TopMnt_Horizontal`):
  `F.Cu`, at `(3.675, 22.5)`, rotated `-90°`. The receptacle's `F.Fab` front
  body edge (local `y = +3.675`) sits flush on the left board edge `x = 0`; its
  `F.CrtYd` courtyard extends `0.505 mm` past `x = 0` (`x = -0.505..8.435`).
  All 22 pads lie inside the board (`edge_pads = 0`), so `J1` requires explicit
  `edge_mounted: ["J1"]`. Courtyard edge distance to `x = 0` is `0.000 mm`,
  while courtyard centre distance is `3.965 mm`.
- `J2` (`Connector_PinHeader_2.54mm:PinHeader_1x08_P2.54mm_Vertical`):
  `F.Cu`, at `(38.42, 22.5)`, rotated `90°`. An interior 1×8 header whose right
  courtyard edge sits `2.030 mm` from the right board edge `x = 60` (well
  within the 10 mm connector-edge limit), while its courtyard centre sits
  `12.690 mm` from `x = 60` (exceeding 10 mm under the old centre metric).
- `J3` (`Connector_PinHeader_2.54mm:PinHeader_1x08_P2.54mm_Vertical`):
  `F.Cu`, at `(18.0, 0.0)`, rotated `90°`. Eight plated through-hole pads
  centred right on the top board edge `y = 0` (castellated half-hole pattern).
  Automatically recognized as edge-mounted (`edge_pads = 8`).
- `J4` (`Connector_PinHeader_2.54mm:PinHeader_1x08_P2.54mm_Vertical`):
  `B.Cu` (flipped in `pcbnew`), at `(38.0, 45.0)`, rotated `90°`. Eight plated
  through-hole pads centred right on the bottom board edge `y = 45`. Tests
  back-side transform for pad-on-outline detection (`edge_pads = 8`).
- `H1` (`MountingHole:MountingHole_3.2mm_M3`):
  `F.Cu`, at `(60.0, 10.0)`, `0°`. Non-`J*` footprint with a single
  `np_thru_hole` centred on the right board edge `x = 60` (`edge_pads = 1`),
  courtyard overhanging by `3.450 mm`.
- `R1` (`Resistor_SMD:R_0603_1608Metric`):
  `F.Cu`, at `(25.0, 22.5)`, `0°`. Ordinary interior resistor well inside the
  outline (`edge_pads = 0`, `overhang = 0.000 mm`).

## Oracle (KiCad 10.0.8 `pcbnew.LoadBoard`)

Each footprint's courtyard bbox is computed from its reloaded `F.CrtYd`/`B.CrtYd`
graphical items (`GetBoundingBox()` less half the stroke width), and `edge_pads`
counts reloaded `fp.Pads()` whose `GetBoundingBox()` meets any of the four
`Edge.Cuts` segments within `0.01 mm`.

| ref | layer | anchor (mm) | rot | courtyard bbox (mm) | pads | edge_pads | overhang (mm) | edge dist (mm) | centre dist (mm) |
|---|---|---|---:|---|---:|---:|---:|---:|---:|
| H1 | F.Cu | (60.000, 10.000) | 0° | (56.550, 6.550)..(63.450, 13.450) | 1 | 1 | 3.450 | 0.000 | 0.000 |
| J1 | F.Cu | (3.675, 22.500) | -90° | (-0.505, 17.180)..(8.435, 27.820) | 22 | 0 | 0.505 | 0.000 | 3.965 |
| J2 | F.Cu | (38.420, 22.500) | 90° | (36.650, 20.730)..(57.970, 24.270) | 8 | 0 | 0.000 | 2.030 | 12.690 |
| J3 | F.Cu | (18.000, 0.000) | 90° | (16.230, -1.770)..(37.550, 1.770) | 8 | 8 | 1.770 | 0.000 | 0.000 |
| J4 | B.Cu | (38.000, 45.000) | 90° | (18.450, 43.230)..(39.770, 46.770) | 8 | 8 | 1.770 | 0.000 | 0.000 |
| R1 | F.Cu | (25.000, 22.500) | 0° | (23.520, 21.770)..(26.480, 23.230) | 2 | 0 | 0.000 | 21.770 | 22.500 |

## Generation

Built with KiCad 10.0.8 `python3 -I` using `pcbnew.BOARD()`, four `Edge.Cuts`
segments from `(0, 0)` to `(60, 45)`, `pcbnew.FootprintLoad` from
`/usr/share/kicad/footprints/*.pretty`, `board.Add`, `SetPosition`, `Flip`
(for `J4`), `SetOrientationDegrees`, and `pcbnew.SaveBoard`.

## sha256

```text
49249fcd85dfffa2efb4a664ebed5a57447acca6c775d2983c88de1aef256969  edge_mounted_kicad10.kicad_pcb
```
