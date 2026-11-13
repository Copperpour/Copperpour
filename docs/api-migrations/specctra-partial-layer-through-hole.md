### Native Specctra export accepts partial-layer through-hole pads (#484, minor release)

`export_specctra_dsn` keeps `native_bridge_mode: "disable"` (and the Rust
exporter's all-copper through-hole requirement) unchanged. When KiCad 10 native
export is selected via `prefer` or `require`, `prepare_export` now accepts
through-hole pads whose declared copper layers form a valid non-empty subset of
the board's copper layers, rather than refusing them before contacting the
native bridge.

For footprints placed on `B.Cu`, physical copper layers are mapped bottom-to-top
into front-image Specctra padstack layers to match KiCad's native DSN export.
Empty, duplicated, or foreign copper layer lists remain refused in every mode,
and Rust export (including `prefer` fallback) still refuses partial-layer
through-hole pads. Existing tool names, arguments, manifest schema versions, and
response keys are unchanged.
