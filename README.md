<a name="top"></a>

<div align="center">

<img src="resources/images/KiCAD-MCP-Server-rust.svg" alt="KiCAD-MCP-Server Logo" height="240" />


# Copperpour *BETA Release

</div>

**AI-assisted PCB design for KiCAD 10.** Copperpour is a native KiCAD plugin — a single
Rust binary — that lets Claude and other AI assistants design schematics and PCBs
through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP).

**241 tools across 21 on-demand toolsets.** Schematic capture, PCB layout and
routing, ERC/DRC, design-review audits, JLCPCB part search, reference
circuits, and a full manufacturing export pipeline — with bundled skills and agents
that teach Claude KiCAD conventions out of the box.

> **Status: beta.** The core toolchain is tested and working, but this is a young
> release and it wants real-world mileage and review. Issues and PRs are welcome —
> see [CONTRIBUTING.md](CONTRIBUTING.md) and the
> [naming conventions](docs/NAMING_CONVENTIONS.md).
>
> **Formerly Konnect and LibreKonnect.** Copperpour is the community-maintained
> AGPL-3.0 continuation of [`mixelpixx/Konnect`](https://github.com/mixelpixx/Konnect),
> which went closed source in October 2026. It began as LibreKonnect, whose `v0.14.0`
> integrated 60 open community pull requests and fixes on top of Konnect `v0.13.0` —
> see [docs/UPSTREAM_BACKLOG.md](docs/UPSTREAM_BACKLOG.md). Upgrading? See
> [Migrating from Konnect](#migrating-from-konnect-or-librekonnect).

## Why Copperpour exists

Copperpour continues Konnect, the successor to [KiCAD-MCP-Server](https://github.com/mixelpixx/KiCAD-MCP-Server),
a Python/TypeScript project that proved AI-driven PCB design works — and, in the
process, showed exactly where that architecture runs out of road. Konnect was built
to fix those specific problems:

**The call path was too long.** In the original server, a single tool call travels
through TypeScript, schema validation, a spawned Python subprocess, JSON over
stdin/stdout, a command router, and finally SWIG-generated C++ proxy objects before
anything touches your board. That's four language and serialization boundaries, each
with its own failure modes — subprocess lifecycle management, stdout parsing that
filters out warnings KiCAD leaks into the stream, chunked-JSON reassembly. In
Copperpour, a tool call is a function call. One process, one language, no plumbing.

**The dependency surface was enormous.** Running the original means carrying Node.js
and its npm tree, Python and its pip packages, wxPython, kicad-skip, and KiCAD's
SWIG bindings — two package ecosystems plus a binding layer, every one of them a
moving target that can break an install. Copperpour is a single static binary —
20–25 MB depending on platform, a ~9 MB download. There is nothing to install
alongside it and nothing to version-match.

**SWIG is a dead end.** The original's PCB backend depends on KiCAD's SWIG Python
bindings, which KiCAD is deprecating in favor of its IPC API. SWIG also carried
real operational scars: a zone-fill call that can segfault the backend, proxy-object
comparison bugs, and a fallback path that can silently swap backends mid-session.
Copperpour talks to KiCAD 10 through the official IPC API (protobuf over NNG) — the
interface KiCAD is investing in — with real-time board edits that integrate with
KiCAD's own undo/redo.

**Schematic edits should not corrupt files.** Copperpour edits `.kicad_sch` files
through its own S-expression engine with atomic writes (write, fsync, rename), UUID
preservation, and round-trip tests — no third-party schematic library with known
gaps, no text-manipulation workarounds.

**Context economy is a feature.** Exposing all 241 tools to an LLM costs roughly 23K
tokens of context on every listing. Copperpour's router loads a starter kit (~2K
tokens) and lets the model pull in toolsets on demand — plus built-in observability
(`get_recent_calls`, `server_stats`, JSONL call logs) so the model can diagnose its
own tool failures.

The result is smaller, faster to install, aligned with where KiCAD is going, and
built for production use rather than experimentation. The original project remains
open, maintained, and useful — see [the comparison below](#relationship-to-kicad-mcp-server).

## What it does

Instead of describing changes and applying them by hand, the AI works your project
directly:

- **Place and wire schematic components** — add resistors, ICs, connectors; wire them
  together by pin name
- **Lay out the PCB** — place, move, rotate, and route footprints in real time via
  KiCAD's IPC API, with full undo/redo integration
- **Run design checks** — ERC, DRC, connectivity validation, decoupling audits,
  power-rail review, BOM health checks
- **Export production files** — Gerbers, drill, BOM, pick-and-place, 3D models, PDF
- **Search JLCPCB parts** — find in-stock components in a local 2.5M-part catalog and
  suggest alternatives
- **Start from reference circuits** — USB-C, LDO, buck converter, STM32, I2C, LED
  templates with verified component values
- **Watch it happen** — a live schematic viewer auto-refreshes as the AI edits

The full tool catalog is documented in [tool-directory.md](tool-directory.md).

## How it works

| Layer | Mechanism |
|-------|-----------|
| Schematic editing | Direct `.kicad_sch` S-expression editing with atomic writes (no KiCAD required) |
| PCB editing | KiCad 10 IPC API (NNG + protobuf) — real-time and undo-aware; single-footprint placement has a safe headless fallback |
| Specctra routing | Revision-bound Rust DSN export, local Freerouting MCP routing, and strict one-transaction SES import; an authenticated KiCad 10 native-export bridge is explicit opt-in |
| Exports & checks | `kicad-cli` subprocess (Gerber, PDF, ERC, DRC, …) |
| Transport | MCP JSON-RPC over stdio (default), or Streamable HTTP (`transport = "http"` / `"both"`) |

Explore the request path and safety boundaries in the
[interactive architecture diagrams](docs/ARCHITECTURE_DIAGRAMS.md). They are
published from a standalone documentation repository and add no dependencies to
Copperpour.

## Installation

### From the KiCAD Plugin Manager (recommended)

1. Download the package for your OS from [Releases](https://github.com/Copperpour/Copperpour/releases):
   `copperpour-pcm-v<version>-windows.zip`, `-macos.zip`, or `-linux.zip`. Each
   bundles that platform's server binary — the macOS package is a universal
   build, so one download covers Apple Silicon and Intel. (The `copperpour-pcm-*`
   assets are the KiCAD plugin packages; the other archives are standalone
   server binaries.)
2. Open KiCAD 10 → **Plugin and Content Manager**
3. Click **Install from File** and select the zip
4. Restart KiCAD

Verify: open the **PCB Editor** → **Tools → External Plugins** → you should see
**Copperpour**.

For KiCad 10, the Copperpour settings dialog also offers an optional **native
Specctra bridge**. When enabled, `export_specctra_dsn` can ask the active PCB
Editor to generate its native DSN while Copperpour still binds the export to the
exact IPC snapshot and creates the strict reverse manifest used during SES
import. The bridge is local-only, authenticated, disabled by default, and not
the KiCad 11 integration path. Copperpour uses its Rust exporter by default;
`native_bridge_mode: "prefer"` enables fallback to Rust when the bridge is
unavailable, while `"require"` refuses instead.

For end-to-end autorouting, run `check_freerouting`, then
`export_specctra_dsn` → `route_specctra_dsn` →
`plan_specctra_ses_import` / `apply_specctra_ses`. Readiness reports engine
discovery, native-MCP compatibility, and complete bridge availability
separately. The owned Java child is loopback-only and is reaped on success,
failure, timeout, or cancellation. The first supported profile preserves fixed
straight tracks and through vias; unlocked routing, arcs, zones, and unsupported
geometry are rejected before mutation.

### Build from source

```bash
# protoc is required (protobuf code generation), and cmake (the nng crate
# compiles the NNG C library with it).
# Windows: choco install protoc cmake
# macOS:   brew install protobuf cmake
# Linux:   apt install protobuf-compiler cmake
cargo build --release -p konnect   # builds target/release/copperpour
```

### Install guidance for your AI client

Copperpour bundles shared KiCad skills for Claude and Codex. Select the client when
installing, checking, or removing that guidance:

```bash
# Existing behavior remains the default: Claude skills, agents, and hooks
copperpour init

# Codex installs only the shared skills under ~/.agents/skills
copperpour init --client codex
copperpour status --client codex
copperpour uninstall --client codex
```

MCP server startup never installs or restores guidance. Run `copperpour init`
explicitly when you want those files installed; after `copperpour uninstall`,
starting the server leaves them removed.

Guidance written by an older `copperpour init` is not updated by upgrading the
binary. `copperpour status` compares every installed skill, agent, and hook with
the bundle in the binary you run and marks each `current`, `different`, or
`missing`. The server makes the same check once for each installed version:
the first start after `copperpour init` compares the files, records
the result in `~/.copperpour`, and adds a one-line notice to its `initialize`
instructions if guidance is out of sync. Later starts reuse that record, so
guidance you chose to keep is not reported again. `get_installation_info`
shows the result. Re-run `copperpour init` to update it.
It overwrites differing files, so keep a copy of any file you edited. `--client` remains accepted in server
commands for compatibility. For example, register a standalone binary with the
Codex CLI using:

```bash
codex mcp add copperpour -- /path/to/copperpour --client codex
```

Claude remains the default when `--client` is omitted. The installer tracks the
two clients independently, and a Codex install does not create or modify
`~/.claude`.

`--help` on any subcommand prints that subcommand's usage and writes nothing —
`copperpour init --help` describes the installer rather than running it. An
argument Copperpour does not recognise is an error rather than being ignored, so a
typo such as `--cleint codex` stops instead of quietly installing for the
default client.

To verify which Copperpour process an MCP client is actually using, call the
always-visible `get_installation_info` tool. It reports the serving build,
executable path, verified installation source when one can be proven, KiCad
CLI and IPC detection, and restart guidance. A missing build commit or an
`unknown` installation source means the available evidence was insufficient;
it is not silently guessed from a directory name.

### macOS

The [Releases](https://github.com/Copperpour/Copperpour/releases) page ships
standalone server binaries for both Apple Silicon (`aarch64-apple-darwin`) and
Intel (`x86_64-apple-darwin`). They are not yet code-signed, so if you download
one through a browser, clear the quarantine flag before first launch:

```bash
tar xzf copperpour-v*-aarch64-apple-darwin.tar.gz
xattr -d com.apple.quarantine ./copperpour   # only needed for browser downloads
./copperpour --help
```

Or build from source as above (verified on Apple Silicon; the same
`target/release/copperpour` binary is the MCP server).

KiCad on macOS keeps its tools inside the app bundle and they are not on
`PATH`, so point Copperpour at them in `~/Library/Application Support/copperpour/config.toml`:

```toml
kicad_cli = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
kicad_binary = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad"
# KiCad 10's IPC socket on macOS (enable it in KiCad:
# Preferences → Plugins → "Enable KiCad API")
ipc_address = "ipc:///tmp/kicad/api.sock"
```

Claude Desktop's config lives at
`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "copperpour": {
      "command": "/path/to/copperpour"
    }
  }
}
```

For Claude Code, put the same snippet in a `.mcp.json` in your project root.

The macOS PCM package (`copperpour-pcm-v<version>-macos.zip`) bundles a universal
server binary, so one download covers Apple Silicon and Intel. The schematic
viewer compiles and launches on macOS (Tauri 2 uses the system WKWebView —
WebView2 is only a Windows requirement) but hasn't had the same mileage as
the Windows build yet.

## Setup with Claude Desktop

After a PCM install, the server binary lives in your KiCAD documents folder:

```
C:\Users\<YOU>\Documents\KiCad\10.0\3rdparty\plugins\com_github_copperpour_copperpour\bin\copperpour.exe
```

Edit `%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "copperpour": {
      "command": "C:\\Users\\<YOU>\\Documents\\KiCad\\10.0\\3rdparty\\plugins\\com_github_copperpour_copperpour\\bin\\copperpour.exe"
    }
  }
}
```

Restart Claude Desktop and the Copperpour tools appear. For Claude Code, drop the same
snippet into a `.mcp.json` in your project root (see [examples/](examples/)).

## Migrating from Konnect or LibreKonnect

Copperpour is a separate KiCad package from Konnect, with its own identifier, so
KiCad installs it beside Konnect rather than over it.

1. If you ran `konnect init`, run `konnect uninstall` first. It removes the hooks
   that start the old binary, which stop working once its package is gone.
2. Install the Copperpour package, then uninstall Konnect (or LibreKonnect) in the
   Plugin and Content Manager.
3. Point your MCP client at the new binary, `copperpour` (`copperpour.exe` on
   Windows), as shown above. Renaming the server entry from `konnect` to
   `copperpour` is optional: the bundled hooks and agents accept both names. A
   renamed entry changes the tool names to `mcp__copperpour__*`, so update any
   permission rules that name `mcp__konnect__*`.
4. Run `copperpour init` to install the guidance again.

Your files move on first use: `~/.konnect` to `~/.copperpour` on Linux and macOS,
`%APPDATA%\konnect` to `%APPDATA%\copperpour` on Windows,
`~/Library/Application Support/konnect` to `.../copperpour` on macOS,
`~/.config/konnect` to `~/.config/copperpour` on Linux, and each project's
`.konnect` directory to `.copperpour`. If a directory cannot be moved, it is used
where it is. Nothing is moved when the new directory already exists.

`KONNECT_STATE_DIR`, `KONNECT_BRIDGE_DIR` and `KONNECT_BUILD_COMMIT` are now
`COPPERPOUR_*`; the old names are still read when the new ones are unset. A
`konnect.toml` in the working directory is still read, after `copperpour.toml`.
Source builds and the standalone archives also include a `konnect` executable
that runs `copperpour`; it will be removed in a later release.

## Schematic viewer

A standalone viewer that auto-refreshes as the schematic file changes:

```bash
schematic-viewer.exe path\to\your\root_schematic.kicad_sch
```

Point it at the root sheet of a hierarchical design and every sub-sheet is rendered
too, with a depth-indented sheet selector in the toolbar. Edits saved from KiCAD (or
made by the AI through the schematic tools) re-render only the sheets that changed
and refresh the view live — rendering runs against temp-folder snapshots, so the
viewer never blocks KiCAD from saving. Pan with click-drag, zoom with the wheel,
`0` to fit, `R` to refresh, drag-and-drop to open a different file. Also launchable
by the AI via the `open_schematic_viewer` tool.

Needs the WebView2 runtime (pre-installed on Windows 10/11) and a KiCAD install for
`kicad-cli` (auto-discovered, or pass `--kicad-cli <path>`). Built separately from
the main workspace — see [DEV.md](DEV.md) for build steps.

## Requirements

- KiCAD 10 (Windows is the most-tested platform; macOS works from the release
  binaries or a source build — see the [macOS section](#macos) above. Linux
  compiles and passes tests in CI but hasn't had per-platform QA yet; both are
  tracked on the [roadmap](ROADMAP.md))
- `kicad-cli` (ships with KiCAD — used for exports, ERC, DRC)
- For most PCB tools: KiCAD running with the target board open (IPC API).
  `place_component`, `move_component`, `rotate_component`, and (on KiCAD
  10.0.6+) `flip_component` can safely fall back to a closed board file when
  IPC is unreachable. An older reachable KiCAD makes `flip_component` fail
  closed with `unsupported_capability`; it never edits the file underneath a
  live editor that cannot perform the native flip.

## License: free for the little guys

Copperpour is licensed under the **[GNU AGPL-3.0](LICENSE)**.

If you're a hobbyist, student, freelancer, or open-source project: **use it freely,
no strings attached.** Design boards, ship them, sell them.

If you're a business: using it in-house is free too. The AGPL's conditions apply when
you distribute Copperpour, or let outside users interact with a modified copy over a
network. Then they get its source under the same license.

## Relationship to KiCAD-MCP-Server

The original [Python/TypeScript project](https://github.com/mixelpixx/KiCAD-MCP-Server)
remains fully open (MIT) and maintained. Konnect, now continued as Copperpour, is
the architecture it proved, rebuilt for production:

| | KiCAD-MCP-Server | Copperpour |
|---|---|---|
| Runtime | Node.js + Python + SWIG bindings | Single static binary (20–25 MB) |
| Tool call path | TS → subprocess → Python → SWIG C++ | Direct function call |
| PCB backend | SWIG (deprecated by KiCAD) + experimental IPC | KiCAD 10 IPC API |
| Schematic backend | kicad-skip + custom loaders | Native S-expression engine, atomic writes |
| Context cost | Router pattern | Load/unload toolsets + observability |
| Skills / agents | — | 6 skills + 2 agents bundled |
| License | MIT | AGPL-3.0 |

## Troubleshooting

**Plugin doesn't appear in KiCAD** — install via the Plugin and Content Manager (not
manual copy), then restart KiCAD.

**PCB tools return "KiCAD must be running with the board loaded"** — open KiCAD
with that board first; most PCB tools talk to the running PCB editor. Only an
unreachable KiCAD produces that message: if KiCAD is running and the tool
refuses anyway, the error is the tool's own reason for refusing.
`place_component`, `move_component`, `rotate_component`, and (on KiCAD
10.0.6+) `flip_component` can fall back to a closed board file when no KiCAD
process is reachable. An older, reachable KiCAD makes `flip_component` refuse
rather than silently editing the file underneath it.

See [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) for socket setup, tools
that don't appear after `load_toolset`, and transaction recovery.

**"kicad-cli not found"** — common install paths are auto-detected; set the path
explicitly in the plugin settings dialog, in a `settings.json` beside the binary,
or in a `copperpour.toml` in the working directory. (A file under any other name
works only when passed with `--config`.)

## Support

- Community Discord: [join the Konnect community](https://discord.gg/NVp9RGMmDu)
  for installation help, AI-client setup, design-workflow discussion,
  contributor coordination, and live project-status updates
- Issues & feature requests: [GitHub Issues](https://github.com/Copperpour/Copperpour/issues)
- Upstream backlog & merged PRs: [docs/UPSTREAM_BACKLOG.md](docs/UPSTREAM_BACKLOG.md)
- Roadmap: [ROADMAP.md](ROADMAP.md)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)
