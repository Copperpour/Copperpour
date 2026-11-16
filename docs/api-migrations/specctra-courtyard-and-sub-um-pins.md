### `export_specctra_dsn` adopts KiCad courtyard polygons and sub-micrometre pins (minor release)

Native Specctra adoption (`native_bridge_mode: "prefer"` or `"require"`)
previously refused KiCad DSNs from ordinary stock footprints because of two
parser/correlator limits ([#98](https://github.com/Copperpour/Copperpour/issues/98)):

- **Courtyard polygons:** KiCad exports a footprint's `fp_rect` courtyard as
  `(outline (polygon ...))`, which the underlying Specctra syntax validator
  rejected. Syntax validation now accepts `(outline (polygon ...))` while
  keeping the native DSN byte-for-byte unchanged.
- **Sub-micrometre pin positions:** Stock footprints such as `R_0805_2012Metric`
  place pads at ±0.9125 mm (±912.5 µm), and 45°-rotated footprints can store
  nanometre-rounded offsets such as ±499.999 µm. Copperpour's DSN model now
  preserves pad offsets at nanometre resolution and compares native pin
  coordinates within one DSN resolution step (0.1 µm).

No tool names, arguments, or manifest schema fields changed. Boards with stock
footprints that previously fell back to Rust under `prefer` or failed under
`require` can now adopt KiCad's native DSN directly.
