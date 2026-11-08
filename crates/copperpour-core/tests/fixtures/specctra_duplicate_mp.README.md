# Specctra fixture: two pads sharing a number

`specctra_duplicate_mp.kicad_pcb` is `specctra_two_resistors.kicad_pcb` with
KiCad's stock `Connector_JST:JST_SH_SM02B-SRSS-TB_1x02-1MP_P1.00mm_Horizontal`
added as J1 at (105, 60): pad 1 on VCC, pad 2 and both pads numbered `MP` on
GND (#338). It was built with KiCad 10.0.7's `pcbnew` Python and saved through
`pcbnew.SaveBoard`, so the serialization, pad UUIDs included, is KiCad's.

`specctra_duplicate_mp.native-kicad-10.dsn` is KiCad's
`pcbnew.ExportSpecctraDSN` of the saved board. Only the absolute output path in
the root `(pcb ...)` atom was normalized to `board.dsn`. KiCad names the two
pads `MP` and `MP@1`, in file order.

```python
# Usage: python3 -I make338.py <specctra_two_resistors.kicad_pcb> <out_dir>
import os, sys, pcbnew
src, out = sys.argv[1:3]
lib = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/Connector_JST.pretty"
name = "JST_SH_SM02B-SRSS-TB_1x02-1MP_P1.00mm_Horizontal"
b = pcbnew.LoadBoard(src)
fp = pcbnew.FootprintLoad(lib, name)
fp.SetFPID(pcbnew.LIB_ID("Connector_JST", name))
fp.SetReference("J1")
fp.SetValue("SH 2P")
fp.SetPosition(pcbnew.VECTOR2I_MM(105, 60))
b.Add(fp)
nets = {"1": b.FindNet("VCC"), "2": b.FindNet("GND"), "MP": b.FindNet("GND")}
for pad in fp.Pads():
    pad.SetNet(nets[pad.GetNumber()])
path = os.path.join(out, "specctra_duplicate_mp.kicad_pcb")
pcbnew.SaveBoard(path, b)
pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(path),
                         os.path.join(out, "specctra_duplicate_mp.native-kicad-10.dsn"))
```

`specctra_duplicate_mp.freerouting-2.3.0.ses` is Freerouting 2.3.0's unedited
output for Copperpour's export of the board (`export_dsn` with the tests'
0.25 mm / 0.2 mm rules), which names the pads `J1-MP` and `J1-MP@1`. Freerouting
ran on Eclipse Temurin 25.0.5 with the `freerouting/freerouting` v2.3.0 release
jar (SHA-256
`3cf18d608437740bc497db6b8ef5888e2e60a08de0def20691d1bad0c0e0ee24`) and
reported 6 unrouted nets routed, 0 unrouted and 0 violations:

```text
java -Djava.awt.headless=true -jar freerouting-2.3.0.jar \
  -de specctra_duplicate_mp.dsn -do specctra_duplicate_mp.freerouting-2.3.0.ses -mp 20
```

| File | SHA-256 |
|---|---|
| `specctra_duplicate_mp.freerouting-2.3.0.ses` | `1188bec2c94e5796bb072338242a6e0515e359b178f9959fde4814de1dbe027b` |
