### Schematic viewer's Open in KiCAD reports a KiCad that exits at startup (patch release)

`POST /open-in-kicad` on the browser viewer started by `open_schematic_viewer`
now waits up to a second for KiCad to exit, as `launch_kicad_ui` does. A KiCad
that exits in that time, such as one started with no `DISPLAY`, returns `503`
with its exit status and the end of its stderr, where it used to return
`200 {"spawned":true}`. A second request while one launch is being watched
returns `409`. A KiCad that stays up still returns `200 {"spawned":true}`, now
about a second later. KiCad no longer inherits the server's stdin and stdout.

The page shows the error in its status bar. Fix the cause it names, for example
the server's display, and click again.
