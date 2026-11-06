<a name="top"></a>

<div align="center">

# Copperpour

</div>

**AI-assisted PCB design for KiCAD 10.** Copperpour is a native KiCAD plugin — a single
Rust binary — that lets Claude and other AI assistants design schematics and PCBs
through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP).

**245 tools across 21 on-demand toolsets.** Schematic capture, PCB layout and
routing, ERC/DRC, design-review audits, JLCPCB part search, reference
circuits, and a full manufacturing export pipeline — with bundled skills and agents
that teach Claude KiCAD conventions out of the box.

> **Status: beta.** The core toolchain is tested and working, but this is a young
> release and it wants real-world mileage and review. Issues and PRs are welcome —
> see [CONTRIBUTING.md](CONTRIBUTING.md) and the
> [naming conventions](docs/NAMING_CONVENTIONS.md).
>
> **Formerly Konnect.** See [Why Copperpour exists](#why-copperpour-exists).

**New here?** [Install](#installation) in four steps, then follow
[Getting started](docs/GETTING_STARTED.md) through a first board.

## Why Copperpour exists

Copperpour keeps AI-assisted KiCAD design open. It is the community-maintained
AGPL-3.0 fork of [Konnect](https://github.com/mixelpixx/Konnect), which went closed
source in October 2026, and Konnect was the successor to
[KiCAD-MCP-Server](https://github.com/mixelpixx/KiCAD-MCP-Server). Copperpour's
first release, `v0.14.0`, added 60 open community pull requests and fixes on top of
Konnect `v0.13.0`. Upgrading? See [Migrating from Konnect](#migrating-from-konnect).

- **One binary.** A single static Rust binary, a ~9 MB download, with no Node.js,
  Python or bindings to install. A tool call is a function call.
- **KiCAD's IPC API.** Board edits go through KiCAD 10's official IPC API in real
  time, with KiCAD's own undo/redo, not the SWIG bindings KiCAD is deprecating.
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
  KiCAD's IPC API, with full undo/redo integration
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

Requires **KiCad 10** on Windows, macOS or Linux.

### Step 1: Install the plugin into KiCad

1. Download the PCM package for your OS from
   [Releases](https://github.com/Copperpour/Copperpour/releases):
   `copperpour-pcm-v<version>-{windows,macos,linux}.zip`. The macOS package is a
   universal build (Apple Silicon and Intel). The other release archives are
   standalone server binaries; see [Advanced installation](#advanced-installation).
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

Upgrading from Konnect? See [Migrating from Konnect](#migrating-from-konnect) first.

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

That build has no outbound HTTP client and no vendor endpoints, and CI checks
both. It leaves out three features, which a release build has:

- `jlcpcb`: the JLCPCB parts catalog, its download, the LCSC and EasyEDA
  lookups, and the order upload. That is the catalog, import and datasheet
  tools of the `integration` toolset, `generate_jlcpcb_order_link`, and
  `lcsc_id` in `add_schematic_component` and `batch_place_components`.
- `freerouting`: `check_freerouting` and `route_specctra_dsn`.
- `native-bridge`: the KiCad plugin's Specctra export bridge, which talks to
  KiCad on localhost. `export_specctra_dsn` then always uses Copperpour's own
  exporter, and refuses `native_bridge_mode: "require"`.

Fabrication and assembly output work for every fab house. Without the catalog,
`estimate_cost` prices JLCPCB parts at its average. Add a feature back with
`--features jlcpcb`, for example. The tool counts in these docs are for the
default build.

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

## Migrating from Konnect

Copperpour is a separate KiCad package from Konnect, with its own identifier, so
KiCad installs it beside Konnect rather than over it.

1. If you ran `konnect init`, run `konnect uninstall` first. It removes the hooks
   that start the old binary, which stop working once its package is gone.
2. Install the Copperpour package, then uninstall Konnect in the Plugin and
   Content Manager.
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

Needs the WebView2 runtime on Windows (pre-installed on 10/11; macOS uses the
system WebView, where the viewer runs but has had less testing) and a KiCAD install for
`kicad-cli` (auto-discovered, or pass `--kicad-cli <path>`). Built separately from
the main workspace — see [DEV.md](DEV.md) for build steps.

## Requirements

- KiCAD 10 on Windows, macOS or Linux. Windows is the most tested; macOS and Linux
  have had less hands-on testing.
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

**Schematic edits are refused while KiCad has the file open** — close the Schematic
Editor and retry. Copperpour won't write while KiCad's `~<name>.kicad_sch.lck` lock
exists, so it never overwrites unsaved changes. Read-only tools still work.

**"kicad-cli not found"** — common install paths are auto-detected; set the path
explicitly in the plugin settings dialog, in a `settings.json` beside the binary,
or in a `copperpour.toml` in the working directory. (A file under any other name
works only when passed with `--config`.)

## Support

- Issues & feature requests: [GitHub Issues](https://github.com/Copperpour/Copperpour/issues)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)
