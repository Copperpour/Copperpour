# placement_overview_kicad10

A board for `get_placement_overview`, saved by KiCad 10.0.7's pcbnew, with its
project file. `generate.py` builds both and prints the expected values below;
rerun it with `python3 generate.py . probe` (KiCad 10 `pcbnew` and `kicad-cli`
on PATH) and delete the `.kicad_prl` it leaves.

## Contents

- Outline: a rectangle on `Edge.Cuts`, 10..70 x 10..50 mm.
- Nine two-pad parts, R1 to R9, no tracks. R9 sits at (90, 30), outside the
  outline.
- Netclass patterns in the project file: `Power` = `VCC`, `HighVoltage` =
  `HV*`, `Rails` = `*VCC`. The nets `/power/VCC` and `/drv/HV_OUT` carry a
  sheet path, so they tell KiCad's whole-name match from a short-name match.
- Four more netclasses whose patterns only work as regular expressions:
  `Signals` = `SIG_[AB]`, `AnyChar` = `S.G_A`, `Drive` = `/drv/HV_(OUT|IN)`,
  and `Unanchored` = `IG_A`, which names no net.

## Oracle

- **Netclasses:** `NETINFO_ITEM.GetNetClassName()` after `pcbnew.LoadBoard`,
  which resolves the project's patterns as the board editor does.
- **Airwires:** `kicad-cli pcb drc --format json --severity-all`. With no
  tracks, each unconnected item is one ratsnest line and names its two pads
  and their positions. `generate.py` computes the lengths, the sums per net
  and per footprint, and the crossings from those endpoints.
- **Pattern probe:** the last table. Each pattern is the only netclass
  pattern of a scratch board with the fifteen nets named in its heading; the
  row lists the nets pcbnew puts in that class. The last five rows are the
  patterns `get_placement_overview` reads as wildcards only, so it names no
  net for them where KiCad names `HV22`, `HV1` and `A.B`.
- **Outside the outline:** by construction. A DRC run with all severities
  reports nothing for R9 except the missing `fixture` library.
- pcbnew 10.0.7's Python API does not give the ratsnest lines themselves:
  `GetRatsnestForNet` returns an opaque `RN_NET *`. Its
  `GetUnconnectedCount()` returns 11, the number of unconnected items in the
  DRC report.

`GND` is left out of the totals because the tool skips ground nets by default.

```
netclass of each net (pcbnew GetNetClassName):
  /drv/HV_OUT    Drive
  /power/VCC     Rails
  GND            Default
  HV             HighVoltage
  SIG_A          AnyChar,Signals
  SIG_B          Signals
  VCC            Power,Rails
ratsnest (kicad-cli pcb drc, unconnected items):
  SIG_B          R3.2 (28.8, 40.0) - R9.1 (89.2, 30.0)  61.22 mm
  GND            R8.2 (50.8, 30.0) - R9.2 (90.8, 30.0)  40.00 mm
  /power/VCC     R4.2 (30.8, 14.0) - R5.2 (60.8, 20.0)  30.59 mm
  SIG_B          R4.1 (29.2, 14.0) - R3.2 (28.8, 40.0)  26.00 mm
  SIG_A          R1.1 (19.2, 20.0) - R3.1 (27.2, 40.0)  21.54 mm
  SIG_A          R2.1 (39.2, 20.0) - R1.1 (19.2, 20.0)  20.00 mm
  VCC            R5.1 (59.2, 20.0) - R6.1 (59.2, 40.0)  20.00 mm
  GND            R2.2 (40.8, 20.0) - R1.2 (20.8, 20.0)  20.00 mm
  /drv/HV_OUT    R8.1 (49.2, 30.0) - R7.2 (50.8, 45.0)  15.09 mm
  GND            R8.2 (50.8, 30.0) - R2.2 (40.8, 20.0)  14.14 mm
  HV             R7.1 (49.2, 45.0) - R6.2 (60.8, 40.0)  12.63 mm
  net /drv/HV_OUT    15.09 mm
  net /power/VCC     30.59 mm
  net GND            74.14 mm
  net HV             12.63 mm
  net SIG_A          41.54 mm
  net SIG_B          87.23 mm
  net VCC            20.00 mm
total without GND: 207.08 mm in 8 airwires
airwires per footprint without GND:
  R3 3 airwires 108.77 mm
  R9 1 airwires 61.22 mm
  R4 2 airwires 56.60 mm
  R5 2 airwires 50.59 mm
  R1 2 airwires 41.54 mm
  R6 2 airwires 32.63 mm
  R7 2 airwires 27.72 mm
  R2 1 airwires 20.00 mm
  R8 1 airwires 15.09 mm
crossings without GND: 4
netclass pattern probe (nets: HV1 HV22 HVX /a/HV1 hv1 D_1 D_12 A.B AxB +3V3 3V3 A+B AAB NN N):
  HV               -
  HV*              HV1 HV22 HVX
  HV?              HV1 HVX
  D_?              D_1
  hv1              hv1
  /a/*             /a/HV1
  *HV1             /a/HV1 HV1
  V1               -
  A.B              A+B A.B AAB AxB
  A\.B             A.B
  HV.*             HV1 HV22 HVX
  HV[0-9]+         HV1 HV22
  HV[^0-9]         HVX
  HV(1|X)          HV1 HVX
  HV1|HVX          HV1 HVX
  ^HV1$            HV1
  HV\d+            HV1 HV22
  HV\w+            HV1 HV22 HVX
  HV[[:digit:]]+   HV1 HV22
  (?i)hv1          HV1 hv1
  +3V3             +3V3
  A+B              A+B AAB
  N{2}             -
  HV\d{2}          -
  HV[0-9]{2}       HV22
  HV(?=1)1         HV1
  \QA.B\E          A.B
```
