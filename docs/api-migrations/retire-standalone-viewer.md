### Standalone Tauri schematic viewer retired (minor release)

The standalone `schematic-viewer` / `schematic-viewer.exe` command and its
`--kicad-cli` flag are removed. There is no shim or Tauri fallback. Use the
`open_schematic_viewer` MCP tool, which already runs the browser service embedded
in the MCP server. Configure the server’s `kicad_cli` when automatic KiCad
CLI discovery is unsuitable. `open_browser: false` returns the private URL
without launching a browser; opener failure leaves a successfully started
service available. See [the MCP contract](browser-viewer-mcp.md) for response
fields, lifetime, saved-file behavior and headless forwarding.

The earlier plan retained Tauri for one shipped deprecation release. The
[owner explicitly waived that grace period](https://github.com/Copperpour/Copperpour/issues/242#issuecomment-6825001324)
after the MCP default moved to the browser service in #439. This retirement
supersedes the retention text in the earlier phase migration notes.

`crates/schematic-viewer`, its lockfile, frontend source path, platform icons,
Tauri configuration and separate `viewer` CI job are removed. Browser assets
now live in `crates/copperpour-viewer/frontend` and are embedded at build time.
HTTP paths and authentication/sandbox behavior are unchanged. README uses the
shared PCM logo. No GUI runtime is needed by the server; rendering still needs
`kicad-cli`, and viewing needs a browser. KiCad itself retains its own native
runtime requirements.

PCM and release builds package only the MCP server, which includes the viewer.
Remove `--viewer PATH` from `packaging/build-pcm.sh` calls and `-ViewerPath PATH`
from `packaging/build-pcm.ps1` calls; these retired arguments explicitly refuse
rather than silently dropping an expected binary. PowerShell retains its old
parameter slot solely for this refusal, preserving other positional meanings.
Old PCM archives remain readable by the merge/validation tooling. Existing
installs may still contain an old viewer executable; it is no longer launched
by the MCP tool. Update scripts or shortcuts that call it directly.

Tauri-only GTK/WebKit development dependencies and universal viewer builds are
removed from release CI and the Nix development shell. Workspace tests cover the
engine/service; browser execution checks remain separate. The live required
`Schematic viewer` check and Local CI configuration need maintainer coordination
when this PR lands; deleting its workflow does not itself edit branch rules.
