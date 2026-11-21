# 3D model footprints

Three versions of the same footprint, each written by KiCad, used to check that
`set_footprint_models` writes a model's visibility, opacity and transforms as
KiCad does (#715).

The footprint is `Package_SON.pretty/Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm`
from the KiCad 10.0.8 standard library (format 20260206), as in
`solder_margins_kicad10.README.md`. It has one model,
`${KICAD10_3DMODEL_DIR}/Package_SON.3dshapes/Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm.step`.

## Provenance

All three were written by KiCad 10.0.8's own footprint writer on macOS, through
its Python bindings (`pcbnew.PCB_IO_KICAD_SEXPR`): `FootprintLoad(library,
name, True)`, one change through `FP_3DMODEL`, then `FootprintSave`. Nothing was
edited by hand. KiCad on macOS writes LF line endings.

| Fixture | Change made in KiCad | SHA-256 (first 16) |
|---|---|---|
| `footprint_models_source_kicad10.kicad_mod` | none (load and save) | `71e4cc69268a873a` |
| `footprint_models_hidden_kicad10.kicad_mod` | the model: `m_Show = False`, `m_Opacity = 0.5` | `ca99619e1cc6e9bc` |
| `footprint_models_two_models_kicad10.kicad_mod` | a second `FP_3DMODEL` appended: `${KIPRJMOD}/models/extra.step`, offset `0.1 -0.2 0`, rotation `0 0 90` | `cedce855bfc3ad52` |

## What they show

- KiCad writes `(hide yes)` for a hidden model and `(opacity 0.5000)` for a
  transparent one, in that order, before `(offset …)`
  (`PCB_IO_KICAD_SEXPR::format( const FOOTPRINT* )`,
  `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp`, 10.0.8), and each
  transform with its `(xyz …)` on its own line.
- Each variant differs from the source in its models only. The stock file has
  no UUIDs, so KiCad gives new ones on each load; the tests compare files with
  their UUIDs masked.

## Regenerating

With KiCad 10's Python, `<stock>` being KiCad's `footprints/Package_SON.pretty`:

```python
import os, sys, pcbnew

stock, out = sys.argv[1:3]
NAME = "Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm"


def hidden(fp):
    model = fp.Models()[0]
    model.m_Show = False
    model.m_Opacity = 0.5


def two_models(fp):
    extra = pcbnew.FP_3DMODEL()
    extra.m_Filename = "${KIPRJMOD}/models/extra.step"
    extra.m_Offset = pcbnew.VECTOR3D(0.1, -0.2, 0.0)
    extra.m_Rotation = pcbnew.VECTOR3D(0.0, 0.0, 90.0)
    fp.Models().push_back(extra)


io = pcbnew.PCB_IO_KICAD_SEXPR()
for variant, change in {"source": lambda fp: None, "hidden": hidden, "two_models": two_models}.items():
    library = os.path.join(out, f"{variant}.pretty")
    os.makedirs(library, exist_ok=True)
    fp = io.FootprintLoad(stock, NAME, True)
    change(fp)
    io.FootprintSave(library, fp)
```
