# delete_symbol_pin libraries

A small symbol library saved by KiCad, and KiCad's own save of the same library
with one pin removed, used to check that `delete_symbol_pin` (#716) leaves
exactly the library KiCad saves.

## Provenance

The symbols were drawn for these tests, not copied from KiCad's libraries, so
nothing CC BY-SA is committed. Each library was written as a KiCad 9 draft
(format 20241209) and saved by `kicad-cli sym upgrade --force` (KiCad 10.0.8 on
macOS, format 20251024); a second resave leaves each byte for byte as it is.
KiCad wrote every byte of each fixture.

| Fixture | Content | SHA-256 (first 16) |
|---|---|---|
| `delete_symbol_pin_source_kicad10.kicad_sym` | `DUP`: pins `1`, `2` (an `NC` of type `no_connect` at x 5.08), `2` (an `NC` of type `passive` at x 6.35), `3`, all in unit 1, body style 1; `GATE`: one unit drawn in two body styles (KiCad adds `(body_styles demorgan)`), pins `1`-`3` in each; `GATE_ALT`, which extends `GATE` | `a456905aee98cefa` |
| `delete_symbol_pin_duplicate_removed_kicad10.kicad_sym` | the source without `DUP`'s passive pin `2` | `5213a920c3eea004` |
| `delete_symbol_pin_de_morgan_removed_kicad10.kicad_sym` | the source without `GATE`'s pin `1` in body style 2 | `ca8fa14a11c0592b` |

## What they show

- KiCad keeps two pins with one number in one unit and body style as written;
  its symbol checker reports them (`CheckDuplicatePins`,
  `eeschema/symbol_checker.cpp`, 10.0.8).
- Without the pin, KiCad's save differs from the source by that pin's block and
  its line only.

## Regenerating

Each draft is one line: `(kicad_symbol_lib (version 20241209) (generator
"kicad_symbol_editor") (generator_version "9.0") …)` holding the symbols above,
each with Reference, Value and hidden empty Footprint, Datasheet and
Description properties, pins of length 2.54 with names and numbers in 1.27 mm
fonts, and `DUP` with a background-filled rectangle in `DUP_0_1`. Save each with
`kicad-cli sym upgrade --force -o <fixture> <draft>`.
