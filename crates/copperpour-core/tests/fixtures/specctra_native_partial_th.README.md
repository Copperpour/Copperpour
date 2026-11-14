# Native export with partial-layer through-hole pads (#484)

`specctra_native_partial_th.kicad_pcb` is the KiCad-authored four-layer
fixture (`specctra_four_layers.kicad_pcb`) with existing tracks removed,
through-hole header `J1` (`PinHeader_1x02_P2.54mm_Vertical`, front side)
narrowed to `(F.Cu, In1.Cu)` on pad 1 and `(F.Cu, In2.Cu)` on pad 2, and a
second distinct through-hole header `J2` (`PinHeader_1x02_P2.54mm_Horizontal`)
placed on `B.Cu` (`Flip`) narrowed to `(B.Cu, In2.Cu)` on pad 1 and
`(B.Cu, In1.Cu)` on pad 2 (distinct footprint names avoid KiCad's `<name>::1`
image suffix, which Freerouting's SES omits).

`specctra_native_partial_th.generate.py` authors the board with KiCad 10.0.6’s
native `pcbnew` API without patching serialized design text. In KiCad 10.0.6,
`pcbnew.LoadBoard` resets `thru_hole` pad layer sets back to `*.Cu` on reload,
while live in-memory pads modified with `pad.SetLayerSet(...)` retain their
partial layer sets through both `SaveBoard` and `ExportSpecctraDSN`. The
script therefore calls `ExportSpecctraDSN` directly on the live `board` object
before reload, modeling a live KiCad session where the pad layer sets are
narrowed in memory.

In the KiCad-native DSN, bottom-side `J2` pads are exported in front-image
layer order (`F.Cu, In1.Cu` for pad 1 and `F.Cu, In2.Cu` for pad 2), matching
the reversed physical stack (`B.Cu -> F.Cu`, `In2.Cu -> In1.Cu`, `In1.Cu ->
In2.Cu`). Only the environment-dependent root `(pcb ...)` identifier was
normalized to `board.dsn`. The Rust writer deliberately continues to refuse
partial-layer through-hole pads (`export_rust`).

`specctra_native_partial_th.native-kicad-10.freerouting-2.5.0.ses` was routed
with:

```bash
java -Djava.awt.headless=true -jar /path/to/freerouting-2.5.0.jar \
  -de specctra_native_partial_th.native-kicad-10.dsn \
  -do specctra_native_partial_th.native-kicad-10.freerouting-2.5.0.ses -mp 2
```

Freerouting completed routing with zero unrouted connections and zero
violations, using vias between `In1.Cu`/`In2.Cu` and `F.Cu`/`B.Cu` to connect
the partial-layer pads across disjoint copper sets.
`specctra_native_partial_th.native-applied.kicad_pcb` is KiCad’s own
`pcbnew.ImportSpecctraSES` + `pcbnew.SaveBoard` oracle.

Real KiCad 10.0.6 `kicad-cli pcb drc` on disposable copies retained two
`lib_footprint_issues` warnings from the source `R_0402` footprints before and
after routing, while unconnected items fell from six to zero with zero errors.
Freerouting’s original trailing spaces in the SES are preserved; source/document
whitespace checks exclude `.ses` fixtures.
