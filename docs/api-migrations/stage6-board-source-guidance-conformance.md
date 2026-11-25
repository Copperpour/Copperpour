### Per-domain saved-schematic provenance on manufacturing packages and design review (minor release)

`export_manufacturing_package`, `generate_jlcpcb_order_link`, and `run_design_review` now attach an additive `source_evidence.schematic` object whenever they read a saved `.kicad_sch` file ([#58](https://github.com/Copperpour/Copperpour/issues/58)):

- **Shape:** `source_evidence.schematic` carries `schematic_state: "saved_schematic"`, `excludes_unsaved_editor_state: true`, `reason` (`"saved_snapshot_only"`, `"saved_snapshot_only_with_editor_lock"`, or `"saved_snapshot_only_with_uninspectable_lock"`), and `detail`.
- **Manufacturing packages (`export_manufacturing_package`, `generate_jlcpcb_order_link`):** Attached whenever `include_assembly` is true and a `schematic` path was supplied, including on the live JLCPCB IPC snapshot path where the top-level `source_evidence.board_state` is `"ipc"`. `generate_jlcpcb_order_link` carries forward the schematic evidence observed when the package exported the BOM. Omitted for fabrication-only exports or when no `schematic` was supplied.
- **Design review (`run_design_review`):** Attached on both board-supplied and schematic-only calls so a KiCad editor lock on the saved schematic is disclosed alongside the board evidence.
- **Backwards compatibility:** Existing top-level `source_evidence` fields (`board_state`, `excludes_unsaved_editor_state`, `reason`, `detail`), `sources`, and error kinds are unchanged.
