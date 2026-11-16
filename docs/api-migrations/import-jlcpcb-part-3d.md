### `import_jlcpcb_part` attaches and transforms 3D models on `include_3d: true` (patch release)

When `include_3d: true` is passed to `import_jlcpcb_part`:

- It resolves the 3D model UUID from the EasyEDA `SVGNODE` (`outline3D`) shape rather than `head.uuid_3d`, downloads STEP when available or falls back to EasyEDA's Wavefront OBJ converted to KiCad-scaled VRML (`.wrl`), computes KiCad `(offset (xyz ...))` and `(rotate (xyz ...))`, and attaches `(model "${KIPRJMOD}/3dmodels/..." ...)` to the generated `.kicad_mod` file.
- The tool response now includes a structured `model_3d` object (`status`: `"attached"`, `"not_available"`, or `"error"`, with `uuid`, `format`, `kicad_path`, `path`, `offset_mm`, `rotation_degrees`, or `reason` as applicable) instead of silently discarding download errors.
- Passing a non-boolean `include_3d` value is now refused with `invalid_argument` rather than treated as `false`.
