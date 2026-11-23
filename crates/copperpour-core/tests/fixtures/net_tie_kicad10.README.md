# Net-tie footprints

Three of the twelve net-tie footprints in KiCad 10.0.8's standard library,
used to check that live placement, Update PCB and
`update_footprints_from_library` carry a footprint's net-tie pad groups
([#85](https://github.com/Copperpour/Copperpour/issues/85)).

## Provenance

Copied unchanged from `NetTie.pretty` in KiCad 10.0.8's standard library
(the Ubuntu 24.04 `kicad-footprints` package, version 10.0.8, format
20260206). Nothing was edited by hand, and the repository stores the
directory `-text`, so the bytes (LF line endings) are KiCad's.

| Fixture | Library footprint | Pad group | SHA-256 (first 16) |
|---|---|---|---|
| `net_tie_2_smd_pad0.5mm_kicad10.kicad_mod` | `NetTie-2_SMD_Pad0.5mm` | `"1, 2"` | `6fa0592afb0af821` |
| `net_tie_3_tht_pad0.3mm_kicad10.kicad_mod` | `NetTie-3_THT_Pad0.3mm` | `"1, 2, 3"` | `9acc1669d889bc5e` |
| `net_tie_4_smd_pad0.5mm_kicad10.kicad_mod` | `NetTie-4_SMD_Pad0.5mm` | `"1, 2, 3, 4"` | `3eb595bf3c76b7d5` |

## What they show

- KiCad writes one `(net_tie_pad_groups …)` after `(attr …)`, holding each
  group as a quoted string of its pad numbers joined with `", "`
  (`PCB_IO_KICAD_SEXPR::format`). Every group here names every pad.
- The SMD footprints have round `smd` pads on `F.Cu` bridged by one filled
  `fp_poly` on `F.Cu`; the THT one has round plated pads on `*.Cu` bridged
  on both `F.Cu` and `B.Cu`. Nothing else in them is refused by the typed
  library refresh, so a refresh reads them whole.
- KiCad's report of a placed stock net tie is `issue_300_u8.ipc.bin`
  (`issue_300_ipc.README.md`): the Antenova antenna's `"1, 2"` comes back as
  one `NetTieDefinition` with pad numbers `1` and `2`.
