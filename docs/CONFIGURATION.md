# Manual configuration

Standard KiCad installations need no executable path configuration. Copperpour
looks for `kicad-cli` on `PATH`, then in known install locations: the Windows
registry and common install directories, the macOS app bundle, or Linux system
binary directories. The KiCad project manager is also discovered automatically.
The API socket is discovered when KiCad is running with its API enabled.

Use overrides for an unusual installation, to select a particular KiCad version,
or when automatic discovery cannot identify your API socket.

## Check discovery first

Ask your AI client to call `get_installation_info`. Its
`prerequisite_health` block reports the observed CLI path, version, and IPC
readiness. Its `configuration` block identifies the configuration file used
by the running server and any files it shadowed. After changing that file,
restart the server to load it.

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
[configuration reference](KICAD_INTEGRATION.md#configuration). Whether your
MCP client needs `eager_toolsets` is in
[Which clients see a loaded toolset](TROUBLESHOOTING.md#which-clients-see-a-loaded-toolset).

## Issue reporting and privacy

Copperpour can send structured bug reports and feature requests to the `copperpour` Sentry project (via the `report_issue` meta-tool) so agents can report bugs or missing capabilities without a GitHub account. Automatic sending is **off by default**: panics, `handler_error`, and `mutation_outcome_uncertain` results are scrubbed and stored locally (`0600` on Unix) in the Copperpour state directory, and the agent asks you before anything is sent.

### What is sent

- Issue category, bug vs. feature-request tag, and the agent's summary/details (which you approve before sending).
- Server context: Copperpour version (`copperpour@<version>`) and build features, OS/arch, KiCad and `kicad-cli` versions, and MCP transport.
- Recent sanitized call records: tool name, toolset, status, `error_kind`, and duration.
- For a captured panic or error: the tool, `error_kind`, the panic location in Copperpour's source, and a fingerprint of the message. The message itself is never stored or sent, because it can name references, nets or files.

### What is never sent

- Design files, file contents, net/reference/value/label names, or tool arguments/results.
- Absolute filesystem paths (home directories become `~`, project roots become `<project>`, and non-Copperpour path components are dropped).
- Usernames, hostnames, or IP addresses (`send_default_pii = false`, `server_name` unset).

### Configuration and opt-out

Configure `[issue_reporting]` in your active `config.toml` (or via **Tools → External Plugins → Copperpour** in KiCad):

```toml
[issue_reporting]
enabled = true      # master switch (default: true)
capture = true      # store scrubbed crash/error reports locally and ask (default: true)
automatic = false   # send captured crash/error reports without asking (default: false)
```

Environment overrides:

- `COPPERPOUR_ISSUE_REPORTING=0` disables issue reporting completely (no local capture, `report_issue` refuses and sends nothing).
- `DO_NOT_TRACK=1` forces `automatic = false`.
- Offline builds (`cargo build --no-default-features`) omit the `issue-reporting` feature, Sentry, and all outbound HTTP client code.
