# Sheet-pin edge fixture

`sheet_pin_edges.kicad_sch` and `child.kicad_sch` reproduce the two-net
hierarchy from mixelpixx/Konnect#687. Generated through the local Konnect 0.12.1 MCP
executable
using stock `Device:R` symbols and hierarchical labels. Pin A's intended left
edge was set to rotation 180 before both sheets were force-resaved by KiCad
10.0.6 (`kicad-cli sch upgrade --force`). The committed pair is KiCad's
serialization, with real library records, UUIDs and hierarchy instances.

The sheet is at (101.6, 101.6), size (50.8, 30.48). A is at
(101.6, 106.68, 180); B at (152.4, 106.68, 0). Both have matching child labels.

Independent netlist control, observed on 2026-10-01:

```text
kicad-cli sch export netlist --output clean.net sheet_pin_edges.kicad_sch
/child/A: R1.1
/child/B: R2.1
```

Changing only A's stored rotation from 180 to 0, then running
`kicad-cli sch upgrade --force sheet_pin_edges.kicad_sch`, moves A to
(152.4, 106.68, 0), exactly onto B. The next exported netlist contains:

```text
/child/A: R1.1, R2.1
/child/B: absent
```

The served-dispatch regressions start from the committed clean pair and
change only A's `at` record to probe each edge, its finite span, and an
unsupported rotation. Expected sides and coordinates are literal KiCad
fixture values, not calculated by the classifier under test. Each call checks
that both input files remain byte-identical. The original mismatching record
must be checked before KiCad resaves it: this validation does not reconstruct
intent after KiCad has already moved a pin and merged its nets.
