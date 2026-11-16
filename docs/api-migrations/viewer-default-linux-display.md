### Schematic viewer defaults to the local X display on Linux (patch release)

**Open in KiCad** now starts KiCad with `DISPLAY=:0` on Linux when both
`DISPLAY` and `WAYLAND_DISPLAY` are unset or empty. Existing display settings,
`XAUTHORITY`, and runtime variables are preserved; the fallback affects only the
KiCad child process. Other platforms and `launch_kicad_ui` are unchanged.

If the fallback display is unavailable or inaccessible, the viewer still reports
the startup failure, identifies the attempted fallback, and recommends configuring
Copperpour with the desktop's display and `XAUTHORITY`. To use another display,
set the server's display environment before starting Copperpour. The button label
and tooltip now use the official KiCad spelling.
