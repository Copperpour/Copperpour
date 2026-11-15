### The viewer's Open in KiCAD honours `kicad_binary` (patch release)

**Open in KiCAD** in a viewer started by `open_schematic_viewer` now runs the
KiCad that `launch_kicad_ui` runs: the configured `kicad_binary`, else the GUI
beside the configured `kicad_cli`, else the usual install paths, else `kicad`
on `PATH`. It used to check only a fixed list of install paths and `kicad` on
`PATH`, so a Flatpak wrapper or an `/opt` install set in `kicad_binary` was
ignored.

On Linux, both now also try `/usr/bin/kicad` and `/usr/local/bin/kicad` after
`PATH`, so a server started with a reduced `PATH` still finds a stock install;
`launch_kicad_ui` used to fall back to a bare `kicad` there. Nothing to change
in calls.
