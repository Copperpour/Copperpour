# One board saved at two page settings (issue #257)

Both files are **KiCad's own saves** of one small board, made by the PCB
Editor of KiCad 10.0.8 on macOS:

| File | Page | `(paper …)` line |
|---|---|---|
| `issue_257_page_A4.kicad_pcb` | A4, landscape | `(paper "A4")` |
| `issue_257_page_A3_portrait.kicad_pcb` | A3, portrait | `(paper "A3" portrait)` |

The board is a four-layer scratch board: four `R_0603_1608Metric` footprints,
two segments, two arcs and two blind vias, with no title block.

## Provenance

1. The board was saved at A4 landscape by the PCB Editor: that save is
   `issue_257_page_A4.kicad_pcb`.
2. A copy was opened in the PCB Editor, File > Page Settings set the paper to
   A3 and the orientation to Portrait, and File > Save saved it (2026-11-02):
   that save is `issue_257_page_A3_portrait.kicad_pcb`.

The two files differ in line 9 alone, the `(paper …)` line, which KiCad writes
directly after `(general …)`. `diff` of the two:

```text
9c9
< 	(paper "A4")
---
> 	(paper "A3" portrait)
```

## Derived in the tests

KiCad 10 always writes a `(paper …)` token, so no KiCad save lacks one. Two
inputs are therefore derived in `board_page_tests.rs` from the A4 save, not
committed, and labelled there:

- the A4 save with its `(paper "A4")` line removed, for a board with no token,
  which KiCad reads as A4;
- the A4 save with that line written `(page A4)`, as KiCad 5 named the node.

## What the tests rely on

`set_board_page` on the A4 save with A3 portrait gives the A3-portrait save
byte for byte, and A4 landscape on the A3-portrait save gives the A4 save. On
the derived board without a token, A4 gives the A4 save back.

## sha256

- `issue_257_page_A4.kicad_pcb`:
  `3c55e18cc570ae2c632559330999ad37d461ba2101ffccf5bfb722d4ddd96238`
- `issue_257_page_A3_portrait.kicad_pcb`:
  `0a5311317966041a9522e7fa2fef35fc79af1e6e9fcf8312756ff5cdbf81f171`

## Tests using these fixtures

`crates/copperpour-core/src/tools/board_page_tests.rs`.
