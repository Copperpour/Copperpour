# Claude Desktop Extension (`.mcpb`)

Copperpour publishes self-contained Claude Desktop extension packages (`.mcpb`,
the [MCP Bundles](https://github.com/modelcontextprotocol/mcpb) format) on each
GitHub release. Installing the `.mcpb` registers Copperpour as a local stdio MCP
server in Claude Desktop without manually editing `claude_desktop_config.json`
or locating the KiCad PCM plugin's version-dependent installation path.

## Release packages and supported platforms

| OS / Architecture | Release asset | Claude Desktop support |
| --- | --- | --- |
| Windows (`x86_64`) | `copperpour-v<version>-windows-x86_64.mcpb` | Official `.mcpb` install (`>=0.10.0`) |
| macOS (Apple Silicon & Intel) | `copperpour-v<version>-macos-universal.mcpb` | Official `.mcpb` install (`>=0.10.0`) |
| Linux (`x86_64`) | `copperpour-v<version>-linux-x86_64.mcpb` | Beta / community builds (see [Linux notes](#linux-notes)) |

Each bundle carries the compiled `copperpour` binary for its target platform,
an MCPB `v0.3` `manifest.json` declaring `server.type: "binary"`, and the
Copperpour icon. No Node.js, Python, or external runtime is required.

## Prerequisites

- **KiCad 10** installed locally. Copperpour automatically discovers `kicad-cli`
  and the KiCad project manager in standard install locations on Windows, macOS,
  and Linux.
- **PCB layout tools require the KiCad GUI and IPC API**: open your board in
  KiCad and enable **Preferences → Plugins → Enable KiCad API**. Schematic
  editing, `kicad-cli` exports, ERC/DRC, and read-only project inspection work
  without the KiCad GUI running.
- **Relationship to the KiCad PCM plugin**: the `.mcpb` extension is
  self-contained and runs its own bundled `copperpour` binary. You do not need
  to install the KiCad PCM plugin just to use Copperpour in Claude Desktop, and
  you do not need to point Claude Desktop at KiCad's
  `3rdparty/plugins/com_github_copperpour_copperpour/bin/` path. Installing the
  PCM plugin remains optional if you also want the KiCad toolbar action,
  settings dialog, or the opt-in native Specctra export bridge.

## Install

1. Download the `.mcpb` file for your OS from
   [Releases](https://github.com/Copperpour/Copperpour/releases).
2. In Claude Desktop, open **Settings → Extensions → Advanced settings →
   Install Extension…** (or double-click the `.mcpb` file on Windows/macOS) and
   select the downloaded `.mcpb`.
3. Enable the **Copperpour** extension when prompted.

If you previously registered Copperpour manually in
`claude_desktop_config.json`, remove the manual `"copperpour"` entry under
`mcpServers` and restart Claude Desktop so two copies of the server are not
launched side by side.

## Configuration and operating guidance

- **Zero required extension settings**: the bundle defines no required
  `user_config` prompts. For unusual KiCad paths or a custom IPC socket, use
  Copperpour's standard per-user configuration file
  ([Manual configuration](CONFIGURATION.md)).
- **Operating guidance**: Copperpour delivers its operating instructions and
  bundled references automatically through MCP `initialize` instructions, MCP
  resources, and the `get_guidance` tool (available before toolset loading).
  Claude Desktop chat skill ZIP uploads remain separate from the `.mcpb`
  extension.

## Verify

Start a new chat in Claude Desktop and ask:

1. Call `get_installation_info` — confirm `build.version` matches the `.mcpb`
   version, `runtime.executable_path` points inside Claude Desktop's unpacked
   extension directory, and `kicad.probe_status` is `ok`.
2. Ask Claude to load a toolset (for example `load_toolset` with `sch_components`
   or `pcb_board`) and run a read-only call such as `get_project_info`.
3. With KiCad 10 running and **Enable KiCad API** turned on, confirm
   `get_installation_info` reports a non-null `ipc.endpoint` (with `ipc.source`
   not `"unresolved"`), then run a read-only PCB inspection call on an open
   board.

## Upgrade and uninstall

- **Upgrade**: download the newer `.mcpb` from Releases and install it through
  **Settings → Extensions → Advanced settings → Install Extension…**. Claude
  Desktop replaces the unpacked extension in place without modifying unrelated
  `mcpServers` entries in `claude_desktop_config.json`. Call
  `get_installation_info` after restarting to confirm the updated version.
- **Uninstall**: remove **Copperpour** in Claude Desktop
  **Settings → Extensions**. Removing the extension deletes only the unpacked
  extension bundle and its extension registration; it leaves unrelated entries
  in `claude_desktop_config.json`, your KiCad projects, and your per-user
  Copperpour configuration (`config.toml`) intact.

## Linux notes

Anthropic's official desktop builds target macOS and Windows, with Linux support
distributed through beta/community packages. Depending on the Linux build you
run, the GUI **Install Extension…** picker for `.mcpb` files may be missing,
hidden, or unable to register binary extensions.

If your Linux Claude Desktop build does not install `.mcpb` files directly, use
the manual fallback:

1. Either download the standalone Linux archive
   (`copperpour-v<version>-x86_64-unknown-linux-gnu.tar.gz`) or unzip the
   `.mcpb` archive (a `.mcpb` file is a standard ZIP containing
   `server/copperpour`) into a permanent location such as
   `~/.local/share/copperpour/copperpour` and ensure it is executable
   (`chmod +x`).
2. Add `"copperpour"` to `~/.config/Claude/claude_desktop_config.json`:

   ```json
   {
     "mcpServers": {
       "copperpour": {
         "command": "/home/<YOU>/.local/share/copperpour/copperpour"
       }
     }
   }
   ```

3. Restart Claude Desktop and run the verification steps above.

## Building and validating locally

```bash
python packaging/build-mcpb.py --binary target/release/copperpour --platform linux --out dist
python packaging/validate-mcpb.py --bundle dist/copperpour-v<version>-linux-x86_64.mcpb --smoke-test
```

`validate-mcpb.py` checks the archive against the vendored MCPB `v0.3` JSON
schema (`packaging/schema/mcpb-manifest-v0.3.schema.json`), verifies that the
bundled executable's format matches `compatibility.platforms` (and carries Unix
execute bits on POSIX), and with `--smoke-test` unpacks the bundle and runs a
real stdio MCP `initialize`, `tools/list`, `get_installation_info`, and
`load_toolset` round trip.
