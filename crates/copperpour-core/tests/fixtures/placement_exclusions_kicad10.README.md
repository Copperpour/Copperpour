# Library exclusions fixture

`placement_exclusions_kicad10.kicad_sym` holds four one-pin symbols, each
excluded from one thing, so the placement tests can check that a placed symbol
takes its library symbol's flags (#124). Across the four, every flag is read
both ways, and no two flags read alike.

## Provenance

1. All four made through Copperpour's `create_symbol`, which writes the four
   flags at KiCad's defaults.
2. One edit by hand in each: `EXCLUDED_FROM_SIM`'s `(exclude_from_sim no)`
   became `yes`; `EXCLUDED_FROM_BOM`'s `(in_bom yes)`, `EXCLUDED_FROM_BOARD`'s
   `(on_board yes)` and `EXCLUDED_FROM_POS_FILES`' `(in_pos_files yes)` became
   `no`. No Copperpour tool sets a library symbol's flags, and no stock KiCad
   symbol sets `(in_pos_files no)`.
3. Parsed and force-resaved by KiCad 10.0.6:

   ```text
   kicad-cli sym upgrade --force placement_exclusions_kicad10.kicad_sym
   ```

What is committed is that serialization: `(generator "kicad_symbol_editor")`,
`(version 20251024)`, the symbols in KiCad's order. KiCad kept every edit.

## Cases

| Symbol | `exclude_from_sim` | `in_bom` | `on_board` | `in_pos_files` |
|---|---|---|---|---|
| `EXCLUDED_FROM_SIM` | `yes` | `yes` | `yes` | `yes` |
| `EXCLUDED_FROM_BOM` | `no` | `no` | `yes` | `yes` |
| `EXCLUDED_FROM_BOARD` | `no` | `yes` | `no` | `yes` |
| `EXCLUDED_FROM_POS_FILES` | `no` | `yes` | `yes` | `no` |

## KiCad's own answer

All four placed by `batch_place_components`, in that order as `H1` to `H4`,
into a sheet from `create_project`: KiCad 10.0.6's `kicad-cli sch export bom`
lists every one but `H2`, and `kicad-cli sch export netlist` every one but
`H3`.
