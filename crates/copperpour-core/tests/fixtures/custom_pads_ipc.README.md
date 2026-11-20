# `custom_pads_*_ipc.bin`

Seven `kiapi.board.types.FootprintInstance` messages, serialised exactly as
KiCad's IPC API sent them, for stock footprints with custom-shape pads placed
by KiCad itself. `pcb_components`' tests build the same footprints from the
library with `PreparedFootprintPlacement::ipc_item` and require every custom
pad's shape, anchor, size and primitives to be what KiCad holds (#679).

| File | Footprint | Placement | Custom pads |
|------|-----------|-----------|-------------|
| `custom_pads_vqfn20_0_ipc.bin` | `Package_DFN_QFN:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm` | F.Cu, 0° | 8, circle anchors, a filled `gr_poly` each |
| `custom_pads_vqfn20_90_ipc.bin` | the same | F.Cu, 90° | 8; the primitives are the same as at 0°, so they travel pad-local and unrotated |
| `custom_pads_microsd_ipc.bin` | `Connector_Card:SD-SIM_microSD-microSIM_Molex_104168-1620` | F.Cu, 0° | 2, rect anchors, `gr_poly` and `gr_circle` |
| `custom_pads_wuerth_ipc.bin` | `Mounting_Wuerth:Mounting_Wuerth_WA-SMSE-ExternalM3_H10mm_9771100360` | F.Cu, 0° | 3, circle anchors, `gr_arc` and `gr_line` (unfilled) |
| `custom_pads_fiducial_ipc.bin` | `Fiducial:Fiducial_Cross_1.5mm_Mask2mm` | F.Cu, 0° | 1, circle anchor, `gr_rect` |
| `custom_pads_sfh2201_0_ipc.bin` | `OptoDevice:Osram_SFH2201` | F.Cu, 0° | 2, rect anchors, a `gr_poly` each; no rule areas, so the refresh can compare it |
| `custom_pads_sfh2201_270_ipc.bin` | the same | F.Cu, 270° | 2; rotated and rect-anchored |

The maintainer asked for one capture per anchor shape on #679: the microSD
connector and the SFH2201 have rect anchors, the others circle anchors. The
microSD connector also has footprint rule areas, which the refresh does not
carry, so the SFH2201 gives the refresh a rect-anchored footprint to compare.
Together they hold every primitive type KiCad 10's stock library uses for
custom pads. The tests build them from KiCad's own resave of the same stock
footprints: `pad_primitives_*_kicad10.kicad_mod`
(`pad_primitives_kicad10.README.md`) and `custom_pads_sfh2201_kicad10.kicad_mod`
(below).

## Provenance

| | |
|---|---|
| KiCad | 10.0.8, Windows 11 |
| Source boards | Scratch 2-layer boards made with pcbnew Python from the stock library, with nothing else on them: `FootprintLoad`, `SetFPID`, `SetReference`, `SetPosition`, `SetOrientationDegrees`, `SaveBoard`. First board: U1 (VQFN at 0°, 100/100 mm), U2 (VQFN at 90°, 110/100 mm), J1 (130/100 mm), H1 (160/100 mm), FID1 (100/120 mm). Second board: D1 (SFH2201 at 0°, 100/100 mm), D2 (SFH2201 at 270°, 115/100 mm) |
| Transport | `GetItems` for `KOT_PCB_FOOTPRINT` over the running pcbnew's IPC socket, each item's `Any.value` written as is |
| Captured | 2026-11-20 |

| File | SHA-256 (first 16) | Bytes |
|---|---|---|
| `custom_pads_vqfn20_0_ipc.bin` | `b0bc5cf080137102` | 16234 |
| `custom_pads_vqfn20_90_ipc.bin` | `d18269f0d94d73c7` | 16522 |
| `custom_pads_microsd_ipc.bin` | `4fef8cfb073778ba` | 16034 |
| `custom_pads_wuerth_ipc.bin` | `e8e93f486fa98c70` | 14160 |
| `custom_pads_fiducial_ipc.bin` | `e5bc4bf23479ccb2` | 2585 |
| `custom_pads_sfh2201_0_ipc.bin` | `2341f2534fe25824` | 7060 |
| `custom_pads_sfh2201_270_ipc.bin` | `02ae5d98f7bf434f` | 7159 |
| `custom_pads_sfh2201_kicad10.kicad_mod` | `e922feabcc7f8cec` | 6169 |

`custom_pads_sfh2201_kicad10.kicad_mod` is `OptoDevice:Osram_SFH2201` loaded
from the stock library and saved unchanged with KiCad 10.0.8's
`PCB_IO_KICAD_SEXPR().FootprintSave` (CRLF, as written on Windows).

Redaction: none needed. The printable strings are UUIDs, the stock references
and values, and the stock 3D model path `${KICAD10_3DMODEL_DIR}/...`.

## What KiCad reports that the builder does not send

Each custom primitive comes back with its own `id`, the pad as `parent`, and an
empty `net`. KiCad assigns them on creation, so the tests compare past them.
Everything else is compared as KiCad sent it: the pad's shape `PSS_CUSTOM`,
`custom_anchor_shape`, size (the anchor's), position and angle, and each
primitive's geometry, stroke width and style (solid), fill and layer (F.Cu).

## Regenerating

Build the board with KiCad's Python (`bin\python.exe` on Windows):

```python
import os, pcbnew
stock = r"C:\Program Files\KiCad\10.0\share\kicad\footprints"
board = pcbnew.NewBoard("custom_pads.kicad_pcb")
for lib, name, ref, x, y, angle in [
    ("Package_DFN_QFN", "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm", "U1", 100, 100, 0),
    ("Package_DFN_QFN", "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm", "U2", 110, 100, 90),
    ("Connector_Card", "SD-SIM_microSD-microSIM_Molex_104168-1620", "J1", 130, 100, 0),
    ("Mounting_Wuerth", "Mounting_Wuerth_WA-SMSE-ExternalM3_H10mm_9771100360", "H1", 160, 100, 0),
    ("Fiducial", "Fiducial_Cross_1.5mm_Mask2mm", "FID1", 100, 120, 0),
]:  # the second board: ("OptoDevice", "Osram_SFH2201", "D1", 100, 100, 0), (..., "D2", 115, 100, 270)
    fp = pcbnew.FootprintLoad(os.path.join(stock, lib + ".pretty"), name)
    fp.SetFPID(pcbnew.LIB_ID(lib, name))
    fp.SetReference(ref)
    fp.SetPosition(pcbnew.VECTOR2I(pcbnew.FromMM(x), pcbnew.FromMM(y)))
    fp.SetOrientationDegrees(angle)
    board.Add(fp)
pcbnew.SaveBoard("custom_pads.kicad_pcb", board)
```

Then open it in pcbnew with the API enabled and write `GetItems` for
footprints, one file per item's `Any.value`. New captures differ in their UUIDs
only.
