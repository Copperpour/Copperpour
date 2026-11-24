# Field placements on the fields fixtures

`symbol_fields_kicad10.placed.kicad_sym` is what `set_symbol_properties`
writes for these two calls on `symbol_fields_kicad10.kicad_sym`:

```text
set_symbol_properties FIELDS_BARE  field_placements {Reference: {y: 6.35},
                                                     Value: {y: -6.35, rotation: 90},
                                                     Footprint: {hide: false, x: 2.54, y: -8.89}}
set_symbol_properties FIELDS_FULL  field_placements {Value: {hide: true},
                                                     MPN: {hide: false, x: 0, y: -10.16}}
sha256 cfd94069df07db9e6eae5b2b32cf4196ef17288f091277cec38012a50105b8e8
```

KiCad 10.0.8 on Windows resaves it unchanged but for its line breaks, which it
writes as CRLF there:

```text
kicad-cli sym upgrade --force symbol_fields_kicad10.placed.kicad_sym -o resaved.kicad_sym
diff --strip-trailing-cr symbol_fields_kicad10.placed.kicad_sym resaved.kicad_sym
```

`fiber_optic_kicad7.placed.kicad_sym` is what it writes for this call on
`fiber_optic_kicad7.kicad_sym` (KiCad 7's stock library, see
`fiber_optic_kicad7.README.md`), whose Footprint is hidden by a `hide` inside
its `(effects …)`, the KiCad 6 to 8 form:

```text
set_symbol_properties AFBR-1624Z  field_placements {Footprint: {hide: false},
                                                    Reference: {hide: true, x: -7.62, y: 10.16}}
sha256 864984f945318283bb59cc4f143516b2d5b16a54657bd42628be6412a9818995
```

`fiber_optic_kicad7.placed.resaved.kicad_sym` is KiCad 10.0.8's resave of that
file, as it wrote it (CRLF), in the KiCad 10 format:

```text
kicad-cli sym upgrade --force fiber_optic_kicad7.placed.kicad_sym -o fiber_optic_kicad7.placed.resaved.kicad_sym
sha256 0dd05ca99cbca50b8eb0048d555cbdcc6cd199bdbfd52dd4b74fb47e3c61a653
```

In it KiCad reads Reference as hidden at (-7.62, 10.16) and Footprint as shown.

`field_placements_write_what_kicad_resaves` and
`field_placements_reach_kicad_in_a_kicad7_library` hold the tool to these.
