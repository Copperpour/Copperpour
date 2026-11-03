### The live-KiCad test variables use `COPPERPOUR_*` names (patch release)

Every `KONNECT_<X>` variable that points an ignored live-KiCad or e2e test at a
board, schematic, library, reference, `kicad-cli` or output directory is now
`COPPERPOUR_<X>`: `COPPERPOUR_LIVE_KICAD_BOARD`, `COPPERPOUR_E2E_KEEP_DIR`,
`COPPERPOUR_CAPTURE_IPC_FIXTURE` and so on. The `KONNECT_<X>` name is still
read when the new one is unset, for one release. An empty value now counts as
unset, so set `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` rather than to nothing. Only developers running those
tests see this; no tool's arguments or responses change
([#250](https://github.com/Copperpour/Copperpour/issues/250)).
