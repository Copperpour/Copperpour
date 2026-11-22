### `get_installation_info` reports cached KiCad prerequisite health (minor release)

The result adds `prerequisite_health` with `source`, `checked_at_unix_ms`, and
observations for the KiCad CLI and IPC. The server records the startup
assessment in the background. Pass `{"refresh_prerequisite_health": true}`
to take a fresh sample with the current server configuration and cache it.

The existing `kicad` block now reports cached CLI evidence. With a blank
`kicad_cli` setting, that block keeps the viewer's installation discovery.
The health finding reports `disabled` for CLI-backed tools that honor the
blank value.
The existing `ipc` block reports an explicit endpoint, a single discovered
candidate, or recent routing evidence from live tools. Multiple automatic
candidates without routing evidence report `ambiguous` and may leave
`configured` false at startup. Live operations keep their endpoint
disambiguation and target checks. For communication and PCB readiness,
read `prerequisite_health.ipc`. Endpoints continue to be redacted.
