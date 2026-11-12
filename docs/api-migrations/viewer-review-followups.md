### `open_schematic_viewer` identifies missing schematics (minor release)

> Later superseded by [standalone viewer retirement](retire-standalone-viewer.md):
> the owner waived the planned release grace period and approved immediate removal.
> Statements below about retention describe this earlier phase only.

A schematic path that cannot resolve because it does not exist now returns
structured `file_not_found` with the requested `path`, instead of a generic
`handler_error`. Other resolution failures retain their existing legacy error text; clients
should not interpret a permission error or symlink loop as a missing file.

Correct the path before retrying. This refusal starts no viewer service or browser.
Existing successful response fields, service ownership, browser-launch behavior,
and the standalone Tauri deprecation window are unchanged.

The shared browser handle adds `request_shutdown()`, a non-consuming shutdown
signal. `is_running()` becomes false when stop is requested; `shutdown().await`
waits for task teardown. The MCP registry replaces stopped services rather than
reusing their credential URLs. No new MCP tool or HTTP stop endpoint is added.
