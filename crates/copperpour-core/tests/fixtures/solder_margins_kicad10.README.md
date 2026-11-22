# Solder mask and paste margin footprints

Nine versions of the same footprint, each written by KiCad. The first five
check that `edit_footprint_pad` and `set_footprint_metadata` write a pad's and
a footprint's own solder mask and paste margins exactly as KiCad does. The last
four are library changes `update_footprints_from_library` refreshes from
([#723](https://github.com/Copperpour/Copperpour/issues/723)).

The footprint is
`Package_SON.pretty/Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm` from the
KiCad 10.0.8 standard library (format 20260206). It has no margins of its
own; its exposed pad is pad `9`.

## Provenance

The first five files were written by KiCad 10.0.8's own footprint writer on
Windows 11, through its Python bindings (`pcbnew.PCB_IO_KICAD_SEXPR`):
`FootprintLoad(library, name, True)`, one change through the setters named
below, then `FootprintSave`. The last four were written the same way by KiCad
10.0.8 on Linux x86_64, each from the first-five file named below, keeping
the stock footprint name; they end their lines with LF where the first five
have CRLF. Nothing was edited by hand.

| Fixture | Change made in KiCad | SHA-256 (first 16) |
|---|---|---|
| `solder_margins_none_kicad10.kicad_mod` | none (load and save) | `07db3d5c7c4a53b0` |
| `solder_margins_ep_kicad10.kicad_mod` | pad `9`: `SetLocalSolderMaskMargin(-0.05 mm)`, `SetLocalSolderPasteMargin(-0.1 mm)`, `SetLocalSolderPasteMarginRatio(-0.2)` | `a92a73bb733fc7bb` |
| `solder_margins_ep_zero_kicad10.kicad_mod` | pad `9`: the same three set to 0 | `fb68a75f9728822c` |
| `solder_margins_footprint_kicad10.kicad_mod` | footprint: `SetLocalSolderMaskMargin(0.05 mm)`, `SetLocalSolderPasteMargin(-0.03 mm)`, `SetLocalSolderPasteMarginRatio(-0.1)` | `ac0b381216859776` |
| `solder_margins_footprint_zero_kicad10.kicad_mod` | footprint: the same three set to 0 | `62fd1d34cf3e7c38` |
| `solder_margins_ep_changed_kicad10.kicad_mod` | from `ep`: pad `9`: `SetLocalSolderMaskMargin(0.03 mm)` | `54ea1e4e96fe80be` |
| `solder_margins_footprint_changed_kicad10.kicad_mod` | from `footprint`: footprint: `SetLocalSolderMaskMargin(-0.02 mm)` | `f653a8570a431682` |
| `solder_margins_clearance_kicad10.kicad_mod` | from `none`: footprint: `SetLocalClearance(0.2 mm)` | `c53f7d38f4a53a58` |
| `solder_margins_zone_connect_kicad10.kicad_mod` | from `none`: footprint: `SetLocalZoneConnection(ZONE_CONNECTION_FULL)` | `f5ec6ce989cfa4d0` |

## What they show

- On a pad, KiCad writes `(solder_mask_margin …)`, `(solder_paste_margin …)`
  and `(solder_paste_margin_ratio …)` after `(layers …)` and before
  `(zone_connect …)`, in that order.
- On a footprint, it writes them after the last `(property …)` and before
  `(attr …)`.
- A margin of 0 is written like any other value, and nothing for no override.
- The stock file carries no UUIDs, so KiCad gives every item new ones on each
  load. The tests compare files with their UUIDs masked; everything else must
  match KiCad's byte for byte.
