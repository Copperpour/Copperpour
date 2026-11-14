### `launch_kicad_ui` takes `display`; KiCad health messages name dialogs and `enable_server` (minor release)

`launch_kicad_ui` accepts an optional `display`, a local X display such as
`":0"` or `":1.0"`. KiCad starts with that `DISPLAY`, `GDK_BACKEND=x11`, and
without `WAYLAND_DISPLAY`. Any other value, a remote `host:N` display among
them, is refused as `invalid_argument`, and so is any value on Windows and
macOS. When KiCad exits during startup and the server has neither
`DISPLAY` nor `WAYLAND_DISPLAY`, the error adds a line saying so, and
`launch_kicad_ui` adds that `display` fixes it. The browser viewer's
`POST /open-in-kicad` `503` body adds the same line, and so does
`open_schematic_viewer`'s `browser_launch_error` when the opener failed (not
when it timed out).

When KiCad answers `AS_NOT_READY`, `ipc_failure` (from `check_kicad_ui` and
`open_project`) carries a `note` saying it is still starting or held by a
modal dialog. `launch_kicad_ui` returns that `note` in place of "IPC not yet
responsive" when its wait ends that way. The `no_listener` message names
`api.enable_server` in `kicad_common.json`.

From an SSH session, pass `display`. Treat a lasting `AS_NOT_READY` as a dialog
to close, not a start to wait for.
