### `check_schematic_overlaps` moves to `sch_readability` (minor release)

Load `sch_readability` instead of `sch_analysis` before calling
`check_schematic_overlaps` directly. If the toolset is unloaded, a direct call returns
`toolset_not_loaded` naming `sch_readability`. The tool name, arguments and results are
unchanged. Read-only calls through `call_tool` still work without loading the toolset.
