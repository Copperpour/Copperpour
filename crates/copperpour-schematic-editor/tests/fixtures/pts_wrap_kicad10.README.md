# `(pts …)` wrapping fixture

`pts_wrap_kicad10.kicad_sch` is a sheet eeschema wrote, with runs of `(xy …)`
points that eeschema keeps on one line and runs that it wraps, so the writer's
packing can be checked byte for byte against KiCad's.

## Provenance

Built through Copperpour's served dispatch (`tools/call`), then force-resaved by
KiCad 10.0.6:

1. `create_project` (`name`: `pts_wrap_kicad10`).
2. `create_symbol` into `Wrap.kicad_sym` in the project: `PtsWrap`,
   `reference_prefix` `X`, one passive pin `1` named `~` at (-5.08, 0), angle 0,
   length 2.54, drawn by `graphics`: four `poly` primitives, stroke 0.254 mm,
   fill `none`, with the points the fixture's four polylines carry.
3. `register_symbol_library` (`scope`: `project`) for `Wrap` on `Wrap.kicad_sym`.
4. `add_schematic_component` for `X1` (`Wrap:PtsWrap`) at (101.6, 101.6).
5. `add_wire` from (50.8, 50.8) to (76.2, 50.8), from (76.2, 50.8) to
   (76.2, 76.2), and from (152.4, 50.8) to (152.4, 76.2).

```text
kicad-cli sch upgrade --force pts_wrap_kicad10.kicad_sch
```

Only the schematic was resaved, alone, and committed. What is committed is that
Eeschema serialization: `(generator "eeschema")`, `(version 20260306)`.

## Cases

eeschema puts a point after a point on the same line while that line is shorter
than 99 characters, a tab counting one; the first point of a run opens a line.
The symbol's polylines are six tabs deep in `lib_symbols`, the wires' points
three.

| Block | Points | eeschema's lines | Case |
|---|---|---|---|
| each wire | 2 | 1 | the common case: a wire's two points on one line |
| polyline from (-1.27, -1.27) | 3 | 1 | a short run |
| polyline from (1.27, 10.16) | 9 | 7 + 2 | the line is 98 characters after the sixth point, so the seventh joins it and the line runs to 113 |
| polyline from (1.27, 2.54) | 9 | 6 + 3 | the line is 99 characters after the sixth point, so the seventh opens a new line |
| polyline from (1.27, 1.27) | 20 | 7 + 7 + 6 | a run over three lines |

`tests/pts_round_trip.rs` loads and saves the file and gets it back byte for
byte, and adds one wire and gets back the file plus that wire's lines.
