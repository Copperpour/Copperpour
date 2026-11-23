# Pad layer footprints

Five versions of the same footprint, each written by KiCad, used to check that
`edit_footprint_pad`'s `layers` (#710) writes a pad's layers exactly as KiCad
does.

The footprint is
`Package_SON.pretty/Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm` from the
KiCad 10.0.8 standard library (format 20260206), as in
`solder_margins_kicad10.README.md`. Its pads `1`-`8` are on
`F.Cu F.Mask F.Paste`; its exposed pad `9` is on `F.Cu F.Mask`.

## Provenance

All five files were written by KiCad 10.0.8's own footprint writer on macOS,
through its Python bindings (`pcbnew.PCB_IO_KICAD_SEXPR`):
`FootprintLoad(library, name, True)`, one `PAD::SetLayerSet`, then
`FootprintSave`. Nothing was edited by hand. KiCad on macOS writes LF line
endings.

| Fixture | Change made in KiCad | SHA-256 (first 16) |
|---|---|---|
| `pad_layers_source_kicad10.kicad_mod` | none (load and save) | `6689f87551c54ba2` |
| `pad_layers_no_paste_kicad10.kicad_mod` | pad `1`: `F_Cu`, `F_Mask` | `78117a586acabdfb` |
| `pad_layers_back_kicad10.kicad_mod` | pad `1`: `B_Cu`, `B_Mask`, `B_Paste` | `df2575958ecdc273` |
| `pad_layers_ep_paste_kicad10.kicad_mod` | pad `9`: `F_Cu`, `F_Mask`, `F_Paste` | `73f325cec67563a9` |
| `pad_layers_both_sides_kicad10.kicad_mod` | pad `9`: `F_Cu`, `B_Cu`, `F_Mask`, `B_Mask` | `02ed0df813e4a7c4` |

## What they show

- Each variant differs from the source in that pad's `(layers …)` only.
- KiCad writes the layers as `PCB_IO_KICAD_SEXPR::formatLayers`
  (`pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp`, 10.0.8) orders them:
  wildcards first (`*.Cu` for a pad on front and back copper, then `*.Paste`,
  `*.Mask` for both sides), then each remaining layer in layer-ID order
  (`F.Cu`, `F.Mask`, `B.Cu`, `B.Mask`, `F.Paste`, `B.Paste`).
- The stock file has no UUIDs, so KiCad gives new ones on each load; the tests
  compare files with their UUIDs masked.

## Regenerating

With KiCad 10's Python (`python3` inside KiCad.app on macOS, `bin\python.exe`
on Windows), `<stock>` being KiCad's `footprints/Package_SON.pretty`:

```python
import os, sys, pcbnew

stock, out = sys.argv[1:3]
NAME = "Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm"


def set_layers(number, *ids):
    def change(fp):
        (pad,) = [p for p in fp.Pads() if p.GetNumber() == number]
        lset = pcbnew.LSET()
        for layer in ids:
            lset.AddLayer(layer)
        pad.SetLayerSet(lset)
    return change


VARIANTS = {
    "source": lambda fp: None,
    "no_paste": set_layers("1", pcbnew.F_Cu, pcbnew.F_Mask),
    "back": set_layers("1", pcbnew.B_Cu, pcbnew.B_Mask, pcbnew.B_Paste),
    "ep_paste": set_layers("9", pcbnew.F_Cu, pcbnew.F_Mask, pcbnew.F_Paste),
    "both_sides": set_layers("9", pcbnew.F_Cu, pcbnew.B_Cu, pcbnew.F_Mask, pcbnew.B_Mask),
}
io = pcbnew.PCB_IO_KICAD_SEXPR()
for variant, change in VARIANTS.items():
    library = os.path.join(out, f"{variant}.pretty")
    os.makedirs(library, exist_ok=True)
    fp = io.FootprintLoad(stock, NAME, True)
    change(fp)
    io.FootprintSave(library, fp)
```
