# Troubleshooting

## Schematic viewer without a desktop session

`open_schematic_viewer` uses the loopback browser service. It can serve saved schematics
when the MCP client's server environment has no `DISPLAY`, `WAYLAND_DISPLAY`,
or `XDG_RUNTIME_DIR`. Set `open_browser: false` for headless or remote use:

```json
{"schematic": "/absolute/project/root.kicad_sch", "open_browser": false}
```

Open the returned private `url` in a browser yourself. A default OS-opener failure
is reported separately in `browser_launch_error`; it does not stop a successfully
started service or remove the returned URL. The viewer displays saved files,
so save the schematic before expecting edits to appear. For remote use, forward
the returned loopback port to the same local port, preserving `127.0.0.1`
and the URL's private credential fragment;
see [viewer service contract](../crates/copperpour-viewer/README.md#service-contract).

If `open_schematic_viewer` still reports `Failed to initialize GTK`, check
`get_installation_info`: restart the MCP client with a build containing the
browser-backed tool (introduced in #439).
Older installed binaries keep their previous behavior.

The standalone `schematic-viewer` command has been retired. Use the MCP tool
instead; there is no separate viewer executable to install or configure.

## JLCPCB assembly cannot obtain native midpoint geometry

Open the exact requested `.kicad_pcb` in PCB Editor and enable KiCad IPC.
JLCPCB assembly export requires native per-pad bounding boxes; a saved file or
another open board is not a substitute. Stop editing during export. A missing
box or changing live snapshot refuses the attempt before output creation;
retry after correcting the reported cause. The tool does not save or modify
your board. Generic and fabrication-only exports retain file-based behavior.

If positions move after upgrading, review old anchor-compensation offsets in
your correction policy. Midpoints now precede user corrections, so old
workarounds can double-correct them. See [JLCPCB coordinates and evidence](JLCPCB_CPL_CORRECTIONS.md).
Always inspect the fabricator's placement preview before ordering.

## IPC operation returned `ipc_outcome_unknown`

Stop automatic retries. Copperpour sent a request but could not establish its
outcome, or a transaction's attempted rollback failed. The live board may
contain some or all of the requested changes; this is not proof that nothing
happened. The structured error reports `board_state: unknown` and
`retry_safe: false`.

Inspect the requested board in KiCad and reconcile what was actually applied
before repeating any mutation. Do not edit the saved file as a workaround:
it may be older than the live editor state. Keep the full diagnostic, including
any commit identifier, when reporting the failure. Bounded recovery is only
available for schematic-to-PCB sync when the original operation answers within
its additional recovery window. An unknown result does not establish that an
undo will reverse a pending, unpublished commit.

## KiCad rejected items during an IPC write

Check `recovery.evidence.rejected_items` for the item statuses KiCad reported. `rollback: "confirmed"` means KiCad acknowledged dropping the named commit. It does not verify the overall board outcome. Inspect the live board and reconcile any changes, then correct the rejection before retrying. Do not edit the saved board as a workaround.

## Schematic sync returned `ipc_batch_recovered`

The apply was aborted after a late IPC reply, not published. Copperpour confirmed
the known commit was dropped and compared a fresh serialized live snapshot
with the exact target's pre-apply snapshot. This reports `board_state: unchanged`,
`drop_confirmed: true`, and `retry_safe: false`. Review the live board and run a
fresh dry run before deciding whether to apply again; do not replay the old
apply automatically or edit the saved file as a workaround.

Sync allows up to 30 additional seconds after an ordinary 30-second receive
window. The remaining allowance is shared by rollback and verification, not
renewed at each step. Missing or late publish replies cannot be rolled back
automatically; failed verification still reports `ipc_outcome_unknown`.

Sync creates are sent in ordered groups of at most 32 footprints, with every
group and the updates inside one KiCad commit. A successful apply is one Undo
entry, not one entry per group. This bounds the item count, not the serialized
request bytes: it does not establish KiCad's listener size limit or eliminate
timeouts. A rejected or uncertain group stops the apply; Copperpour never retries
it or publishes a partial group sequence automatically.

## Which Copperpour binary is this client using?

Call the always-visible `get_installation_info` tool in the affected MCP
session. The result comes from the process serving that call and includes its
version, build commit when available, executable path, process working directory
(`runtime.working_directory`), conservatively detected install source, the version
produced by the binary currently on disk at that same path, KiCad CLI version,
redacted IPC endpoint, and restart guidance.

`installation.binary_on_disk.newer_than_running: true` is reported only when
both stable versions can be parsed and the on-disk binary is newer. `null`
means the comparison could not be proven, not that the process is current.
Likewise, `installation.source: "unknown"` means no trusted package manifest
identified the channel; Copperpour does not guess from directory names. Endpoint
credentials and query or fragment data are redacted.

Follow the returned platform-specific guidance, restart the **host process that
owns the MCP connection** (not merely the conversation), then call
`get_installation_info` and `server_stats` again to verify that the answering
process actually restarted (`runtime.executable_path`,
`configuration.selected_path`, `started_at_ms`, `total_calls`). This diagnostic
writes nothing.

Which host process owns the connection depends on the client surface:

- **Claude Code (CLI)**: exit and restart the CLI process.
- **Claude Desktop (Chat)**: fully quit and relaunch Claude Desktop. Starting a
  new chat reuses the app's running Copperpour process.
- **Claude Code inside Claude Desktop (Code tab)**: current builds follow
  Anthropic's surface separation and spawn a per-session Copperpour process from
  Claude Code's config (`~/.claude.json` or `.mcp.json`), so a new Code session
  reloads it. Some builds have been observed to share an app-managed Copperpour
  process across Code sessions; if `server_stats` still reports the same
  `started_at_ms` in a new session, fully quit and relaunch Claude Desktop
  instead.
- **KiCad-owned server**: stop the plugin server or quit and reopen KiCad.

Configure Copperpour only for the surface you use unless you are intentionally
testing multiple registrations; registering in both Claude Code and Claude
Desktop spawns separate server processes side by side and makes diagnosis
harder. Note also that `calls.jsonl` is written by every Copperpour process on
the machine, so interleaved entries in that log do not prove two sessions share
a process — `server_stats` (`started_at_ms`, `total_calls`, `per_tool`) is
in-process and settles it.

## Relative paths return `file_not_found`

Relative paths in tool arguments resolve against the Copperpour server
process's working directory (`std::env::current_dir()`), which the MCP client sets
when launching the server. They do not resolve against an editor's workspace root
or the directory of any open project.

Callers and agents should pass absolute filesystem paths to avoid ambiguity.
When a handler refuses on a missing path, the structured `file_not_found` error
reports the exact `absolute_path` it checked alongside the argument `path` as given.
Call `get_installation_info` to inspect `runtime.working_directory` and see which
directory the server process is running from.

## Installed guidance is out of date

Installing a new Copperpour binary does not update installed guidance. Run
`copperpour status` with the binary your MCP client uses and the same client or
directory selection used for installation. The default client is Claude Code;
for other clients, use `--client` with `codex`, `gemini`, `cursor`, or `antigravity`
(alias `agy`).
See the [client setup table](../README.md#step-3-install-the-skills-optional).

For a custom directory, pass the same `--skills-dir <absolute path>` or
`COPPERPOUR_SKILLS_DIR` to `status`, `init`, and `uninstall` (the variable is
ignored when `--client` is given). Custom targets and the
Gemini, Cursor, and Antigravity presets use the ownership-checked skills-only mode.
Keep `.copperpour-skills.json` in that target: it records ownership and installed
contents, so uninstall preserves local edits. If init refuses an edited file,
back it up and move it aside before retrying. Init also refuses a file the marker
does not list, even with identical contents, because it may belong to another
installation. Files an earlier release installed and this one no longer bundles
are reported by `status` and removed by `init` or `uninstall` unless edited.
Marker entries that are not skill file paths are refused rather than trusted as
deletion targets. See
[Managing skills](../README.md#advanced-installation).

For native client installations, each file and hook is compared with that
binary's bundle:

| Status | Meaning |
|--------|---------|
| `current` | Byte-identical to the bundle, or the exact hook command `init` writes |
| `different` | Present, but not identical. When `.installed-manifest-<client>.json` is present, unmodified files from a previous Copperpour install are distinguished from local user edits |
| `missing` | Not present |
| `unreadable` | Present but could not be read, or `settings.json` is not valid JSON or not the shape Claude expects |

A `different` hook carries a reason: `legacy_handler` is the pre-hook-JSON
`… skill <name>` form, which Claude ignores; `alias_handler` is a hook for the
`konnect` alias that v0.15 and v0.16 shipped beside this binary, or, in
the all-platforms PCM package, for the server behind the `bin/copperpour`
launcher; both are this install's own, and
`init` replaces them; `duplicate_handler` is the current hook with another handler of
ours beside it, or a second entry for it, which `init` removes;
`stale_matcher` is our entry under a tool matcher from an older release,
which `init` rewrites; `shared_entry` is the current hook only inside an entry
that also holds your handlers under your matcher, which `init` moves into an
entry of its own; and `other_executable` is a hook that runs another
Copperpour binary, written exactly as `copperpour init` writes it. A hook is
`current` exactly when `copperpour init` would leave our handler for it
unchanged.

The serving process makes the same comparison for both clients once per
`(install marker, embedded guidance bundle)` pair. The first start for a
marker—or after a binary upgrade that changes that client's embedded skills,
agents, or hook matchers—scans. When an install manifest
(`~/.copperpour/.installed-manifest-<client>.json`) matches the current install
marker, unmodified files and managed hook entries (matching the previously
recorded digests/entries) and newly added bundled files are updated in place
automatically; if every item is then `current`, the install marker and check
record advance silently without emitting a notice. Locally modified files,
user-deleted files, user-edited/shared hook entries, symlinks, and legacy
installs without a manifest are left untouched; when either client remains
`out_of_sync`, the server sets the MCP `initialize` `instructions` to a
one-line notice naming any locally modified files and the runnable `init`
command (using the serving executable's path when `copperpour` is not on
`PATH`), attaches the same notice as `guidance_notice` to non-`get_guidance`
tool calls in that session, and records
`~/.copperpour/.guidance-checked-<client>` once the first eligible tool call
delivers it (while `current` and `not_installed` states are recorded on scan).
A session that exits after `initialize` without calling a tool does not consume
the notice. Later starts with the same marker and embedded bundle read the
record instead of rescanning and give no notice. Running `copperpour init`
starts a new check; upgrading to a binary with an unchanged guidance bundle
stays quiet. `get_installation_info` reports the result under `guidance`; its
`checked` field is `now` for a scan in this process, with per-file detail, or
`earlier` for a recorded state without it. `copperpour status` always scans and
never modifies files. Run the reported `init` command to overwrite remaining
`different` files (back up any edits you made first).

## "KiCAD IPC socket path not configured"

Any tool that talks to a live KiCAD session (`save_project`, PCB editing,
`check_kicad_ui`, …) needs the IPC socket address.

**At startup** Copperpour resolves it in order: the `ipc_address` in your
config, then `KICAD_API_SOCKET` (set only for plugins KiCad launches itself),
then the platform default — `<temp dir>/kicad/api.sock` and any
`api-<pid>.sock` beside it (live-PID sockets on Linux and macOS; live named
pipes in `\\.\pipe` on Windows, though KiCad 10.0.8 on Windows does not yet
bind a fallback `api-<pid>.sock` pipe when another KiCad holds `api.sock`).
The startup log on stderr says which it picked, and warns when nothing was
found.

When the address came from that probe, or nothing was found, Copperpour probes
again on every IPC call. So the order in which the AI client and KiCad start no
longer matters, and a KiCad restarted while the server runs is found again
without restarting the server. A configured `ipc_address` or `KICAD_API_SOCKET`
is used as given and never replaced.

**A stale `api.lock`.** KiCad 10 binds `api.sock` only while it holds
`<temp dir>/kicad/api.lock`. If a session that crashed left that lock behind,
the next KiCad listens on `api-<pid>.sock` instead, and a server that only
knew `api.sock` reported "Connection refused". Copperpour now finds that
socket. When more than one socket qualifies (an old `api.sock` beside
`api-<pid>.sock`, or two KiCads running), it uses the one that answers a Ping;
if not exactly one answers, it picks none and the error lists every candidate,
on every call, never falling back to the address found at startup: set
`ipc_address` to the one KiCad shows under Preferences → Plugins. Telling
candidates apart sends each a Ping before the call, so a busy KiCad that does
not answer within the 30 s reply timeout delays the call and counts as not
answering. Quitting
every KiCad and deleting the stale `api.lock` and `api*.sock` files also clears
it.

Where the probe looks differs by platform, because the endpoint does. On Linux
and macOS the socket is a filesystem entry and its metadata is read. On Windows
KiCad's `ipc://` endpoint is a **named pipe**, which has no filesystem presence
at all, so the same path is looked up in the pipe namespace (`\\.\pipe\`)
instead. Before that was true, Windows detected nothing and every setup had to
be configured by hand; on a Konnect release from before that fix, set
`ipc_address` or `KICAD_API_SOCKET` explicitly.

If the address is unresolved, both of the following must be correct — neither
happens automatically:

1. **The socket path in Copperpour's plugin settings** (inside KiCAD)
2. **The Copperpour server registration in your AI client's MCP config**

Step by step (based on the diagnostic guide contributed in
[#18](https://github.com/mixelpixx/Konnect/issues/18)):

1. Open KiCAD normally.
2. Go to **Preferences → Plugins** (or **KiCad → Preferences → Plugins** on macOS) and check **"Enable KiCad API"**.
   Confirm a line like this appears:

   ```
   Listening on ipc://C:\Users\<you>\AppData\Local\Temp\kicad\api.sock
   ```

   Copy the whole address including the `ipc://` prefix — it is unique to
   your machine and user.
3. In KiCAD, open **Tools → External Plugins → Copperpour** to open the settings
   dialog.
4. Paste the address into the **IPC Socket** field and click **Save**.
5. Confirm your AI client has the `copperpour` MCP server registered in the
   config for the surface you are using — `~/.claude.json` or `.mcp.json` for
   Claude Code, or `claude_desktop_config.json` (or the `.mcpb` extension) for
   Claude Desktop Chat — pointing at the `copperpour` binary; see
   [examples/](../examples/). The MCP client starts the stdio server from this
   registration. The KiCad plugin saves settings and applies the native bridge
   preference. Configure only the surface you use to avoid duplicate Copperpour
   processes.
6. Restart the host process that owns the MCP connection so it spawns a fresh
   Copperpour process that reads the saved settings (starting a new chat in
   Claude Desktop reuses the existing process — see
   ["Which Copperpour binary is this client using?"](#which-copperpour-binary-is-this-client-using)).
   Verify the restart with `get_installation_info` and `server_stats`.
7. Verify: have the AI call `open_project`. Expected:

   ```json
   { "kicad_ui_running": true, "message": "KiCAD is running and IPC is available." }
   ```

Alternative: launching the server from within KiCAD sets `KICAD_API_SOCKET`
automatically, and a `copperpour-settings.json` passed via `--config` can carry
`ipc_socket_path` directly.

## PCB tools return "KiCAD must be running with the board loaded"

The IPC tools talk to KiCAD's **running PCB editor**. Open your board file in
KiCAD first, and make sure the API is enabled (previous section).

That message means the transport was unreachable. If KiCAD *is* running with
that board open and a tool still refuses, the error you get back is the tool's
own reason — "a polygon needs at least 3 points", "requested board … is not open
in KiCAD" — and it names what to change about the request.

`place_component`, `move_component`, `rotate_component`, and (on KiCad 10.0.6+)
`flip_component` can fall back to a closed board file when no KiCad process is
reachable. An older, reachable KiCad makes `flip_component` refuse rather than
silently edit the file underneath it.

## Footprint unit metadata disappears after a live PCB edit

KiCad 10 releases before 10.0.8 can drop a footprint's `(units ...)` block, which records which pins belong to each symbol unit. This can happen whenever Copperpour sends an existing footprint back whole through IPC `UpdateItems`. That includes moves, rotations, batch placements, library refresh, schematic sync updates, and some field and pad edits. Closed-board placement edits and native `FlipItems` do not replace the footprint through `UpdateItems`.

The requested edit can succeed without a warning. The affected versions leave the mapping out of typed IPC footprint replies, so those replies cannot reveal the loss. A tool that compares the full board text, such as `set_teardrop_settings`, can detect it after the edit and report `mutation_outcome_uncertain`. That does not undo the edit.

Upgrade to KiCad 10.0.8 or later before making these live edits, or make them directly in PCB Editor instead. See [KiCad issue #25859](https://gitlab.com/kicad/code/kicad/-/work_items/25859) and the [10.0.8 IPC fix](https://gitlab.com/kicad/code/kicad/-/commit/b7c0b5206f3f5cb9b5fb062be441bcaad6ef135c).

Upgrading does not restore missing metadata. If you already edited the board on an affected version, save the edit before comparing the `.kicad_pcb` with your backup or previous revision. Check the edited footprints for missing `(units ...)` blocks before committing the board. If a linked footprint's mapping is missing, run KiCad's **Update PCB from Schematic** using the matching schematic.

## `launch_kicad_ui` from an SSH session

A server started from an SSH session inherits no `DISPLAY` or `WAYLAND_DISPLAY`,
so KiCad exits at once with `Unable to initialize GTK+, is DISPLAY set properly?`.
`launch_kicad_ui` reports that exit and says the server has no display. Pass
`display` to open KiCad on the desktop's X display (Xwayland on a Wayland desktop):

```json
{"project": "/absolute/project/board.kicad_pro", "display": ":0"}
```

`display` takes local displays only (`:N` or `:N.S`). KiCad takes `XAUTHORITY`
from the server's environment. If the X server refuses the connection, set
`XAUTHORITY` where the MCP client starts the server, to the cookie file the
desktop session uses: `echo $XAUTHORITY` in a terminal on the desktop shows it.
That is often `~/.Xauthority` on an X11 session, and a file under
`$XDG_RUNTIME_DIR` (such as `.mutter-Xwaylandauth.*`) under Xwayland.

## KiCad is running, but `ipc_failure` says it did not answer

`check_kicad_ui` and `open_project` report `ipc_failure` as
`{ "kind", "message" }` whenever they establish why KiCad did not answer their
Ping. It is `null` when no kind was established: the Ping succeeded with
`AS_OK`, or
`check_kicad_ui`'s own timeout expired first, which it reports as
`timed_out: true`. The kind comes from the error NNG returned, so it tells
apart cases that used to share one `false`, and each has a different fix:

| `kind` | What happened | What to do |
|---|---|---|
| `not_configured` | No address was configured or discovered. | See ["KiCAD IPC socket path not configured"](#kicad-ipc-socket-path-not-configured). |
| `no_listener` | Nothing is listening at the address. | Open an editor with the API enabled, and check the address against KiCad's "Listening on" line. KiCad's first-run wizard can write a missing `kicad_common.json` with `api.enable_server: false`; set it back to `true`. |
| `access_denied` | The operating system refused this account access to the endpoint. Likely a different account or a restrictive ACL. | Run Copperpour as the same operating-system user as KiCad; see below. |
| `handshake_failed` | A listener accepted the connection and did not complete NNG's handshake. | Another program holds KiCad's address ([#52](https://github.com/Copperpour/Copperpour/issues/52)). Close it, then restart KiCad so it can bind. |
| `transport_error` | Any other dial or send failure. | Read `message`. |
| `request_failed` | The request did not complete and may have reached the endpoint. A KiCad status in `message` (for example `AS_NOT_READY` while an editor is still loading) proves it did; a receive timeout or a malformed reply does not. | With a KiCad status: wait for the editor to finish loading, then retry. Without one: treat the endpoint as unproven and check what holds the address. For `AS_NOT_READY`, `ipc_failure` also carries a `note`: if it keeps coming back, a modal dialog (the first-run wizard, a message box) is open; close it. |

`handshake_failed` takes NNG's fixed 10-second negotiation limit to appear.
That is longer than `check_kicad_ui`'s default 5-second budget, which then
reports `timed_out` instead, so pass `timeout_seconds` above 10 to see it.

### `access_denied` from a sandboxed AI client

Some AI clients run tool commands in a sandbox under a separate
operating-system account. On Windows, Codex does
([#300](https://github.com/mixelpixx/Konnect/issues/300),
[#532](https://github.com/mixelpixx/Konnect/issues/532)). The refusal itself
proves only that the operating system denied this account. The likely cause in
that setup: KiCad's endpoint was created by the desktop user, and a Windows
named pipe created without an explicit security descriptor gives full control
only to its creator and read access to everyone else, while a request needs
both read and write. A Copperpour started inside the sandbox is then refused,
however its address is configured. A restrictive ACL on the pipe produces the
same kind.

The known workaround: run Copperpour outside the sandbox, as the same user as
KiCad, over HTTP:

```toml
# copperpour.toml in the directory Copperpour is started from
transport = "http"
http_address = "127.0.0.1:3000"
ipc_address = "ipc://C:/Users/<you>/AppData/Local/Temp/kicad/api.sock"
```

Start `copperpour` from a normal terminal and point the client at
`http://127.0.0.1:3000/mcp`; `http://127.0.0.1:3000/health` answers `ok` while
the server is up. Keep the address on `127.0.0.1`: the HTTP transport has no
authentication. KiCad needs no change.

## Tools answer from the file while KiCad is open

Tools that read the board IPC-first report `"source": "ipc"` or `"file"`, and a
`file` answer means the live board was never consulted — unsaved changes are
missing from it. Those tools go through one shared IPC helper, and it logs a
`WARN` on stderr each time the transport could not be reached, naming the
address it tried; check that against the address KiCad reports as "Listening
on".

The warning covers that helper, which is every tool that falls back to a file.
It is not a global "IPC failed" log: the health tools (`check_kicad_ui`,
`launch_kicad_ui`) and `get_project_info` dial KiCad directly and report the
outcome in their own response — `ipc_responsive`, `connected` — rather than
falling back to anything, so read those fields instead of looking for a
warning. `check_kicad_ui` and `open_project` also say why in `ipc_failure`
(previous section). A KiCad that *answered and refused* is not warned about anywhere; that
is a tool error, and it says so.

## Live schematic navigation on KiCad 10: `schematic_editor_document_identity`

KiCad 10.x's IPC API covers the PCB editor; the schematic editor arrives with
KiCad 11. A KiCad 10 schematic editor still answers the open-documents query,
but with the sheet's file name only: no project and no sheet path. Copperpour
cannot address a schematic without both and does not guess them from the
saved file, the board or your arguments. So on KiCad 10:

- `get_editor_state` reports the schematic editor with `addressable: false`,
  no documents, and an `unavailable_reason` naming your KiCad version. The PCB
  editor is reported as usual.
- `get_editor_selection`, `mutate_editor_selection`,
  `resolve_navigation_target` and `resolve_cross_probe_target` refuse a
  schematic request with `unsupported_capability`,
  `capability: "schematic_editor_document_identity"`, and your KiCad version
  in `kicad_version`. That field is `null` from KiCad 10's standalone
  schematic editor, which does not report its version.

This is a limit of the KiCad version, not a stale or changed document, so
retrying will not help. PCB navigation works on KiCad 10. Every file-based
schematic tool is unaffected. Live schematic navigation needs KiCad 11 or
later; KiCad's 10.99 development builds already report the identity.

## "layer 'X' has no KiCAD board layer this build can represent"

The footprint or request names a layer this build cannot map, so the request was
refused before anything was sent. Nothing on the board changed.

This refusal exists because the alternative is worse. KiCAD 10.0.5 does not
validate the layer field on an incoming item, so an unrepresentable value used
to reach it and **terminate the process**, discarding any unsaved board
([#237](https://github.com/mixelpixx/Konnect/issues/237)). Copperpour now stops at
its own boundary instead.

Every layer a KiCAD 10 footprint can legally draw on is supported, including
`Dwgs.User`, `Cmts.User`, `Eco1/2.User`, `F/B.Adhes`, `Margin`, `Rescue`,
`In1.Cu`–`In30.Cu` and `User.1`–`User.45`. If you hit this on stock library
content, that is a bug worth reporting with the footprint name — the message
names the layer and the item.

**If you are on v0.6.0 or earlier**, placing
`Connector_USB:USB_C_Receptacle_GCT_USB4105-xx-A_16P_TopMnt_Horizontal` or
`Connector:BJB_Pico_46.110.1001_Receptacle_Horizontal` can kill KiCAD outright.
Update to v0.6.1 or later.

## `unsafe_file_fallback` after KiCad disappears

Copperpour remembers each board it positively observes open through IPC during the
current server process. If IPC later becomes unreachable, a board-file mutation
for that same board fails with `error.kind: "unsafe_file_fallback"` instead of
editing the saved file. KiCad may have crashed or been force-quit with unsaved
state, so the saved `.kicad_pcb` is not known to be authoritative. The error
confirms that Copperpour left it unchanged
([#240](https://github.com/mixelpixx/Konnect/issues/240)).

KiCad running with no PCB editor open is *not* this state: nothing was ever
identified, so a `board_source` read answers from the saved file and says
`no_pcb_editor_at_endpoint`. The refusal below is only for a board this server
did observe live.

Read-only tools that take a `board_source` selector report the same kind for the
same reason, and nothing was going to be written: under the default
`board_source: "auto"` they refuse rather than present a possibly stale file as
the board's current state. The recovery below applies, and inspecting the
snapshot deliberately — `board_source: "saved"`, which answers and states its
freshness limitation — is also available there.

Recover deliberately:

1. Reopen or recover the board in KiCad.
2. Reconcile any recovered/unsaved work and save the authoritative board.
3. Continue through live IPC.

If KiCad was intentionally closed cleanly and closed-board mode is desired,
first confirm that the saved file is authoritative, then restart Copperpour to
begin a new server session. Repeating the tool call does not clear the safety
memory, and an agent must not restart Copperpour or edit `.kicad_pcb` directly to
bypass the refusal.

This memory is intentionally process-local. It cannot detect a KiCad crash that
happened before the current Copperpour process started. File-fallback success
therefore carries a warning describing that cold-start limitation.

## `invalid_configuration` from a config tool

`load_user_config`, `get_effective_config` or a save refuses with
`error.kind: "invalid_configuration"` when a Copperpour preferences file exists
but cannot be used. `error.path` names the file and `error.reason` starts with
`malformed_json:` (invalid JSON, or a root that is not an object — often a
truncated write or a hand edit), `unreadable:` (permissions, a directory at
that path, invalid UTF-8), or `non_regular_file:` (a save target is a symbolic
link or another non-regular entry).

Older versions answered with their defaults here, and the next save then replaced
the file with them. It now refuses and writes nothing, so your settings are
still in the file. For `non_regular_file:`, use a regular file at the config
path; fixing the JSON does not fix a symbolic link. Otherwise fix the JSON or
the read error (or move the file aside to start again from defaults), then
retry. The user file is `%APPDATA%\copperpour\config.json` on
Windows, `~/Library/Application Support/copperpour/config.json` on macOS and
`~/.copperpour/config.json` elsewhere; a project's is
`<project_dir>/.copperpour/project.json`.

A save that ends in `mutation_outcome_uncertain` could not prove what is on
disk. Read the file before retrying; do not assume nothing was written.

## An older schematic-to-PCB sync left extra unnamed pads

Konnect v0.4.0 through v0.6.1 could rewrite each drawing shape inside
a footprint as an anonymous pad while `update_pcb_from_schematic` reassigned
pad nets ([#244](https://github.com/mixelpixx/Konnect/issues/244)). Current
versions prevent and detect that corruption, but prevention does not repair a
board already saved by an affected release.

Open the affected board in KiCAD, load `pcb_components`, and call
`repair_corrupted_footprints` with the board path. Its default dry run scans
for that corruption's exact signature: an anonymous pad with no net and an empty layer set,
paired one-for-one with a drawing shape missing from the registered footprint
library. It refuses ambiguous pad layouts or an unavailable library rather
than guessing. Optionally pass `references` to restrict the scan.

Review `candidates`, then call the tool again with `dry_run: false` and the
exact returned `plan_revision` as `expected_plan_revision`. All candidates are
repaired in one KiCAD undo commit. Placement, footprint identity, symbol path,
pad nets and non-shape children are preserved; a live read-back verifies that
the phantom pads are gone and the expected drawing shapes returned. Save the
board and run DRC afterward. Ctrl-Z reverses the complete repair if its visual
result is not what you expect.

## KiCad prerequisite health

Copperpour checks the KiCad CLI and IPC once in the background when its MCP
server starts. The new checks run concurrently with budgets of five seconds
for the CLI version and two seconds for IPC. The server logs a summary to
stderr. Initialization and unrelated tools can continue while the checks run.

Call `get_installation_info` and read `prerequisite_health`. The report includes
the time of the observation, the executable or redacted endpoint source, and
next steps for each capability. After fixing setup, call the tool with
`{"refresh_prerequisite_health": true}` or restart the MCP server. A refresh uses
the running server's configuration. Restart after changing its configuration
file or launch environment.

- **CLI.** A version from KiCad 10 or later establishes that the executable
  launched and reported a supported version. For `not_found`, check
  `kicad_cli` or the MCP process's PATH. For `unexecutable` or `timed_out`, run
  the configured CLI with `--version` as the account running Copperpour.
- **IPC.** `communication_confirmed` records whether the probe received a
  recognizable response. The endpoint can respond before the PCB Editor is
  ready or when no PCB document is open. If the endpoint is unreachable,
  verify the intended KiCad instance, its **Enable KiCad API** setting, and
  its endpoint. An unreachable or unknown result leaves the cause unresolved.
  With several automatically detected candidates, health reports ambiguity
  until there is evidence for one endpoint or you configure it explicitly.
  A ready result applies to the sampled endpoint. Tools still verify their
  own board targets.

For an unusual install location, follow [Manual configuration](CONFIGURATION.md).
If a setting appears to be ignored, inspect the tool's `configuration` block:
`selected_path` identifies the file used by the running server, and
`skipped_existing_paths` lists files it shadowed. Only the first existing file is
loaded; settings from later files are not merged.

## Native Specctra export used the Rust fallback

The KiCad-native exporter is an optional KiCad 10 compatibility path. Open the
PCB Editor, choose **Tools → External Plugins → Copperpour**, enable **KiCad 10
native Specctra bridge**, save, and close the dialog. The status changes to
running after the setting is applied. The requested board must be saved and be
the active PCB Editor document.

`export_specctra_dsn` defaults to `native_bridge_mode: "disable"`, Copperpour's
own exporter; pass `"prefer"` to use the bridge when it runs. The response says
whether `method` was `kicad10_native_actionplugin` or `kicad_ipc_snapshot` and
includes bounded `native_bridge_diagnostics`. Use `native_bridge_mode: "require"`
when testing the native path so an unavailable bridge is an error instead of a
fallback.

Shared import-safety refusals apply to all modes. A `.kicad_dru` refusal is not a
Rust outline limitation: custom rules are not carried to the router, whichever
exporter writes the DSN, so Copperpour cannot vouch for routes against them.
`require` does not bypass it (#75 owns future custom-rule support). In contrast,
a circular Edge.Cuts cutout can use native export: `require` attempts the bridge
before any Rust-only outline check. If `prefer` cannot use native and the Rust
writer also refuses, the error includes both the native diagnostic and the Rust
fallback reason. Correct the reported cause and retry with new output paths; no
DSN/manifest is published on these pre-publication refusals. See
[selection order](KICAD_INTEGRATION.md).

The bridge listens only on an ephemeral IPv4 loopback port and requires a
per-session bearer token. Registration and temporary DSN files live under the
per-user local-data directory (`%LOCALAPPDATA%\copperpour\native-bridge` on
Windows); `COPPERPOUR_BRIDGE_DIR`, an absolute path, overrides it for diagnostics
and tests. With no absolute directory (a relative `HOME`, `LOCALAPPDATA` or
override) the settings dialog shows the bridge as unavailable; set `HOME` or
`COPPERPOUR_BRIDGE_DIR` to an absolute path. A clean plugin shutdown removes its own registration and temporary files.
Stale registrations left by a hard KiCad crash are ignored because Copperpour probes and
authenticates each candidate before use.

This option is unavailable on KiCad 11 after removal of the legacy SWIG Python
API. Copperpour then uses its Rust exporter unless KiCad gains an equivalent
supported IPC operation.

## A schematic write is blocked by a KiCad editor lock

Copperpour refuses to change a `.kicad_sch` file while the sibling
`~<name>.kicad_sch.lck` exists. Close the schematic editor normally and retry.
Read-only schematic tools remain available while the lock exists.

KiCad's lock stores only a username and hostname, not a process identifier or
document-instance token. Copperpour therefore cannot distinguish a live lock from
one left by a crash without risking unsaved editor state. It treats valid,
foreign-host, empty, and malformed locks alike and never removes one
automatically. If KiCad crashed, first confirm that no schematic editor owns the
file; reopening and closing the project cleanly is the preferred way to resolve
the lock. Remove a confirmed stale lock manually only as a last resort.

## Transaction recovery is blocked by divergent content

Multi-file schematic changes persist a `.copperpour-transaction-<id>.json`
write-ahead journal in the project before changing any target. Releases before
the rename to Copperpour named it `.konnect-transaction-<id>.json`; those
journals are still found, recovered and abandoned the same way. Recovery leaves
files that already match the intended replacement alone. Apply and recovery both
recheck each replacement target against its recorded before image just before the
rename. If a save is detected, Copperpour leaves that file untouched, stops and
keeps the journal. Earlier writes may already have applied and are not rolled back.
Writers that ignore Copperpour's locks can still save in the gap between the final
check and rename.

Inspect active journals without printing their contents:

```text
copperpour transaction status <project-dir>
```

Each target is reported as `pending`, `applied`, or `divergent`. Retry safe
recovery with:

```text
copperpour transaction recover <project-dir>
```

If a target is divergent, first inspect the schematic in KiCad and preserve
the version you want. To unblock future transactions without changing any
schematic file, explicitly abandon the journal:

```text
copperpour transaction abandon <project-dir> <transaction-id> --force
```

Abandonment renames the journal to
`.copperpour-transaction-<id>.abandoned.json` (an old journal keeps its
`.konnect-` prefix); it does not restore, replace, or
delete a target. The abandoned file is retained as recovery evidence and is
ignored by future transactions. Delete it only after you have made any backup
you need.

Active and abandoned journals contain complete before/after images of every
affected schematic. Treat them as sensitive, do not attach them to bug reports
without reviewing their contents, and do not commit them. Both forms are
ignored by the repository `.gitignore`.

Cooperative document locks are stored outside the project under the platform
local-data directory. Set `COPPERPOUR_STATE_DIR` to an absolute directory to
override that location. A relative override is rejected rather than falling
back to project-local sidecars, and so is a relative default (a relative
`HOME`, say): edits then refuse until `HOME`, `XDG_DATA_HOME` or
`COPPERPOUR_STATE_DIR` is an absolute path.

## Which clients see a loaded toolset

Copperpour decides which toolsets are loaded; each MCP client keeps its own copy
of `tools/list`. The server behaves the same for every client:

- The first `tools/list` holds the meta-tools and the starter kit (`project`,
  `config`). It holds every toolset when `eager_toolsets` is `true`, or when the
  client's `clientInfo.name` starts with `claude-ai` or `local-agent-mode-`, the
  two Claude Desktop connections seen not to offer tools loaded later
  (`CLIENTS_THAT_CACHE_TOOL_LIST` in
  `crates/copperpour-core/src/mcp/handler.rs`).
- `initialize` declares `tools.listChanged`. `load_toolset` and `unload_toolset`
  send `notifications/tools/list_changed` over stdio and over the HTTP SSE
  stream, and the next `tools/list` has the change. The server cannot see
  whether the client uses the new list, so a `load_toolset` reply that adds
  tools carries a `hint` with the recovery. `get_active_toolsets` reports the
  server's state, not the client's.
- A direct `tools/call` runs the tool it names and no other. A tool whose
  toolset is not loaded is refused with `toolset_not_loaded`, which names the
  toolset. A name no toolset has is refused with `unknown_tool`. Neither loads
  anything, unless `auto_load_toolsets` is `true`: then the owning toolset loads
  and the named tool runs.
- `find_tools` searches every registered tool, loaded or not, and returns its
  name, toolset, description and load state, but no schema. It was added after
  v0.17.0. `call_tool` runs a tool by name whether or not it is loaded or
  listed, with that tool's own argument checks, and by default only read-only
  tools (see [`call_tool`](TOOL_SYSTEM.md#call_tool)). Neither changes
  `tools/list`, and both are in every first listing, so neither needs a refresh.

A loaded toolset becomes callable in the same session when the client fetches
`tools/list` again after the notification and offers the new tools. A client
that ignores the new list never offers them, and restarting it, or a reconnect
that respawns a stdio server, starts a new server from the starter kit. Such a
client needs a complete first listing (`eager_toolsets`, or the automatic load
above), which takes effect at the next start, or `call_tool`, which needs no
refresh. For which process to restart, see
["Which Copperpour binary is this client using?"](#which-copperpour-binary-is-this-client-using).

What has been observed, by client and setting. "Not observed" means nobody has
reported it yet.

| Client | Default (`load_toolset`) | `eager_toolsets = true` | Recommended |
| --- | --- | --- | --- |
| Claude Code (CLI) | Same session: Claude Code 2.1.327, Linux, v0.16.1 ([#50](https://github.com/Copperpour/Copperpour/issues/50#issuecomment-7003867638)). Never on two other setups, cause not found: Konnect 0.11.1 on Windows, Claude Code version not recorded ([#50](https://github.com/Copperpour/Copperpour/issues/50)), and 2.1.325 on Linux with v0.15.0, CLI or Code tab not stated ([#50](https://github.com/Copperpour/Copperpour/issues/50#issuecomment-6993958716)). | Callable after a restart: 2.1.325, Linux, v0.15.0 ([#50](https://github.com/Copperpour/Copperpour/issues/50#issuecomment-6993958716)). | Default. If new tools are rejected as "No such tool available", set `eager_toolsets` and restart the CLI. |
| Claude Code in Claude Desktop (Code tab) | Not observed separately; see the 2.1.325 report above. | Not observed separately. | As for the CLI. |
| Claude Desktop (Chat), `claude-ai` | Every tool from the first listing: the server loads every toolset at `initialize` (server log, v0.15.0, Linux, [#448](https://github.com/Copperpour/Copperpour/pull/448)). Before that, never: the app fetched the new list and still refused its tools (Konnect v0.11.0, Kubuntu 24.04, app version not recorded, [Konnect#233](https://github.com/mixelpixx/Konnect/issues/233#issuecomment-5469073195)); also on Windows 11 ([Konnect#169](https://github.com/mixelpixx/Konnect/issues/169)). | Callable across many sessions: Konnect v0.11.0, Kubuntu 24.04 ([Konnect#233](https://github.com/mixelpixx/Konnect/issues/233#issuecomment-5469073195)). | Nothing to set. |
| Claude Desktop cloud agent connection, `local-agent-mode-<key>` | Every tool from the first listing since v0.17.0; loaded tools were then called in cloud sessions ([#448](https://github.com/Copperpour/Copperpour/pull/448)). Before that, never: v0.15.0, Linux, app version not recorded. | Not observed separately. | Nothing to set. |
| GitHub Copilot in VS Code | Never: Copilot keeps its first `tools/list`, and reloading the window did not help (versions not recorded, [Konnect#325](https://github.com/mixelpixx/Konnect/issues/325#issuecomment-5407317596)). | Partly: Copilot offers an unstable subset under a 128-tool cap across all MCP servers, and refuses the rest as "currently disabled by the user" ([Konnect#325](https://github.com/mixelpixx/Konnect/issues/325#issuecomment-5407317596)). | None verified; see [VS Code Copilot](#vs-code-copilot-says-a-tool-is-currently-disabled-by-the-user). |
| Zed | Same session: Zed 1.29.3, stdio ([#53](https://github.com/Copperpour/Copperpour/issues/53#issuecomment-7068905279)). Never on Zed 1.14.2, which also ran other tools in place of the missing ones (Konnect 0.10.1 and 0.11.1, [#53](https://github.com/Copperpour/Copperpour/issues/53)). | Callable over stdio, Zed version not stated ([#53](https://github.com/Copperpour/Copperpour/issues/53#issuecomment-6875990140)). | Default (observed on 1.29.3). `eager_toolsets` on a build that rejects new tools. |
| Muse (Meta) | Never: Muse reads `tools/list` once per session (Konnect v0.11.0, Windows, Muse version not recorded, [#131](https://github.com/Copperpour/Copperpour/issues/131)). | Callable from the next session, no tool cap seen ([#131](https://github.com/Copperpour/Copperpour/issues/131)). | `eager_toolsets`; see [Muse](#muse-meta-never-sees-tools-loaded-after-startup). |
| Reasonix | Never: Reasonix 1.19.7, Konnect v0.2.2 ([Konnect#134](https://github.com/mixelpixx/Konnect/issues/134)). | Not observed. | `eager_toolsets` (not verified). |
| Codex, Gemini CLI, Cursor, Antigravity | Not observed. | Not observed. | Default; `eager_toolsets` if new tools stay unknown. |

Still to observe, so none of it is claimed above:

- `call_tool` and `find_tools` in a client application.
  [#450](https://github.com/Copperpour/Copperpour/pull/450) drove `call_tool`
  from a script over stdio.
- Claude Code in the Code tab after `load_toolset`, and the cause of the two
  Claude Code failures in [#50](https://github.com/Copperpour/Copperpour/issues/50).
- Any client over HTTP. The server's SSE delivery is tested, not a client's
  response to it.
- A reconnect that keeps the server process, in any client.
- Claude Desktop's two connections on macOS and Windows with a current release.
- Reasonix with `eager_toolsets`, and Codex, Gemini CLI, Cursor and Antigravity
  at all.

To add a result, open an issue with the client and its version, the Copperpour
version, the transport, and the `MCP client: …` line the server logs to stderr
at startup.

## Tools don't appear after `load_toolset`

After a successful `load_toolset` call the server sends a
`notifications/tools/list_changed` notification, and MCP clients are expected to
re-fetch `tools/list` in response. If newly loaded tools never show up:

1. Check whether your client honors `notifications/tools/list_changed`; some
   keep their first tool list. What each client has been seen to do is in
   ["Which clients see a loaded toolset"](#which-clients-see-a-loaded-toolset).
2. Disable any competing tool-search or tool-filter layer sitting between the
   model and the server. A Chrome-extension "tool search" that shadowed the real
   tool list caused exactly this in
   [#67](https://github.com/mixelpixx/Konnect/issues/67).
3. Re-issue `tools/list` (reconnect the MCP server or restart the host process
   that owns the MCP connection — starting a new chat in Claude Desktop reuses
   the same server process; see
   ["Which Copperpour binary is this client using?"](#which-copperpour-binary-is-this-client-using)).
   If the client reconnects without respawning the server, the loaded toolset
   state survives a list refresh; if the server process itself restarts, re-run
   `load_toolset` (or set `eager_toolsets` below).

If your client caches the initial tool list and never re-fetches it, none of the
above helps: the tools are loaded server-side, but the client has no schema to
invoke them with. `load_toolset` reports the names it loaded and *not* their
schemas, so a model can see a tool named in the reply and still be unable to
call it. That is the symptom in
[Konnect#134](https://github.com/mixelpixx/Konnect/issues/134) (Reasonix) and
[Konnect#169](https://github.com/mixelpixx/Konnect/issues/169) (Claude Desktop);
the [client table](#which-clients-see-a-loaded-toolset) lists the others. The
server cannot tell whether a client refreshed, so every `load_toolset` reply
that adds tools carries a `hint` pointing here.

For clients that cache the initial list but do not also cap the number of
callable tools, the fix is to make the *first* listing complete:

```json
{ "eager_toolsets": true }
```

in `copperpour.toml` in the working directory, or a `settings.json` beside the binary. Every toolset is then loaded at
startup, so `tools/list` carries all 290 tools from the first call.

It is off by default because it costs what the router exists to save: roughly
25K tokens per listing instead of ~2K. Turn it on only if your client needs it.

Note that `auto_load_toolsets` does **not** solve this. It loads a toolset when
a tool from it is *called*, which helps only a client that already knows the
tool name — so it does nothing for a client whose tool list is stale.

Without a restart, `call_tool` can reach a tool the client never listed, since
`call_tool` itself is in every first listing; `find_tools` finds the tool's
name. By default `call_tool` runs only read-only tools; see
[`call_tool`](TOOL_SYSTEM.md#call_tool) for `call_tool_allow_writes` and what
allowing it means for client approvals. Neither has been observed in a client
application yet.

## VS Code Copilot says a tool is "currently disabled by the user"

That exact message comes from the VS Code Copilot client layer, not Copperpour.
In the confirmed report in
[#325](https://github.com/mixelpixx/Konnect/issues/325), the attempted calls did
not appear in Copperpour's `get_recent_calls` output because Copilot refused them
before they reached the server.

Two Copilot behaviors make the normal toolset settings ineffective:

- With `eager_toolsets = false`, Copilot caches the initial `tools/list` and
  does not re-fetch it after `notifications/tools/list_changed`. Tools loaded
  later therefore remain unavailable to the model.
- With `eager_toolsets = true`, Copperpour advertises its full catalog at startup,
  but Copilot applies its own total callable-tool budget across all configured
  MCP servers. The mixelpixx/Konnect#325 reporter measured a 128-tool ceiling and saw
  only an
  arbitrary, changing subset of Copperpour tools exposed. Tools outside that
  subset produced "currently disabled by the user."

Changing Copperpour from stdio to HTTP/SSE does not remove a limit applied by the
client after it receives `tools/list`. Reloading the VS Code window also does
not make an over-budget catalog callable.

Current options are:

1. Disable unrelated MCP servers or tools if that brings the complete set you
   need below the client's budget.
2. Use an MCP client that honors `tools/list_changed` or can expose Copperpour's
   full catalog.
3. Keep `eager_toolsets` off and call tools through `call_tool`, using
   `find_tools` to look up names. This is the proxy's pattern built into the
   server, but it has not been observed in Copilot. `find_tools` returns no
   schema, and `call_tool` runs only read-only tools unless the operator sets
   `call_tool_allow_writes = true`.
4. Use the community two-tool proxy pattern demonstrated in
   [the #325 follow-up](https://github.com/mixelpixx/Konnect/issues/325#issuecomment-5407317596):
   expose only `copperpour_help` and `copperpour_call` to Copilot, let
   `copperpour_help()` list names or return one tool's description and schema,
   and let `copperpour_call(tool, arguments)` forward the actual call to a child
   Copperpour process started with `eager_toolsets = true`.

The proxy is a community workaround attached to the issue, not code shipped or
reviewed by Copperpour; inspect it and configure its executable path before use.
See ["Which clients see a loaded toolset"](#which-clients-see-a-loaded-toolset)
for what has and has not been observed in Copilot.

## Muse (Meta) never sees tools loaded after startup

Muse reads `tools/list` once at session start and does not act on
`notifications/tools/list_changed`. The symptom is the same class as
[#134](https://github.com/mixelpixx/Konnect/issues/134) and
[#169](https://github.com/mixelpixx/Konnect/issues/169): `load_toolset`
reports success, but every tool outside the starter kit stays uncallable,
persistently across restarts.

Unlike VS Code Copilot ([#325](https://github.com/mixelpixx/Konnect/issues/325)),
no client-side tool cap has been observed, so `eager_toolsets = true` fully
resolves it: the first listing carries the whole catalog (all 290 tools) and
every tool resolves with no further loading. Prefer
`settings.json` beside the binary — a harness-spawned server may run with a
redirected environment that never sees a user-profile config file. Then
restart the Muse session: Muse does not respawn a running MCP server, so the
setting takes effect on the next session start. Cost is the usual ~25K tokens
per listing instead of ~2K.

## Plugin doesn't appear in KiCad

Install via **Plugin and Content Manager → Install from File** with the
`copperpour-pcm-*.zip` release asset (not the bare binary archives), then restart
KiCAD.

## A board no longer opens after `set_active_layer`

Konnect v0.12.0 could write an unsupported `(active_layer "...")` entry into a
board's `(setup ...)` block. KiCad 10.0.6 cannot parse that token because the
active layer belongs to the editor session, not the `.kicad_pcb` document.

Close the board in KiCad, make a backup copy, remove only the injected
`(active_layer "...")` line from the board file, and reopen the board. Do not
remove the surrounding `(setup ...)` block. Later versions, including every
Copperpour release, refuse `set_active_layer` with `unsupported_capability` and
leave the file byte-identical until stable KiCad IPC exposes a native operation with
readback.
