# edit_footprint_pad: corner ratio and custom shape

Three versions of the stock `Package_DFN_QFN:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm`,
each written by KiCad's own footprint writer. They are the expected results for
`edit_footprint_pad`'s `roundrect_rratio` and `custom_shape` (#675).

| Fixture | What KiCad changed |
|---|---|
| `custom_pad_edit_before_kicad10.kicad_mod` | nothing: the stock footprint loaded and saved |
| `custom_pad_edit_ratio_kicad10.kicad_mod` | pad 2's corner ratio, 0.25 to 0.1 (`SetRoundRectRadiusRatio`) |
| `custom_pad_edit_custom_kicad10.kicad_mod` | exposed pad 21 made a custom pad: rect anchor, a filled cross polygon (width 0) and a filled circle of radius 0.25 at (0.9, 0.9) (`SetShape(PAD_SHAPE_CUSTOM)`, `SetAnchorPadShape(PAD_SHAPE_RECTANGLE)`, `AddPrimitiveShape`) |

Against `before`, KiCad's only changes are the ones in the table: the ratio
line in `ratio`; in `custom`, the shape token `rect` to `custom` and the
`(options …)` and `(primitives …)` blocks before the pad's `uuid`. The pad
keeps its size, which is the anchor's size.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Windows 11 |
| How | KiCad's Python, `mywarm`'s `make_custom_pad_edit_fixtures.py` (below): `before` is `FootprintLoad` of the stock library and `FootprintSave`; `ratio` and `custom` load `before` with its UUIDs, make the one change through the `PAD` setters, and save |
| Written | 2026-11-20 |
| Line endings | CRLF, as KiCad wrote them on Windows (the directory is `-text`) |

| Fixture | SHA-256 (first 16) | Bytes |
|---|---|---|
| `custom_pad_edit_before_kicad10.kicad_mod` | `a94f3cc4da0d4489` | 13478 |
| `custom_pad_edit_ratio_kicad10.kicad_mod` | `f79fb805ca69b63c` | 13477 |
| `custom_pad_edit_custom_kicad10.kicad_mod` | `533b88679fa47ba9` | 13900 |

On every load KiCad gives the four standard fields new UUIDs, even when told to
keep them, so the tests compare with UUIDs masked and everything else byte for
byte.

## Regenerating

```python
import os, pcbnew
stock = r"C:\Program Files\KiCad\10.0\share\kicad\footprints\Package_DFN_QFN.pretty"
name = "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm"
MM = 1_000_000
io = pcbnew.PCB_IO_KICAD_SEXPR()

def pad(fp, number):
    return next(p for p in fp.Pads() if p.GetNumber() == number)

os.makedirs("before.pretty", exist_ok=True)
io.FootprintSave("before.pretty", io.FootprintLoad(stock, name, True))

fp = io.FootprintLoad("before.pretty", name, True)
pad(fp, "2").SetRoundRectRadiusRatio(pcbnew.F_Cu, 0.1)
os.makedirs("ratio.pretty", exist_ok=True)
io.FootprintSave("ratio.pretty", fp)

fp = io.FootprintLoad("before.pretty", name, True)
ep = pad(fp, "21")
ep.SetShape(pcbnew.F_Cu, pcbnew.PAD_SHAPE_CUSTOM)
ep.SetAnchorPadShape(pcbnew.F_Cu, pcbnew.PAD_SHAPE_RECTANGLE)
cross = [(-1.2, -0.3), (-0.3, -0.3), (-0.3, -1.2), (0.3, -1.2), (0.3, -0.3), (1.2, -0.3),
         (1.2, 0.3), (0.3, 0.3), (0.3, 1.2), (-0.3, 1.2), (-0.3, 0.3), (-1.2, 0.3)]
poly = pcbnew.PCB_SHAPE(None, pcbnew.SHAPE_T_POLY)
poly.SetPolyPoints([pcbnew.VECTOR2I(int(x * MM), int(y * MM)) for x, y in cross])
poly.SetWidth(0); poly.SetFilled(True); poly.thisown = False
ep.AddPrimitiveShape(pcbnew.F_Cu, poly)
circle = pcbnew.PCB_SHAPE(None, pcbnew.SHAPE_T_CIRCLE)
circle.SetCenter(pcbnew.VECTOR2I(int(0.9 * MM), int(0.9 * MM)))
circle.SetEnd(pcbnew.VECTOR2I(int(1.15 * MM), int(0.9 * MM)))
circle.SetWidth(0); circle.SetFilled(True); circle.thisown = False
ep.AddPrimitiveShape(pcbnew.F_Cu, circle)
os.makedirs("custom.pretty", exist_ok=True)
io.FootprintSave("custom.pretty", fp)
```

Run it with KiCad's own Python (`bin\python.exe` on Windows).
