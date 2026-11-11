# Shared schematic viewer

The crate contains the GUI-independent viewer engine and the phase-two browser
service. `schematic-viewer` remains the supported Tauri application;
`open_schematic_viewer` still launches it. Connecting that MCP tool to the
browser service is phase three of #242.

For local browser testing:

```bash
cargo run -p copperpour-viewer --example browser_viewer -- /absolute/root.kicad_sch /path/to/kicad-cli
```

Open the printed loopback URL, including its fragment credential. Treat it as a
local capability: do not publish it. Ctrl-C shuts down the service. It can show
changes made by KiCad or an MCP agent to the saved schematic, without changing
how the agent connects to Copperpour. It does not show unsaved editor changes.

The browser and Tauri adapters share `crates/schematic-viewer/frontend/index.html` and its adjacent
`viewer.js`. The existing asset path is retained during the Tauri compatibility
period; the browser module embeds the same source assets.
Tauri uses invoke/events; browser mode uses authenticated HTTP and SSE. Browser
instances are bound to their launch root, so browser mode has no file-drop/open
control. Pan, zoom, theme, sheet selection and manual refresh are shared.

## Service contract

`BrowserViewer::start` takes the root path, an explicit CLI executable and a
positive per-render timeout. It binds IPv4 loopback on an ephemeral port and
returns a handle and credential URL; it does not launch a browser. Drop signals
shutdown; `shutdown().await` also waits for the render worker and HTTP service.
On viewer shutdown, in-flight CLI processes are killed and reaped. Opening KiCad is
an explicit authenticated user action; that GUI is allowed to outlive the viewer.
Its response establishes process spawn, not that an editor became ready.

Only sheets discovered in the launch hierarchy and present in the SVG cache can
be fetched. Unknown sheets return 404, and a known sheet whose rendering failed
returns 503 rather than an old SVG. Updates and manual refreshes are serialized;
unchanged sheets retain their cache on watcher updates. SSE sends complete current
snapshots on connection/reconnection, including revision, rendering status and
render/watch errors. It is resynchronization, not a replay log.

The browser path currently requires child references to be relative, without
parent traversal, and to resolve inside the root directory. Missing files,
symlinks outside that directory, cycles and hierarchies beyond the engine's
discovery depth are refused. It refuses snapshot copy failure instead of using
the legacy engine's live-file fallback. These stricter browser restrictions do
not alter the legacy Tauri engine API.

All service requests validate Host; supplied Origin headers must match that
loopback origin. POSTs also require Origin. Data/control endpoints need the
launch credential. The page and script are public but contain no credential or
design data. Fetch requests use a bearer header; SSE uses its credential query
because EventSource cannot set headers. Responses disable caching and referrers.
For SSH forwarding, forward the server port to the same local port and use the
printed 127.0.0.1 origin; arbitrary Host aliases/ports are rejected.

Browser SVGs live in a sandboxed iframe with `allow-same-origin` for parent-side
SVG sizing, **without `allow-scripts`**. The parent page has a restrictive CSP and
external script, and the frame has a separate resource-restricting CSP. SVGs are
XML-validated; KiCad's standard DOCTYPE is accepted without an external entity
resolver. Validation is not sanitization: the sandbox is the script isolation
boundary. Direct SVG responses also carry a sandbox CSP. Do not replace this
with privileged-page `innerHTML` injection. Tauri's existing rendering path is
retained during the transition.

## Validation

Run `cargo test -p copperpour-viewer --locked` and
`cargo clippy -p copperpour-viewer --locked --all-targets -- -D warnings`.
The Unix browser tests exercise real loopback HTTP/SSE, watch invalidation,
refresh ordering, authentication, origin/Host checks, allowlisting, snapshot and
hierarchy refusals, timeout, invalid SVG, and renderer shutdown. Linux additionally
checks that an in-flight renderer PID was reaped. The existing engine tests remain.

The Tauri application is outside the workspace and still needs its own check/test
commands. Native renderer smoke tests are separate from browser execution tests:
HTTP headers and Rust tests alone do not prove that a browser enforced the CSP or
sandbox. Browser visual/security execution and Windows/macOS observations must be
recorded before making the browser path the MCP default.
