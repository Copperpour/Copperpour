# set_symbol_properties field_order libraries

One symbol, saved by KiCad with its custom fields in three orders, used to check
that `field_order` (#717) leaves exactly the library KiCad saves.

## Provenance

The symbol `CAP` was drawn for these tests, not copied from KiCad's libraries,
so nothing CC BY-SA is committed. Each library was written as a KiCad 9 draft
(format 20241209) and saved by `kicad-cli sym upgrade --force` (KiCad 10.0.8 on
macOS, format 20251024); a second resave leaves each byte for byte as it is.
KiCad wrote every byte of each fixture.

`CAP` has the mandatory fields, four custom fields (`Manufacturer Number`
shown at `0 -10.16`, the others hidden) and `ki_keywords`. Each custom field is
the same block in every draft; only their order differs.

| Fixture | Custom field order | SHA-256 (first 16) |
|---|---|---|
| `symbol_field_order_source_kicad10.kicad_sym` | Manufacturer Number, Tolerance, Manufacturer, Technology | `5f465ba02910734f` |
| `symbol_field_order_manufacturer_first_kicad10.kicad_sym` | Manufacturer, Manufacturer Number, Tolerance, Technology | `5213a462554aa3b3` |
| `symbol_field_order_technology_first_kicad10.kicad_sym` | Technology, Tolerance, Manufacturer Number, Manufacturer | `f0f57e80c764bb18` |

## What they show

- KiCad keeps the custom fields in the order the file gives them, between the
  mandatory fields and `ki_keywords`: each field read takes the next ordinal
  (`SCH_FIELD::SCH_FIELD`, `eeschema/sch_field.cpp`, 10.0.8), and the writer
  saves them in ordinal order (`LIB_SYMBOL::GetFields`,
  `eeschema/lib_symbol.cpp`; `SCH_IO_KICAD_SEXPR_LIB_CACHE::SaveSymbol`).
- A reordered library differs from the source only in the order of the custom
  field blocks.

## Regenerating

Each draft is one line, `(kicad_symbol_lib (version 20241209) (generator
"kicad_symbol_editor") (generator_version "9.0") (symbol "CAP" …))`: Reference
`C`, Value `100n`, hidden empty Footprint and Datasheet, Description "A
capacitor drawn for the tests", the four custom fields in the order above
(values `CAP-0603-100N`, `10%`, `Example Corp`, `X7R`), `ki_keywords` "cap", and
unit `CAP_1_1` with passive pins 1 and 2. Save each with
`kicad-cli sym upgrade --force -o <fixture> <draft>`.
