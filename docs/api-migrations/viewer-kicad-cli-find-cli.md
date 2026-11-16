### The schematic viewer renders with the server's `kicad-cli` (patch release)

`open_schematic_viewer` now renders with the `kicad-cli` the other kicad-cli
tools run. A bare `kicad_cli` (the default, `kicad-cli`) is looked up on `PATH`,
then in the install paths, where it used to be handed to the OS unresolved. An
empty one is looked up in the install paths, as `get_installation_info` does,
then as `kicad-cli` on `PATH`; it used to search a separate list that lacked
`~/Applications/KiCad` on macOS. An explicit path is still used as given.

On Linux the install paths now include `/snap/kicad/current/usr/bin/kicad-cli`,
which only the viewer used to try, so `get_installation_info` and the kicad-cli
tools find a snap install too. Nothing to change in calls.
