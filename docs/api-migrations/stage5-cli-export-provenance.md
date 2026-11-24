### Saved-snapshot and mixed-source provenance on Stage 5 CLI, render, and export tools (minor release)

`export_gerber`, `export_pdf`, `export_svg`, `export_3d`, `export_netlist`, `export_position_file`, `export_dxf`, `export_gencad`, `export_ipc2581`, `export_odb`, `get_board_2d_view`, `export_manufacturing_package`, and `generate_jlcpcb_order_link` now report observed `sources` and `source_evidence` fields disclosing their saved-snapshot or mixed-source boundaries ([#58](https://github.com/Copperpour/Copperpour/issues/58)):

- **No selector or refusal change:** CLI export and render tools continue to run `kicad-cli` against saved files on disk without implicit KiCad saves or IPC queries; `export_manufacturing_package` keeps its existing `board_source` and `project_settings_source` fields and `generate_jlcpcb_order_link` keeps its existing `board_source` field alongside the canonical `sources` and `source_evidence` keys.
- **`get_board_2d_view` metadata text block:** `get_board_2d_view` now returns a JSON text content block (`board`, `width`, `height`, `sources`, `source_evidence`) followed by the existing PNG image content block, the form `render_schematic_png` (`inline: true`) and `get_placement_overview` use. Read the image from the second block.
- **Additive per-domain `sources`:**
  - `export_gerber`: `{ "board": "saved_board", "layers": "argument" | "saved_board", "gerbers": "kicad_cli" }` (plus `"drill": "kicad_cli"` when `drill_file` is true)
  - `export_pdf`: `{ "board": "saved_board", "layers": "argument" | "saved_board", "pdf": "kicad_cli" }`
  - `export_svg`: `{ "board": "saved_board", "layers": "argument" | "saved_board", "svg": "kicad_cli" }`
  - `export_3d`: `{ "board": "saved_board", "model_3d": "kicad_cli" }`
  - `export_netlist`: `{ "board": "saved_board", "netlist": "kicad_cli" }` for IPC-D-356, or `{ "schematic": "saved_schematic", "netlist": "kicad_cli" }` for schematic netlist formats
  - `export_position_file`: `{ "board": "saved_board", "positions": "kicad_cli" }`
  - `export_dxf`: `{ "board": "saved_board", "dxf": "kicad_cli" }`
  - `export_gencad`: `{ "board": "saved_board", "gencad": "kicad_cli" }`
  - `export_ipc2581`: `{ "board": "saved_board", "ipc2581": "kicad_cli" }`
  - `export_odb`: `{ "board": "saved_board", "odb": "kicad_cli" }`
  - `get_board_2d_view`: `{ "board": "saved_board", "render": "kicad_cli" }`
  - `export_manufacturing_package`: `{ "board": "ipc" | "saved_board", "project_settings": "project_file" | "unavailable", "gerber_layers": "argument" | "ipc" | "saved_board", "gerbers": "kicad_cli" | "unavailable", "drill": "kicad_cli" | "unavailable" }` (plus `"positions": "derived" | "kicad_cli" | "unavailable"`, `"schematic": "saved_schematic" | "unavailable"`, and `"bom": "kicad_cli" | "unavailable"` when `include_assembly` is true)
  - `generate_jlcpcb_order_link`: `{ "board": "ipc" | "saved_board", "project_settings": "project_file" | "unavailable", "gerbers": "kicad_cli", "drill": "kicad_cli", "drc": "kicad_cli" | "unavailable" }` (plus `"schematic": "saved_schematic"`, `"bom": "kicad_cli"`, `"positions": "derived"` when `include_assembly` is true)
