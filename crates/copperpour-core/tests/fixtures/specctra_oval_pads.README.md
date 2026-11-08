# Specctra fixture: oval pads

`specctra_oval_pads.kicad_pcb` is `specctra_two_resistors.kicad_pcb` with three
footprints added (#384):
- J1: KiCad's stock `Connector_JST:JST_PH_B2B-PH-K_1x02_P2.00mm_Vertical` at
  (92, 60), turned 90 degrees. Pad 1 is a rounded rectangle on VCC; pad 2 is a
  through-hole oval, 1.2 x 1.75 mm, on GND.
- R3 and R4: copies of R2 whose SMD pads were made oval. R3's pad 1 is
  0.6 x 0.9 mm, taller than wide, and its pad 2 0.9 x 0.6 mm, wider than tall.
  R4's pads are 0.7 x 0.7 mm. Pad 1 is on VCC and pad 2 on GND.

It was built with KiCad 10.0.7's `pcbnew` Python and saved through
`pcbnew.SaveBoard`. `specctra_oval_pads.native-kicad-10.dsn` is KiCad's
`pcbnew.ExportSpecctraDSN` of the saved board, with only the absolute output
path in the root `(pcb ...)` atom normalized to `board.dsn`. KiCad writes the
three oval sizes as `(path F.Cu 600 0 -150 0 150)`, `(path F.Cu 600 -150 0 150 0)`
and `(path F.Cu 700 0 0 0 0)`, and the through-hole one on F.Cu and B.Cu.

```python
# Usage: python3 -I make384.py <specctra_two_resistors.kicad_pcb> <out_dir>
import os, sys, pcbnew
src, out = sys.argv[1:3]
lib = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/Connector_JST.pretty"
name = "JST_PH_B2B-PH-K_1x02_P2.00mm_Vertical"
b = pcbnew.LoadBoard(src)
vcc, gnd = b.FindNet("VCC"), b.FindNet("GND")
j1 = pcbnew.FootprintLoad(lib, name)
j1.SetFPID(pcbnew.LIB_ID("Connector_JST", name))
j1.SetReference("J1"); j1.SetValue("PH 2P")
j1.SetPosition(pcbnew.VECTOR2I_MM(92, 60))
j1.SetOrientationDegrees(90)
b.Add(j1)
for pad in j1.Pads():
    pad.SetNet(vcc if pad.GetNumber() == "1" else gnd)
r2 = [f for f in b.GetFootprints() if f.GetReference() == "R2"][0]
def oval_copy(ref, x, sizes):
    fp = pcbnew.Cast_to_FOOTPRINT(r2.Duplicate(False))
    b.Add(fp)
    fp.SetReference(ref)
    fp.SetPosition(pcbnew.VECTOR2I_MM(x, 60))
    for pad in fp.Pads():
        pad.SetShape(pcbnew.PAD_SHAPE_OVAL)
        pad.SetSize(pcbnew.VECTOR2I_MM(*sizes[pad.GetNumber()]))
        pad.SetNet(vcc if pad.GetNumber() == "1" else gnd)
oval_copy("R3", 116, {"1": (0.6, 0.9), "2": (0.9, 0.6)})
oval_copy("R4", 124, {"1": (0.7, 0.7), "2": (0.7, 0.7)})
path = os.path.join(out, "specctra_oval_pads.kicad_pcb")
pcbnew.SaveBoard(path, b)
pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(path),
                         os.path.join(out, "specctra_oval_pads.native-kicad-10.dsn"))
```

`specctra_oval_pads_odd.native-kicad-10.dsn` is KiCad's export of the same
board with R3's pad 1 set to 0.6 x 0.901 mm, loaded and changed in `pcbnew`
without saving, its root atom normalized the same way. Sides that differ by an
odd number of micrometres put the spine's ends on a half micrometre:
`(path F.Cu 600 0 -150.5 0 150.5)`.

`specctra_oval_pads.freerouting-2.3.0.ses` is Freerouting 2.3.0's unedited
output for Copperpour's export of the board (`export_dsn` with the tests'
0.25 mm / 0.2 mm rules). It ran on Eclipse Temurin 25.0.5 with the
`freerouting/freerouting` v2.3.0 release jar (SHA-256
`3cf18d608437740bc497db6b8ef5888e2e60a08de0def20691d1bad0c0e0ee24`) and reported
8 unrouted nets routed, 0 unrouted and 0 violations:

```text
java -Djava.awt.headless=true -jar freerouting-2.3.0.jar \
  -de specctra_oval_pads.dsn -do specctra_oval_pads.freerouting-2.3.0.ses -mp 20
```

| File | SHA-256 |
|---|---|
| `specctra_oval_pads.freerouting-2.3.0.ses` | `db9c1bab6eaf8c5b7a66bfc930518d7bce8f3c16d824301fe49a99bf185b561a` |
