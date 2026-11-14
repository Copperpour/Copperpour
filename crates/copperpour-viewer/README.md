# Shared schematic viewer

The crate contains the GUI-independent viewer engine and browser service.
`open_schematic_viewer` uses this service by default; `ToolContext` owns and reuses
an instance per canonical root until the MCP server context ends (stdio disconnect
or HTTP server shutdown). Set `open_browser: false`
to obtain the private URL without launching a browser. The standalone Tauri
command has been [retired](../../docs/api-migrations/retire-standalone-viewer.md).

For local browser testing:

```bash
cargo run -p copperpour-viewer --example browser_viewer -- /absolute/root.kicad_sch /path/to/kicad-cli
```

Open the printed loopback URL, including its fragment credential. Treat it as a
local capability: do not publish it. Ctrl-C shuts down the service. It can show
changes made by KiCad or an MCP agent to the saved schematic, without changing
how the agent connects to Copperpour. It does not show unsaved editor changes.

The browser module embeds `frontend/index.html` and `frontend/viewer.js` from
this crate. It uses authenticated HTTP and SSE. Instances are bound to their
launch root, so there is no file-drop/open control. Pan, zoom, theme, sheet
selection, manual refresh and explicit KiCad launch are available.

## Service contract

`BrowserViewer::start` takes the root path, an explicit CLI executable and a
positive per-render timeout. It binds IPv4 loopback on an ephemeral port and
returns a handle and credential URL; it does not launch a browser. Drop signals
shutdown; `request_shutdown()` signals stop while retaining the handle, and
`shutdown().await` also waits for the render worker and HTTP service.
Accepted HTTP/1 connections are owned tasks, with one second to drain before
forced closure, so a slow response cannot outlive the viewer. An in-progress
response may be interrupted when its owner shuts down.
Connection-aborted/reset/refused accept errors are retried immediately. Other
accept errors (including file-descriptor pressure) are logged and retried after
one second; shutdown interrupts that backoff. Accepted connections remain owned
and retain the same bounded drain behavior. Retries have no consecutive-failure
cap: task liveness is not proof that the listener can currently accept connections.
A permanent listener failure can therefore keep its URL registered while it logs
and retries. If it does not recover, restart the owning MCP server/session to
release the service and obtain a fresh URL; closing a browser tab does not stop it.

Startup renders each unchanged sheet once. Source modification times and lengths
are recorded before discovery reads, then checked after watches are installed.
Only changed sources and newly discovered sheets need catch-up rendering; a
changed sibling project invalidates all sheets. Unavailable revision metadata
conservatively requests catch-up. An unchanged startup stays at revision 1 until
an actual update or explicit refresh; revision is an opaque change marker.

On viewer shutdown, in-flight CLI processes are killed and reaped. Opening KiCad is
an explicit authenticated user action; that GUI is allowed to outlive the viewer.
It reports success once KiCad has stayed up for a second, not that an editor became
ready; a KiCad that exits sooner, such as one with no display, is a 503 quoting its
exit status and the end of its stderr, and a second request during that second is a
409.

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
the engine’s live-file fallback.

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
with privileged-page `innerHTML` injection.

## Validation

Run `cargo test -p copperpour-viewer --locked` and
`cargo clippy -p copperpour-viewer --locked --all-targets -- -D warnings`.
The Unix browser tests exercise real loopback HTTP/SSE, watch invalidation,
refresh ordering, authentication, origin/Host checks, allowlisting, snapshot and
hierarchy refusals, timeout, invalid SVG, and renderer shutdown. Linux additionally
checks that an in-flight renderer PID was reaped. The existing engine tests remain.

Native renderer smoke tests are separate from browser execution tests:
HTTP headers and Rust tests alone do not prove that a browser enforced the CSP or
sandbox. Linux interactive browser feedback is available. Headless Chromium checks exercised the displayed SVG, theme/zoom/fit/refresh
and blocked script/event-handler payloads, with a positive payload control in a
policy-free document. Windows/macOS runtime observations remain separate platform
validation.
