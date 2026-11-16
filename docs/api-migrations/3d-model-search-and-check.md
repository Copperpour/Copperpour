### New `search_3d_models` and `check_3d_models` tools (minor release)

No existing tool names, arguments, or responses change. Two read-only tools
complete discovery and pre-export checks around 3D models:

- `search_3d_models` (`library` toolset) searches KiCad's bundled `3dmodels`
  directories and/or `<project_dir>/3dmodels` for `.step`, `.stp`, `.stpz`,
  `.wrl`, `.wrz`, and `.obj` files, returning `${KICAD*_3DMODEL_DIR}/...` or
  `${KIPRJMOD}/3dmodels/...` paths usable with `set_footprint_models` and
  `set_placed_footprint_models`.
- `check_3d_models` (`pcb_export` toolset) audits a `.kicad_pcb` file on disk
  for placed footprints with missing `(model ...)` entries or unresolvable model
  files before running `export_3d`, skipping `dnp` and `board_only` footprints
  unless `include_dnp` or `include_board_only` is set.
