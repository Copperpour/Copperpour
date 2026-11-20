# Custom pad fixtures

Four stock KiCad footprints with custom-shape pads, each loaded and saved
unchanged by KiCad's own footprint writer. `pad_primitives`' tests read every
custom pad in them and require the `(options …)` and `(primitives …)` it writes
back to be KiCad's text, byte for byte.

Together they hold every primitive type and option KiCad 10's stock library
uses for custom pads:

| Fixture | Library footprint | Custom pads | What they show |
|---|---|---|---|
| `pad_primitives_vqfn20_kicad10.kicad_mod` | `Package_DFN_QFN:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm` | 8 | circle anchors, a filled `gr_poly` each, all points on one line |
| `pad_primitives_microsd_kicad10.kicad_mod` | `Connector_Card:SD-SIM_microSD-microSIM_Molex_104168-1620` | 2 | rect anchors, `gr_poly` whose points wrap at column 99, `gr_circle` |
| `pad_primitives_wuerth_kicad10.kicad_mod` | `Mounting_Wuerth:Mounting_Wuerth_WA-SMSE-ExternalM3_H10mm_9771100360` | 3 | `gr_arc` and `gr_line` (unfilled, width 0.2) |
| `pad_primitives_fiducial_kicad10.kicad_mod` | `Fiducial:Fiducial_Cross_1.5mm_Mask2mm` | 1 | `gr_rect`, the only stock footprint with it |

In KiCad 10.0.8's stock library, custom pads use `(clearance outline)` only
(2,027 pads, none `convexhull`) and circle (1,686) or rect (341) anchors; their
primitives are `gr_arc`, `gr_line`, `gr_poly`, `gr_circle` and `gr_rect`. Three
of them (`Package_SON:ST_PowerFLAT_HV-8_5x6mm`, two in
`Transistor_Power:GaN_Systems_GaNPX-4_7x8.4mm`) have polygons with arc
segments in their outline, which the model refuses; the stock sweep test checks
that no others are refused.

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Windows 11 |
| How | KiCad's Python: `PCB_IO_KICAD_SEXPR().FootprintLoad(<stock .pretty>, name, True)`, then `FootprintSave` into an empty `.pretty`, nothing changed in between |
| Written | 2026-11-20 |
| Line endings | CRLF, as KiCad wrote them on Windows (the directory is `-text`); the model writes `\n`, so the tests compare with line endings normalised |

| Fixture | SHA-256 (first 16) | Bytes |
|---|---|---|
| `pad_primitives_vqfn20_kicad10.kicad_mod` | `b3749a00425293e4` | 13478 |
| `pad_primitives_microsd_kicad10.kicad_mod` | `8f533251fd1c7351` | 13447 |
| `pad_primitives_wuerth_kicad10.kicad_mod` | `30808f54e8488a1a` | 10390 |
| `pad_primitives_fiducial_kicad10.kicad_mod` | `9ee21bef238a8227` | 2534 |

The stock files carry no UUIDs, so KiCad gave every item a new one on load.
Nothing in these tests compares UUIDs.

## Regenerating

```python
import os, pcbnew
stock = r"C:\Program Files\KiCad\10.0\share\kicad\footprints"
io = pcbnew.PCB_IO_KICAD_SEXPR()
for library, name in [
    ("Package_DFN_QFN", "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm"),
    ("Connector_Card", "SD-SIM_microSD-microSIM_Molex_104168-1620"),
    ("Mounting_Wuerth", "Mounting_Wuerth_WA-SMSE-ExternalM3_H10mm_9771100360"),
    ("Fiducial", "Fiducial_Cross_1.5mm_Mask2mm"),
]:
    fp = io.FootprintLoad(os.path.join(stock, library + ".pretty"), name, True)
    os.makedirs(library + ".pretty", exist_ok=True)
    io.FootprintSave(library + ".pretty", fp)
```

Run it with KiCad's own Python (`bin\python.exe` on Windows). The new files
differ only in their UUIDs.
