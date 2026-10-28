# reference_prefix_kicad10 fixtures

Two stock KiCad 10.0.6 library symbols, for tests of the reference a placement
writes when the caller passes none. `stock_reference_prefix_libraries()` in
`src/tools/mod.rs` registers them in a project `sym-lib-table`.

- `Device.kicad_sym` — `Device:R`, library `Reference` `R`.
- `power.kicad_sym` — `power:PWR_FLAG`, library `Reference` `#FLG`. It is
  `(power global)` with `(in_bom yes)` and `(on_board yes)`, so the `#` prefix
  is the only thing keeping it off the board.

Each file is the library's own header plus the one `(symbol …)` block, cut
verbatim from `/usr/share/kicad/symbols/{Device,power}.kicad_sym` (KiCad
10.0.6, Fedora). Resaving with

```
kicad-cli sym upgrade --force <file> -o <out>
```

gives byte-identical output, so the bytes are KiCad's.

## Oracle

| lib_id | library `Reference` | placed, no `reference` | after `annotate_schematic` |
|---|---|---|---|
| `Device:R` | `R` | `R?` | `R1` |
| `power:PWR_FLAG` | `#FLG` | `#FLG?` | `#FLG01` |

The prefix column is the library itself. `kicad-cli sch export netlist` on a
sheet built through Copperpour this way (`create_project`,
`add_schematic_component` twice, `annotate_schematic`) lists one component,
`R1`: the `#FLG01` flag stays off the netlist, as KiCad treats `#` references.

Byte evidence: this directory is `-text` in `.gitattributes`; never
eol-normalise these files.
