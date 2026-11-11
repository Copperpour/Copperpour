### `open_schematic_viewer` uses the browser service; standalone Tauri viewer deprecated (minor release)

The tool name and required `schematic` argument remain. An optional `open_browser`
boolean defaults to true; false starts/reuses the service without launching a
browser. Malformed present values, including null, are refused. The service uses
the server's configured `kicad-cli` or normal discovery and reads saved files.
It applies the hierarchy/snapshot restrictions documented in the viewer README.

Successful responses retain `launched`, `viewer`, `schematic` and `note`.
`launched: true` now means the loopback browser service started or was reused;
`viewer` is `"browser"` rather than a standalone executable path. `schematic`
is the canonical root path. New fields are `url`, `reused`,
`browser_launch_requested`, `browser_launch_succeeded` and `browser_launch_error`.
The browser fields report the OS opener's observed exit, not proof that a browser
window became visible. A failed/timed-out opener leaves the service usable and
returns its URL with a diagnostic; use it manually rather than starting duplicates.
With `open_browser: false`, both launch booleans are false and the error is null.
Root resolution, render or bind failure returns a tool error without a success URL.

The URL includes a fragment credential; treat it as a private local capability.
One viewer is reused per canonical root in a `ToolContext`. It lives until that
MCP session ends, including when its browser tab is closed. Closing the session
signals shutdown of listener, watcher and render processes. A later session gets
a new URL; old URLs cannot reconnect to it. For headless/SSH use, forward the
printed port to the same local port, preserving the printed `127.0.0.1` origin;
Host/Origin validation rejects other ports or hostnames. The service makes no
outbound network requests and remains available in offline builds.

The documented `schematic-viewer` binary and `--kicad-cli` flag remain available
but are deprecated. Keep its packaging and CI through one shipped release
containing this notice. A later removal PR must wait until that release has
shipped; this change does not remove Tauri or provide an automatic fallback to it.
