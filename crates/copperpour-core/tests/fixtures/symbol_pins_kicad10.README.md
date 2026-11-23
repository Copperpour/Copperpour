# get_symbol_info pin library

A symbol library KiCad saved, used to check that `get_symbol_info` lists every
pin, counts pins as KiCad does and warns about duplicates by KiCad's checker
rule (#712).

## Provenance

The symbols were drawn for these tests, not copied from KiCad's libraries, so
nothing CC BY-SA is committed; the tests check KiCad's own stock examples
(`74LS00`, `74LS78`, `TL431D`) on the installed libraries instead, and skip
without them. The library was written as a KiCad 9 draft (format 20241209) and
saved by `kicad-cli sym upgrade --force` (KiCad 10.0.8 on macOS, format
20251024); a second resave leaves it byte for byte the same. KiCad wrote every
byte of `symbol_pins_kicad10.kicad_sym`. SHA-256 (first 16): `ff2404f96aa1fba3`.

| Symbol | Pins | What it shows |
|---|---|---|
| `GATE` | unit 1 in body styles 1 and 2 (De Morgan, pins `1`-`3` in each; KiCad adds `(body_styles demorgan)`), power unit 2 in body style 0 (pins `7`, `14`) | 8 pins listed, `pin_count` 5 |
| `GATE_ALT` | extends `GATE` | its root's pins, `pin_count` 5 |
| `SHARED` | units 1 and 2 in body style 1, clock pin `1` drawn in both, pins `2`-`5` | `pin_count` 6, a duplicate warning for pin `1` |
| `STACKED` | `1`, `[2-4]`, `5`, `[6,8]` | 4 pins listed, `pin_count` 7 |
| `DUP` | `1`, `2` (`NC`, `no_connect`, x 5.08), `2` (`NC`, `passive`, x 6.35), `3` | `pin_count` 4, a duplicate warning for pin `2` |
| `PLAIN` | `1`, `2` | `pin_count` 2, no warning |

The counts and warnings are those of KiCad 10.0.8's `LIB_SYMBOL::GetPinCount`
(`eeschema/lib_symbol.cpp`) and `CheckDuplicatePins`
(`eeschema/symbol_checker.cpp`).

## Regenerating

The draft is one line, `(kicad_symbol_lib (version 20241209) (generator
"kicad_symbol_editor") (generator_version "9.0") …)`, holding the symbols
above, each with Reference, Value and hidden empty Footprint, Datasheet and
Description properties, and its pins (length 2.54, names and numbers in
1.27 mm fonts) in `NAME_<unit>_<body style>` units. Save it with
`kicad-cli sym upgrade --force -o symbol_pins_kicad10.kicad_sym <draft>`.
