# Manual configuration

Standard KiCad installations need no executable path configuration. Copperpour
looks for `kicad-cli` on `PATH`, then in known install locations: the Windows
registry and common install directories, the macOS app bundle, or Linux system
binary directories. The KiCad project manager is also discovered automatically.
The API socket is discovered when KiCad is running with its API enabled.

Use overrides for an unusual installation, to select a particular KiCad version,
or when automatic discovery cannot identify your API socket.

## Check discovery first

Ask your AI client to call `get_installation_info`. Its `kicad` block reports the
CLI path, version and `probe_status`; `not_found` with a null `cli_path` means no
CLI was found. Its `configuration` block identifies the configuration file used
by the running server and any files it shadowed.

## Set executable paths

Edit the active server configuration, or pass `--config /absolute/path/to/copperpour.toml`
as server arguments in your MCP client. An explicit `--config` bypasses the
normal configuration search. Restart the server after editing its settings.

For example, to select a macOS installation explicitly:

```toml
kicad_cli = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
kicad_binary = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad"
```

These are the standard app-bundle paths and normally do not need to be set.
Replace them with the actual paths for a custom installation. Set only the
fields you need to override. `kicad_cli` is used for exports and checks;
`kicad_binary` selects the KiCad project manager used to launch the UI.

The per-user TOML configuration locations are:

| OS | Path |
| --- | --- |
| Windows | `%APPDATA%\copperpour\config.toml` |
| macOS | `~/Library/Application Support/copperpour/config.toml` |
| Linux | `~/.config/copperpour/config.toml` |

Only the first existing configuration file is loaded; settings from later files
are not merged. A working-directory configuration or plugin `settings.json` can
therefore shadow the per-user file. See the
[configuration search order](KICAD_INTEGRATION.md#configuration) before creating
another file. JSON configuration uses the same field names.

## Override the API socket

Leave `ipc_address` unset for automatic discovery. If discovery is ambiguous,
copy the endpoint KiCad shows under **Preferences → Plugins** into the active
configuration, for example:

```toml
ipc_address = "ipc:///tmp/kicad/api.sock"
```

That address is an example, not a portable default. A configured address takes
precedence over `KICAD_API_SOCKET` and discovery. See
[IPC socket troubleshooting](TROUBLESHOOTING.md#kicad-ipc-socket-path-not-configured)
for discovery and reconnection behavior.

For other server settings, see the
[configuration reference](KICAD_INTEGRATION.md#configuration).
