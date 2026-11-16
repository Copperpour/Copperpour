### `get_installation_info` probes the configured CLI executable (patch release)

For a nonempty `kicad_cli`, `get_installation_info` now uses the same executable
resolution as CLI tools and the schematic viewer. An explicit missing path is
reported in `kicad.cli_path` with `probe_status = "launch_failed"` and a null
version, rather than reporting another installed KiCad. Bare names still use
PATH and installation discovery; an empty value retains installation discovery.

Correct an unusable configured path and restart the server before checking again.
No argument or response-key changes are required.
