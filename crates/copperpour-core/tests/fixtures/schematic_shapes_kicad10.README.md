# Schematic shapes fixture

`schematic_shapes_kicad10.kicad_sch` is `schematic_texts_kicad10.kicad_sch`
with ten graphic shapes and a wire added, so `add_schematic_shapes` and
`list_schematic_shapes` (#228) can be checked against what KiCad writes.

## Provenance

The shapes and the wire were written by hand, one line each, before the net
label of `schematic_texts_kicad10.kicad_sch`, its root uuid replaced, then
parsed and force-resaved by KiCad 10.0.8 with no project file beside it:

```text
kicad-cli sch upgrade --force schematic_shapes_kicad10.kicad_sch
```

What is committed is that Eeschema serialization, `(version 20260306)`. KiCad
wrote the shapes in its own order and left the rest of the sheet as KiCad
10.0.6 had saved it. It writes the two-point polyline, its graphic line, after
the texts and without a fill.

## Shapes

| uuid ends | Shape | Role |
|---|---|---|
| `…0001` | line `(12.7, 101.6)`–`(177.8, 101.6)` | a divider, default stroke |
| `…0002` | three-point polyline | 0.254 mm, `dash`, colour `194 0 0 1` |
| `…0003` | rectangle `(20.32, 17.78)`–`(101.6, 83.82)` | a frame, `dash_dot` |
| `…0004` | rectangle | 0.1524 mm `solid`, `background` fill |
| `…0005` | rectangle | colour fill `255 230 153 0.5` |
| `…0006` | circle `(88.9, 127)`, radius 6.35 | default stroke, no fill |
| `…0007` | circle `(114.3, 127)`, radius 3.81 | 0.3 mm `dot`, stroke and fill colours |
| `…0008` | arc | listed, not written by the tool |
| `…0009` | closed four-point polyline | colour fill `0 194 0 0.3` |
| `…000a` | Bezier curve | listed, not written by the tool |
| `…00a0` | wire | not a shape |

`Device:Ammeter_AC`'s library body draws polylines, arcs and a circle inside
`lib_symbols`; they are not shapes of the sheet.

KiCad 10.0.8 also loaded `schematic_texts_kicad10.kicad_sch` with the eight
shapes but the arc and the curve added by `add_schematic_shapes`, and its
forced resave kept each written shape's block byte for byte, though it moved
the blocks among the sheet's items.
