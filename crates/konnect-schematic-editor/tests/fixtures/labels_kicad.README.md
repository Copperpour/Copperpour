# Label fixtures

Two minimal sheets whose label blocks are copied byte for byte, CRLF line endings
included, from schematics that KiCad itself wrote. They pin how KiCad serializes
`(fields_autoplaced …)` on labels, which differs by file format.

| Fixture | Source (KiCad 10.0.5 demos) | Source SHA-256 | Format | Label blocks |
|---|---|---|---|---|
| `labels_kicad7.kicad_sch` | `vme-wren/front-panel-io.kicad_sch` | `89b49f68e4a49b771476803b1e1b34767768d136ff26b1a1d90723ecf19b48ea` | `20230819` (KiCad 7) | `label "TRIGOUT6"` and `hierarchical_label "TRIGIO_IN[7..0]"`, both carrying the bare `(fields_autoplaced)` |
| `labels_kicad9.kicad_sch` | `tiny_tapeout/tinytapeout-demo.kicad_sch` | `e90de3e79dce87a0f51e607386b3d502aa63649c263989e7f6e81c149aaf93e1` | `20250114` (KiCad 9) | `global_label "out6"` with `(fields_autoplaced yes)` and its `Intersheetrefs` property, and `label "CC2"`, which has no token |

Each sheet is the source file's own header up to `lib_symbols`, an empty
`(lib_symbols)`, the selected blocks verbatim, and a closing paren. Nothing inside
a label block was edited. `kicad-cli sch erc` 10.0.5 loads both sheets (exit 0).
Its only findings are the expected dangling-label warnings.

The blocks were selected by UUID. Across every schematic in those demos, KiCad
writes the token on 320 labels, always directly after `at`. KiCad 7 files carry
the bare form, and later formats carry `yes`.
