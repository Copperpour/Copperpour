# A value, a rename and a removal on the fields fixture

`symbol_fields_kicad10.renamed.kicad_sym` is what `set_symbol_properties`
writes for this call on `symbol_fields_kicad10.kicad_sym`:

```text
set_symbol_properties FIELDS_FULL  value "TPS7A0230",
                                   rename_fields {LCSC: "LCSC Part"},
                                   remove_fields [MPN]
```

KiCad 10.0.6 resaves it byte for byte:

```text
kicad-cli sym upgrade --force symbol_fields_kicad10.renamed.kicad_sym -o resaved.kicad_sym
cmp symbol_fields_kicad10.renamed.kicad_sym resaved.kicad_sym
sha256 14b2b786e8338ef377efb7adc7e8043ee2a2046fcbcf8a33cf5dd8ac766fff0b
```

`a_value_a_rename_and_a_removal_write_what_kicad_resaves` holds the tool to it.
