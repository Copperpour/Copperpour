# rename_symbol fixtures

A small symbol library saved by KiCad, and KiCad's own resave of it after each
rename `rename_symbol` makes, used to check that a renamed library is exactly
what KiCad saves.

## Provenance

`rename_symbol_source_kicad10.kicad_sym` holds four symbols from the KiCad
10.0.8 standard library, copied unedited into one library file and then saved
by `kicad-cli sym upgrade --force` (KiCad 10.0.8, Windows 11):
- `Device:C_Feedthrough`, and `Device:Filter_EMI_C`, which extends it
- `Device:R`
- `74xx:74LS00`, with five units and a De Morgan body style

KiCad saved them in its order, by inheritance depth and then name, and a
second `kicad-cli sym upgrade --force` leaves the file byte for byte as it is.

Each other fixture is `kicad-cli sym upgrade --force` (KiCad 10.0.8) of the
source after one `rename_symbol` call. In each case KiCad's resave is byte for
byte the file `rename_symbol` wrote.

| Fixture | Rename | SHA-256 (first 16) |
|---|---|---|
| `rename_symbol_source_kicad10.kicad_sym` | none | `99063aaa651687a5` |
| `rename_symbol_r_kicad10.kicad_sym` | `R` to `A_Resistor` | `2eaafa9f42313ea4` |
| `rename_symbol_parent_kicad10.kicad_sym` | `C_Feedthrough` to `Z_Feedthrough` | `987a0600b7e5f626` |
| `rename_symbol_units_kicad10.kicad_sym` | `74LS00` to `SN74LS00` | `c7b8353fd06afd6e` |

## What they show

- KiCad writes a symbol's unit blocks from its name, so they follow a rename,
  and a derived symbol's `extends` names its parent's new name.
- The Value follows the name when it equalled the old name.
- KiCad saves by inheritance depth, then name: `A_Resistor` moves before
  `C_Feedthrough`, `Z_Feedthrough` after `R` but before the derived
  `Filter_EMI_C`, and `SN74LS00` after `R`.
