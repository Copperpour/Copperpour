### Shared browser viewer service (minor release; phase two of #242)

> Later superseded by [standalone viewer retirement](retire-standalone-viewer.md):
> the owner waived the planned release grace period and approved immediate removal.
> Statements below about retention describe this earlier phase only.

This phase adds a development browser service in `copperpour-viewer`, accessible
through its Rust API and `browser_viewer` example. It does not switch
`open_schematic_viewer`, deprecate Tauri, remove `schematic-viewer`, or change
that binary's arguments. The MCP default switch and its response migration belong
to phase three; Tauri retirement must wait for the release/deprecation window.

The existing `crates/schematic-viewer/frontend/index.html` source path remains
available; it and the adjacent new `viewer.js` are shared by both adapters.
The installed Tauri binary still embeds its frontend and needs no external HTML
files. There is no source-asset path retirement in this phase.

Browser instances use a launch-bound root, authenticated loopback endpoints and
SSE snapshots. They read saved files through temporary snapshots. Outside-root or
parent-traversing child references and failed snapshot copies are refused in this
path. The legacy Tauri engine retains its existing behavior. See the viewer crate
README for the service, lifecycle, security boundaries and local testing command.
