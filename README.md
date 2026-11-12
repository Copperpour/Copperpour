<a name="top"></a>

<div align="center">

<h1><img src="packaging/resources/icon.png" alt="" height="48" align="absmiddle"> Copperpour</h1>

</div>

> [!IMPORTANT]
> **On 2026-11-20 Copperpour leaves the `mixelpixx/Konnect` fork network.** Users don't
> need to do anything. If your fork was made from `mixelpixx/Konnect`, you will need to
> re-fork before you can open PRs here; see [#413](https://github.com/Copperpour/Copperpour/issues/413).

**AI-assisted PCB design for KiCad 10.** Copperpour is a native KiCad plugin — a single
Rust binary — that lets Claude and other AI assistants design schematics and PCBs
through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP).

**257 tools across 23 on-demand toolsets.** Schematic capture, PCB layout and
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

MCP registration is enough to receive short operating rules each session.
The server also serves its bundled skills and references on demand through
`get_guidance` and MCP resources, matching the running binary. No local skill
installation is needed for this guidance. Client support determines whether it
uses the instructions/resources; `get_guidance` is available before toolset loading.

Run `init` only if you want native skill discovery, locally editable skill files,
or Claude agents/hooks. Use the binary from Step 2 (PCM does not add it to `PATH`):

```bash
copperpour init                  # Claude Code: skills, agents and hooks
copperpour init --client codex   # Codex CLI/desktop: skills under ~/.agents/skills
```

Re-run after upgrading Copperpour; these native Claude Code/Codex installs overwrite
differing files, so back up any you edited. See [Managing skills](#advanced-installation).

For other local clients, `init`, `status`, and `uninstall` also accept these
**skills-only** presets. They use ownership checks to preserve edited/unrelated
content and retain the target root on uninstall:

| Client | Install command | Default user skills root |
| --- | --- | --- |
| [Gemini CLI](https://geminicli.com/docs/cli/skills/) | `copperpour init --client gemini` | `~/.gemini/skills` |
| [Cursor](https://cursor.com/docs/skills) | `copperpour init --client cursor` | `~/.cursor/skills` |
| [Antigravity](https://antigravity.google/docs/skills) | `copperpour init --client antigravity` (or `agy`) | `~/.gemini/config/skills` |

Use the same `--client` with `status` and `uninstall`. For example,
`copperpour status --client cursor` checks that installation. An explicit
`--skills-dir` overrides `COPPERPOUR_SKILLS_DIR`, which overrides the preset root.
To install project skills, use an absolute project skills directory with
`--skills-dir`. These presets install guidance; configure the MCP connection in
Step 2 separately. Local installation alone does not deploy skills or Copperpour
to remote/cloud agent environments. Gemini CLI and Cursor also discover the
shared `~/.agents/skills` directory: if you already installed there with
`--client codex`, use that target rather than installing duplicate copies.

[Codex skills](https://learn.chatgpt.com/docs/build-skills) also work in the desktop
app; no separate desktop preset is needed. The `claude` target means
[Claude Code](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview).
It does not install skills into Claude Desktop's chat interface, whose custom
skills use a [ZIP-upload workflow](https://support.claude.com/en/articles/12512180-use-skills-in-claude).
Desktop skill packaging is separate from this installer.

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

The program is at `target/release/copperpour`, including the browser schematic
viewer. See [DEV.md](DEV.md) for build and test details.

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

For another client, or an isolated installation, choose an **absolute skills root**:

```sh
copperpour init --skills-dir /absolute/path/to/client/skills
copperpour status --skills-dir /absolute/path/to/client/skills
copperpour uninstall --skills-dir /absolute/path/to/client/skills
```

On Windows, use an absolute path such as `"C:\agent\skills"`. Alternatively set
`COPPERPOUR_SKILLS_DIR`; an explicit `--skills-dir` takes precedence. Empty or
relative targets are rejected. These three commands use the custom target in
**skills-only** mode, even with `--client claude`: they do not write client profile
files, agents, hooks, or native installation markers. Existing commands without
an override keep their client-specific behavior. Gemini, Cursor, and Antigravity
presets always use this ownership-checked skills-only mode. The override does not
configure MCP startup or the double-click installer.

Point your client's skill discovery at this root. It contains portable Markdown
`<skill>/SKILL.md` files with frontmatter and optional `references/` files; automatic
discovery depends on the client. Clients without skill discovery can read those
files as guidance. Claude agents and hooks are separate integrations.

Custom installation records the installed contents in `.copperpour-skills.json`
inside the target. Keep that ownership marker: uninstall removes only recorded
files whose contents still match, preserves edits and unrelated files, and removes
only empty skill directories. It never removes the root. Reinitializing refuses
to overwrite locally edited or unowned content that differs from the bundle;
back it up and move it aside before updating. Custom targets reject symlinks at
the root or along managed paths, including the ownership marker. Avoid concurrent
installers or edits while installing/removing skills. Use explicit `status` with
the same target or preset for portable-target drift checks; the MCP startup notice
checks native client installations.

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

The `open_schematic_viewer` MCP tool starts a browser viewer that refreshes when
saved schematic files change. Pass the root sheet of a hierarchy to get a sheet
selector. The tool returns a private loopback URL and asks the OS to open it;
use `open_browser: false` to return the URL without opening a browser. If opening
a browser fails, the service remains available at the returned URL.

The server reuses one viewer per canonical root schematic for the MCP server
context. Keep the URL private: its fragment contains the launch credential.
Stdio disconnect or HTTP server shutdown ends the viewer; disconnecting a client
from the stateless HTTP transport does not stop it. The display reflects saved
files, including agent edits, rather than unsaved KiCad changes. Pan by dragging,
zoom with the wheel, press `0` to fit or `R` to refresh, and use the toolbar to
select sheets or launch KiCad. Browser mode is bound to its root at launch and
has no file-drop control. It needs `kicad-cli`, using the server's configured path
or normal discovery, and a browser; no Tauri runtime is needed.

The browser viewer is embedded in the MCP server and PCM package. No separate
viewer executable or GTK/WebKit/WebView build is needed. The standalone
`schematic-viewer` command has been retired; use `open_schematic_viewer` instead.
See the [retirement migration](docs/api-migrations/retire-standalone-viewer.md) and
[browser response contract](docs/api-migrations/browser-viewer-mcp.md).

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
