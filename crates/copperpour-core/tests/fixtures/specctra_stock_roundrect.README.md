# Stock rounded-rectangle Specctra fixture

`specctra_stock_roundrect.kicad_pcb` is a two-layer board built only from stock
KiCad 10 footprints, each of which has rounded-rectangle (`roundrect`) pads.
It is the fixture for mixelpixx/Konnect#790. KiCad 10.0.6's `pcbnew` Python API built
and saved
it with the script below. Every byte of the board is KiCad's serialization.

```python
import sys, pcbnew
b = pcbnew.BOARD()
b.SetCopperLayerCount(2)
mm = pcbnew.FromMM
lib = "/usr/share/kicad/footprints/"
nets = {}
for n in ["VCC", "GND", "SIG"]:
    ni = pcbnew.NETINFO_ITEM(b, n); b.Add(ni); nets[n] = ni
parts = [("Resistor_SMD", "R_0402_1005Metric", "R1", 100, 50, ("VCC", "SIG")),
         ("Capacitor_SMD", "C_0603_1608Metric", "C1", 110, 50, ("SIG", "GND")),
         ("Resistor_SMD", "R_0603_1608Metric", "R2", 105, 60, ("VCC", "GND"))]
for lib_name, fp_name, ref, x, y, pad_nets in parts:
    fp = pcbnew.FootprintLoad(lib + lib_name + ".pretty", fp_name)
    fp.SetReference(ref)
    fp.SetPosition(pcbnew.VECTOR2I(mm(x), mm(y)))
    b.Add(fp)
    for pad, net in zip(sorted(fp.Pads(), key=lambda p: p.GetNumber()), pad_nets):
        pad.SetNet(nets[net])
pts = [(90, 40), (120, 40), (120, 70), (90, 70)]
for i in range(4):
    s = pcbnew.PCB_SHAPE(b); s.SetShape(pcbnew.SHAPE_T_SEGMENT)
    s.SetStart(pcbnew.VECTOR2I(mm(pts[i][0]), mm(pts[i][1])))
    s.SetEnd(pcbnew.VECTOR2I(mm(pts[(i + 1) % 4][0]), mm(pts[(i + 1) % 4][1])))
    s.SetLayer(pcbnew.Edge_Cuts); s.SetWidth(mm(0.05)); b.Add(s)
pcbnew.SaveBoard(sys.argv[1], b)
```

`specctra_stock_roundrect.native-kicad-10.dsn` is that board as exported by
KiCad 10.0.6's `pcbnew.ExportSpecctraDSN(board, path)`. Only the absolute output
path in the root `(pcb ...)` atom was normalized to `board.dsn`.

Three `.freerouting-2.3.0.ses` files are Freerouting 2.3.0's unedited output.
The jar is the `freerouting/freerouting` v2.3.0 release asset (SHA-256
`3cf18d608437740bc497db6b8ef5888e2e60a08de0def20691d1bad0c0e0ee24`):

```text
java -Djava.awt.headless=true -jar freerouting-2.3.0.jar \
  -de specctra_stock_roundrect.dsn -do specctra_stock_roundrect.freerouting-2.3.0.ses -mp 20
java -Djava.awt.headless=true -jar freerouting-2.3.0.jar \
  -de specctra_stock_roundrect.native-kicad-10.dsn \
  -do specctra_stock_roundrect.native-kicad-10.freerouting-2.3.0.ses -mp 20
```

The first input is Copperpour's Rust export of the board, using the default-netclass
rules in the tests. It is deterministic, so the tests regenerate it rather than
commit it. The second input is the native DSN above. Freerouting reported
`0 unrouted and 0 violations` for both. The first file was routed again for
[#250](https://github.com/Copperpour/Copperpour/issues/250) from the export with `copperpour_*` names (input DSN SHA-256
`8dfd3896df70d8e2d1b952a2808804fbe07f0feff4d561caf8ac327474ba65aa`).

### A job from before the rename ([#250](https://github.com/Copperpour/Copperpour/issues/250))

Earlier releases named the Rust export's padstacks, images and classes
`konnect_*`. `specctra_stock_roundrect.konnect.freerouting-2.3.0.ses` is the
first command's output from such a release, committed for mixelpixx/Konnect#790
and renamed here without edits. Its parser block records the exporter as
`Konnect 0.13.0`. `specctra_stock_roundrect.konnect-manifest.json` is the
reverse manifest `main` at 1349a0be, the last commit with `konnect_*` names,
wrote for the same board and rules. Only its absolute `board_path` was
normalized to `specctra_stock_roundrect.kicad_pcb`, and the test points it at
the board it writes. The planner checks the parser block's shape, not its values, so this pair stands for
a job left by an earlier release; `main` at 1349a0be already accepted it.

| file | SHA-256 |
|---|---|
| `specctra_stock_roundrect.freerouting-2.3.0.ses` | `37f3f76931fe5ea7f9c39ef0371eb0a8121ad193d08f407eadfabccedc2306f5` |
| `specctra_stock_roundrect.konnect.freerouting-2.3.0.ses` | `9c40c6ca1748fc7f573508fe113398d541e2336d984eb63021dec3c2d0412894` |
| `specctra_stock_roundrect.konnect-manifest.json` | `b9d556addec7f70ff0742cfa1e4b7cfbe8e489d36b13ac190a10ff536712616c` |

## Oracle

KiCad's exporter names each rounded-rectangle padstack after the pad:
`RoundRect[T]Pad_<width>x<height>_<radius>_um`. The radius in the name already
includes the growth KiCad adds before approximating the corner,
`r * (1 - cos(pi / 36))`. The tests assert Copperpour's manifest against these
rows:

| footprint | KiCad padstack | size (µm) | KiCad radius (µm) | corner radius (µm) |
|---|---|---|---|---|
| R_0402_1005Metric | `RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0` | 540 × 640 | 135.514 | 135 |
| R_0603_1608Metric | `RoundRect[T]Pad_800.000000x950.000000_200.761000_um_0.000000_0` | 800 × 950 | 200.761 | 200 |
| C_0603_1608Metric | `RoundRect[T]Pad_900.000000x950.000000_225.856000_um_0.000000_0` | 900 × 950 | 225.856 | 225 |

## What the fixture leaves out

Every footprint sits at 0°, and every pad centre falls on a whole micrometre.
Two pre-existing defects, not related to pad shape, would otherwise stop the
tests before they reach the pads:

- mixelpixx/Konnect#840: the Rust exporter writes a rotated footprint's pad angle twice.
- [#98](https://github.com/Copperpour/Copperpour/issues/98): native adoption refuses
  stock 0805 pads at ±912.5 µm. It also refuses
  courtyard `(outline (polygon ...))` lines, which is why the native tests drop
  those three lines in test code. The fixture file keeps them.
