<a name="top"></a>

<div align="center">

<h1><img src="crates/schematic-viewer/icons/icon.png" alt="" height="48" align="absmiddle"> Copperpour</h1>

</div>

> [!IMPORTANT]
> **On 2026-11-20 Copperpour leaves the `mixelpixx/Konnect` fork network.** Users don't
> need to do anything. If your fork was made from `mixelpixx/Konnect`, you will need to
> re-fork before you can open PRs here; see [#413](https://github.com/Copperpour/Copperpour/issues/413).

**AI-assisted PCB design for KiCad 10.** Copperpour is a native KiCad plugin — a single
Rust binary — that lets Claude and other AI assistants design schematics and PCBs
through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP).

**257 tools across 22 on-demand toolsets.** Schematic capture, PCB layout and
routing, ERC/DRC, design-review audits, JLCPCB part search, reference
circuits, and a full manufacturing export pipeline — with bundled skills and agents
that teach Claude KiCad conventions out of the box.

> **Status: beta.** The core toolchain is tested and working, but this is a young
> release and it wants real-world mileage and review. Issues and PRs are welcome —
> see [CONTRIBUTING.md](CONTRIBUTING.md) and the
> [naming conventions](docs/NAMING_CONVENTIONS.md).
>
> **Formerly Konnect.** See [Why Copperpour exists](#why-copperpour-exists).

**New here?** [Install](#installation) in four steps, then follow
[Getting started](docs/GETTING_STARTED.md) through a first board.

## Why Copperpour exists

Copperpour keeps AI-assisted KiCad design open. It is the community-maintained
AGPL-3.0 fork of [Konnect](https://github.com/mixelpixx/Konnect), which went closed
source in October 2026, and Konnect was the successor to
[KiCAD-MCP-Server](https://github.com/mixelpixx/KiCAD-MCP-Server). Upgrading? See
[Migrating from Konnect](docs/MIGRATING_FROM_KONNECT.md).

- **One binary.** A single static Rust binary, a ~9 MB download, with no Node.js,
  Python or bindings to install. A tool call is a function call.
- **KiCad's IPC API.** Board edits go through KiCad 10's official IPC API in real
  time, with KiCad's own undo/redo, not the SWIG bindings KiCad is deprecating.
- **Safe schematic writes.** Its own S-expression engine edits `.kicad_sch` files
  with revision-checked atomic writes.
- **Small context.** A ~2K-token starter kit, with toolsets loaded on demand, instead
  of a ~23K-token catalogue on every listing.

## What it does

Instead of describing changes and applying them by hand, the AI works your project
directly:

- **Place and wire schematic components** — add resistors, ICs, connectors; wire them
  together by pin name
- **Lay out the PCB** — place, move, rotate, and route footprints in real time via
  KiCad's IPC API, with full undo/redo integration
- **Run design checks** — ERC, DRC, connectivity validation, decoupling audits,
  power-rail review, BOM health checks
- **Export production files** — Gerbers, drill, BOM, pick-and-place, 3D models, PDF
- **Search JLCPCB parts** — find in-stock components in a local catalog of about
  800k parts and suggest alternatives
- **Start from reference circuits** — USB-C, LDO, buck converter, STM32, I2C, LED
  templates with verified component values
- **Watch it happen** — a live schematic viewer auto-refreshes as the AI edits

The full tool catalog is documented in [tool-directory.md](tool-directory.md).

## How it works

| Layer | Mechanism |
|-------|-----------|
| Schematic editing | Direct `.kicad_sch` S-expression editing with atomic writes (no KiCad required) |
| PCB editing | KiCad 10 IPC API (NNG + protobuf) — real-time and undo-aware; single-footprint placement has a safe headless fallback |
| Specctra routing | Revision-bound Rust DSN export, local Freerouting MCP routing, and strict one-transaction SES import; an authenticated KiCad 10 native-export bridge is explicit opt-in |
| Exports & checks | `kicad-cli` subprocess (Gerber, PDF, ERC, DRC, …) |
| Transport | MCP JSON-RPC over stdio (default), or Streamable HTTP (`transport = "http"` / `"both"`) |

Explore the request path and safety boundaries in the
[interactive architecture diagrams](docs/ARCHITECTURE_DIAGRAMS.md).

## Installation

Requires **KiCad 10** on Windows, macOS or Linux.

### Step 1: Install the plugin into KiCad

1. Download the PCM package from
   [Releases](https://github.com/Copperpour/Copperpour/releases):
   `copperpour-pcm-v<version>.zip` carries Windows, macOS and Linux x86_64. The
   smaller `copperpour-pcm-v<version>-{windows,macos,linux}.zip` carry one OS each.
   The macOS build is universal (Apple Silicon and Intel). The other release
   archives are standalone server binaries; see
   [Advanced installation](#advanced-installation).
2. In KiCad: **Plugin and Content Manager → Install from File**, select the zip,
   restart KiCad.
3. Enable the IPC API: **Preferences → Plugins → Enable KiCad API**. Most PCB tools
   need KiCad running with the board open.

Verify: **PCB Editor → Tools → External Plugins** lists **Copperpour**.

### macOS: point Copperpour at KiCad

<a id="macos"></a>

KiCad's tools live inside the app bundle and aren't on `PATH`. Add to
`~/Library/Application Support/copperpour/config.toml`:

```toml
kicad_cli = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
kicad_binary = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad"
ipc_address = "ipc:///tmp/kicad/api.sock"
```

### Step 2: Register the MCP server

Your AI client starts the server itself, over stdio, whenever it needs it; you never
run it by hand.

The PCM install puts the server binary at:

| OS | Server binary |
| --- | --- |
| Windows | `C:\Users\<YOU>\Documents\KiCad\10.0\3rdparty\plugins\com_github_copperpour_copperpour\bin\copperpour.exe` |
| macOS | `~/Documents/KiCad/10.0/3rdparty/plugins/com_github_copperpour_copperpour/bin/copperpour` |
| Linux | `~/.local/share/kicad/10.0/3rdparty/plugins/com_github_copperpour_copperpour/bin/copperpour` |

In the all-platforms package, `bin/copperpour` is a small `sh` launcher that runs
the macOS or Linux server beside it, so the path is the same in both packages.

<a id="setup-with-claude-desktop"></a>
**Claude Desktop**: add to `claude_desktop_config.json`
(`%APPDATA%\Claude\` on Windows, `~/Library/Application Support/Claude/` on macOS)
and restart Claude Desktop:

```json
{
  "mcpServers": {
    "copperpour": {
      "command": "C:\\Users\\<YOU>\\Documents\\KiCad\\10.0\\3rdparty\\plugins\\com_github_copperpour_copperpour\\bin\\copperpour.exe"
    }
  }
}
```

**Claude Code**: the same snippet in `.mcp.json` in your project root (see
[examples/](examples/)).

**Codex**:

```bash
codex mcp add copperpour -- /path/to/copperpour --client codex
```

**Other MCP clients**: register a stdio server with the binary as the command and
no arguments. Most clients take the same shape as Claude Desktop:

```json
{
  "mcpServers": {
    "copperpour": { "command": "/path/to/copperpour" }
  }
}
```

### Step 3: Install the skills (optional)

Run the binary from Step 2 (it is not on `PATH` after a PCM install):

```bash
copperpour init                  # Claude: skills, agents and hooks
copperpour init --client codex   # Codex: skills under ~/.agents/skills
```

Re-run after upgrading Copperpour; it overwrites differing files, so back up any
you edited. See [Managing skills](#advanced-installation).

### Step 4: Verify

Ask your AI client for something read-only on an existing project:

> Show the project info for `~/kicad/myboard/myboard.kicad_pro`.

It should answer from `get_project_info` with the project name, its schematic and
PCB paths, and the KiCad version that wrote them. If Copperpour's tools don't show
up, or the call fails, ask for `get_installation_info`: it reports which binary is
serving, and whether it found `kicad-cli` and the KiCad API. See also
[Troubleshooting](#troubleshooting).

> [!TIP]
> **Installed? Continue with [Getting started →](docs/GETTING_STARTED.md)**
>
> It walks you from an empty project through schematic, PCB layout, design review and
> manufacturing files, with example prompts for each step.

Upgrading from Konnect? See [Migrating from Konnect](docs/MIGRATING_FROM_KONNECT.md) first.

## Advanced installation

You don't need any of this for a normal install. Expand a section when you need it.

<details>
<summary><b>Standalone binary</b>: run the server without the KiCad plugin</summary>

The Releases page also has standalone server archives (`copperpour-v<version>-<target>`,
for example `aarch64-apple-darwin`). Unpack one and register the binary as in Step 2.

On macOS these files are not code-signed yet. If you downloaded one through a
browser, clear the quarantine flag first:

```bash
tar xzf copperpour-v*-aarch64-apple-darwin.tar.gz
xattr -d com.apple.quarantine ./copperpour
./copperpour --help
```

</details>

<details>
<summary><b>Build from source</b>: Rust, <code>protoc</code> and <code>cmake</code></summary>

You need [Rust](https://rustup.rs), `protoc` and `cmake`:

| System | Install the build tools |
| --- | --- |
| Windows | `choco install protoc cmake` |
| macOS | `brew install protobuf cmake` |
| Debian, Ubuntu | `apt install protobuf-compiler libprotobuf-dev cmake` |
| Fedora | `dnf install protobuf-compiler protobuf-devel cmake` |

Then build:

```bash
cargo build --release -p copperpour
```

The program is at `target/release/copperpour`. The schematic viewer is built
separately; see [DEV.md](DEV.md).

For a build that makes no outbound connections, leave out the integrations:

```bash
cargo build --release -p copperpour --no-default-features
```

[DEV.md](DEV.md#build-features) lists what that build leaves out.

</details>

<details>
<summary><b>Managing skills</b>: check, update or remove what <code>copperpour init</code> installed</summary>

- `copperpour status` shows whether the installed skills match your Copperpour version.
- The server makes the same check once for each installed version. If the skills
  differ, it adds a one-line notice to its `initialize` instructions, and
  `get_installation_info` shows the result.
- Re-run `copperpour init` to update them.
- `copperpour uninstall` removes them. Add `--client codex` for Codex.

</details>

<details>
<summary><b>Autorouting</b>: route boards with Freerouting</summary>

Copperpour can autoroute a board with [Freerouting](https://freerouting.org):
`check_freerouting` checks the setup, then `export_specctra_dsn` →
`route_specctra_dsn` → `plan_specctra_ses_import` → `apply_specctra_ses` exports
the board, routes it, and imports the result.

Only fixed straight tracks and through vias are supported so far; boards with arcs,
zones or unlocked routing are refused before anything changes.

Optionally, KiCad 10 can export the board itself instead of Copperpour's own
exporter: enable the **native Specctra bridge** in Copperpour's settings dialog and
set `native_bridge_mode` to `"prefer"` (fall back to Copperpour's exporter) or
`"require"` (fail if KiCad's export is unavailable).

</details>

## Schematic viewer

The PCM package ships a standalone viewer beside the server, in the same `bin/`
folder: `schematic-viewer` (`schematic-viewer.exe` on Windows). It auto-refreshes as
the schematic file changes:

```bash
schematic-viewer path/to/your/root_schematic.kicad_sch
```

Point it at the root sheet of a hierarchical design and every sub-sheet is rendered
too, with a depth-indented sheet selector in the toolbar. Edits saved from KiCad (or
made by the AI through the schematic tools) re-render only the sheets that changed
and refresh the view live — rendering runs against temp-folder snapshots, so the
viewer never blocks KiCad from saving. Pan with click-drag, zoom with the wheel,
`0` to fit, `R` to refresh, drag-and-drop to open a different file. The AI can also
open it with the `open_schematic_viewer` tool.

It uses the system WebView: WebView2 on Windows (pre-installed on 10 and 11),
WebKitGTK on Linux; macOS has had less testing. It needs KiCad's `kicad-cli`
(auto-discovered, or pass `--kicad-cli <path>`). To build it from source, see
[DEV.md](DEV.md).

## Requirements

- KiCad 10 on Windows, macOS or Linux. Windows is the most tested; macOS and Linux
  have had less hands-on testing.
- `kicad-cli`, which ships with KiCad, for exports, ERC and DRC.
- For most PCB tools, KiCad running with the board open and its API enabled. A few
  placement tools [fall back to the closed board file](docs/TROUBLESHOOTING.md#pcb-tools-return-kicad-must-be-running-with-the-board-loaded).

## License

Copperpour is licensed under the **[GNU AGPL-3.0](LICENSE)**.

If you're a hobbyist, student, freelancer, or open-source project: **use it freely,
no strings attached.** Design boards, ship them, sell them.

If you're a business: using it in-house is free too. The AGPL's conditions apply when
you distribute Copperpour, or let outside users interact with a modified copy over a
network. Then they get its source under the same license.

## Troubleshooting

Start with `get_installation_info`: it reports which binary is serving, and whether it
found `kicad-cli` and the KiCad API. The common problems:

- [Plugin doesn't appear in KiCad](docs/TROUBLESHOOTING.md#plugin-doesnt-appear-in-kicad)
- [PCB tools return "KiCAD must be running with the board loaded"](docs/TROUBLESHOOTING.md#pcb-tools-return-kicad-must-be-running-with-the-board-loaded)
- [A schematic write is blocked by a KiCad editor lock](docs/TROUBLESHOOTING.md#a-schematic-write-is-blocked-by-a-kicad-editor-lock)
- ["kicad-cli not found"](docs/TROUBLESHOOTING.md#kicad-cli-not-found)
- [Tools don't appear after `load_toolset`](docs/TROUBLESHOOTING.md#tools-dont-appear-after-load_toolset)

[docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) covers the rest, including socket
setup and transaction recovery.

## Support

- Issues & feature requests: [GitHub Issues](https://github.com/Copperpour/Copperpour/issues)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)
