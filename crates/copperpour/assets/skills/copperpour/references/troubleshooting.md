# Troubleshooting and Safe Recovery

Use this reference when a Copperpour tool call fails, returns a partial or
uncertain outcome, or behaves unexpectedly in the client or KiCad.

## Six-Step Recovery Loop

1. **Inspect the complete response.** Classify the failure (`error.kind` or
   status), target, source (`live`, `saved`, or mixed), and mutation outcome
   (`complete`, `partial`, `refused/failed before mutation`, `uncertain after
   possible mutation`, or `check unavailable`) before continuing. Consume
   structured recovery fields when present: the top-level `recovery` block on
   IPC failures (`ipc_failure.recovery` in `check_kicad_ui` and `open_project`),
   with `cause`, `mutation_outcome`, `retry_safe`, `retry_prerequisites`,
   `next_steps` (each marked `agent` or `user`) and an optional `troubleshooting_url`, and
   kind-specific fields such as `board_state`, `drop_confirmed`,
   `browser_launch_error` or `absolute_path`. When an older server omits those
   fields, handle the response conservatively without inventing missing fields
   or guarantees.
2. **Gather read-only diagnostics.** Use `get_installation_info`,
   `get_recent_calls`, `check_kicad_ui`, and target-specific inspection when
   they resolve uncertainty. In `get_installation_info`, read
   `prerequisite_health` for the cached startup or latest refresh result. After
   repairing setup, pass `{"refresh_prerequisite_health": true}` or restart the
   MCP server. For installed guidance drift, use `copperpour status`.
3. **Perform an already-authorized, safe recovery step** when its prerequisites
   are established: for example, call `load_toolset` when the client supports
   dynamic discovery, pass an absolute filesystem path, call `open_board` (from
   `verification`) with the requested `.kicad_pcb` path when no PCB editor holds
   that board, or give the user the returned `url` when `open_schematic_viewer`
   reports `browser_launch_error` with a running loopback service. Avoid
   unnecessary confirmation for read-only diagnostics and already-authorized
   safe steps.
4. **Retry only when outcome and operation semantics establish safety**, after
   correcting the cause. Never loop blindly, replay already-applied batch
   items, or automatically replay recovered/aborted IPC batches
   (`ipc_batch_recovered`).
5. **Escalate cleanly when user intervention is needed.** Explain what happened,
   what changed or remains uncertain, and the exact next action. Include a
   specific command, configuration snippet, or troubleshooting reference when
   useful, and ask only for the choice or action that cannot be resolved from
   available evidence.
6. **Verify recovery** with the relevant fresh observation (`get_installation_info`
   after a restart, `check_kicad_ui` after launching KiCad, or readback after
   reconciliation) before resuming dependent work.

## Safety Boundaries

- **Never bypass safety refusals.** Do not automatically delete lock or socket
  files (`api.lock`, `.lck`), relax file or socket permissions, overwrite
  configuration files, terminate running KiCad/client processes, abandon
  transaction journals, or restart the server to clear state.
- **Preserve local guidance edits.** `copperpour init` can overwrite edited
  skill or hook files. Never run it silently as a repair; recommend
  `copperpour status` and backing up local edits first.
- **Keep outcomes distinct.** Read-only and mutation outcomes are separate;
  unavailable checks never become passes.

## Diagnostic Tools

| Tool | What it establishes | Limits |
|---|---|---|
| `get_installation_info` | Running server version, `installation.binary_on_disk.newer_than_running`, `runtime.working_directory`, `kicad` CLI block, redacted IPC endpoint, and `guidance` drift (`checked`: `"now"` or `"earlier"`). | `newer_than_running: null` means unproven, not current. `guidance` is cached per installed marker and embedded guidance bundle. |
| `get_recent_calls` | Recent tool calls that reached the MCP server process. | Absence from `get_recent_calls` does not prove a specific client failure (the client may filter or block calls before sending). |
| `check_kicad_ui` | Live KiCad IPC reachability, `ipc_failure` reason, and `AS_NOT_READY` status. | A 5-second default timeout can expire while KiCad is still starting. |

## Scenario Playbook

### KiCad item rejections

- For `kicad_api_rejection` or `ipc_outcome_unknown`, look for per-item statuses in `recovery.evidence.rejected_items`. In a failed Specctra cleanup, look under `recovery.evidence.compensating_deletion`.
- `rollback: "confirmed"` records KiCad's acknowledgement that it dropped the named commit. It does not prove the overall board state is unchanged. Inspect the live board before retrying or proceeding with dependent work.

### Setup, Binary, CLI, and Guidance Drift

- **Missing `kicad-cli` ("kicad-cli not found"):** Call `get_installation_info`
  and inspect `prerequisite_health.kicad_cli` for the executable and observed
  reason. Follow its guidance for a missing, unexecutable, timed-out, or
  unsupported CLI. Restart after configuration or launch environment changes.
- **Stale serving binary:** If `installation.binary_on_disk.newer_than_running`
  is `true` (or expected newer behavior is missing), tell the user to restart
  the MCP client (and KiCad if KiCad owns the server), then verify with
  `get_installation_info`.
- **Outdated installed guidance:** When `initialize` or `get_installation_info`
  reports `out_of_sync` or `different`, explain that upgrading the binary does
  not update installed files. Recommend running `copperpour status` (with the
  matching `--client` or `--skills-dir`) and saving any local edits before
  running `copperpour init`.
- **Relative path `file_not_found`:** Check `absolute_path` in the error and
  `runtime.working_directory` in `get_installation_info`. Retry with an
  absolute filesystem path.

### IPC, Endpoints, and KiCad UI State

- **Prerequisite health:** Check `prerequisite_health.ipc` and its timestamp.
  A configured endpoint can be unreachable. Follow the reported guidance
  when evidence is unavailable, and preserve uncertainty about the cause.
  A responding endpoint can still lack a ready PCB Editor or open PCB
  document. Verify the exact board before calling live tools.
- **Wrong board, no PCB editor, or "KiCAD must be running with the board loaded":**
  Load `verification` and call `open_board` with the exact `.kicad_pcb` path
  first so Copperpour finds an existing `pcbnew` endpoint holding that board or
  launches PCB Editor and binds its IPC endpoint. Only if `open_board` cannot
  reach the board (for example, IPC API disabled or no display), ask the user to
  open the exact `.kicad_pcb` in PCB Editor with KiCad IPC enabled. Do not
  substitute another open board or edit the saved file.
- **Ambiguous endpoints / stale `api.lock`:** When `ipc_failure` lists multiple
  candidate sockets, report the candidates and ask the user to either close
  extra KiCad instances or set `ipc_address` (from KiCad Preferences ->
  Plugins). Never delete `api.lock` or socket files.
- **IPC permission failures:** Report the socket/plugin permission error and ask
  the user to fix permissions or KiCad IPC settings.
- **Persistent `AS_NOT_READY`:** Distinguish the two common causes rather than
  treating them as interchangeable:
  - KiCad is still loading the project/editor: wait briefly and retry
    `check_kicad_ui`.
  - A modal dialog (first-run wizard, confirmation, or message box) is blocking
    the UI: ask the user to close the dialog in KiCad, then re-check with
    `check_kicad_ui`.

### Schematic Viewer Opener Failure

- When `open_schematic_viewer` reports `browser_launch_error` (or is called with
  `open_browser: false` in a headless/remote environment) while returning a
  valid `url`, the loopback service is running. Present
  `[Open schematic viewer](<url>)` to the user (and mention loopback port
  forwarding for remote sessions) rather than treating the call as failed.

### Tools Missing from the Client

1. If the client supports dynamic tool discovery, call `list_toolboxes` and
   `load_toolset` for the needed toolset.
2. Check `get_recent_calls` to see whether calls reached the server. Remember
   that absence from `get_recent_calls` does not prove a specific client failure.
3. Suggest client-specific configuration only when the client is known (for
   example from `initialize` `clientInfo` or user context). Setting
   `eager_toolsets: true` helps clients that cache the initial `tools/list`,
   but it is not a universal fix for clients with hard tool-count limits.

### Configuration, Locks, and Uncertain Mutations

- **`invalid_configuration`:** Report the exact config path and validation
  error. Ask the user to fix or confirm the change; never overwrite config
  files automatically.
- **Schematic editor locks (`conflict`):** KiCad holds an editor lock or
  unsaved state on the sheet. Ask the user to save/close in KiCad or reload
  before retrying. Never delete lock files.
- **Divergent transaction journals:** Transaction recovery refused because the
  target file changed outside the transaction. Preserve both the file and the
  journal, inspect the differences, and ask the user how to reconcile. Never
  abandon or delete the journal automatically.
- **`unsafe_file_fallback`:** Live KiCad IPC disappeared while live state may
  differ from disk. For mutations, do not fall back to saved-file tools or
  hand-edits; reconnect KiCad and verify live state. On read-only tools taking
  `board_source`, pass `board_source: "saved"` if the user explicitly wants to
  inspect the last-saved snapshot while disclosing excluded unsaved edits.
- **`ipc_outcome_unknown` (`board_state: unknown`, `retry_safe: false`):** Stop
  automatic retries. Inspect the live board in KiCad and reconcile what was
  applied before repeating any mutation. Keep the full diagnostic, which
  names the IPC command whose outcome is unknown.
- **`ipc_batch_recovered` (`board_state: unchanged`, `drop_confirmed: true`,
  `retry_safe: false`):** The aborted batch was dropped, not published. Do not
  automatically replay it; inspect the live board and run a fresh dry run
  before deciding whether to apply again.
- **`mutation_outcome_uncertain` (`operation`, `path`, `reason`):** The file
  at `path` may have been replaced or partially updated. Preserve the target
  and diagnostic, read back and inspect the file, and retry only unapplied
  items once state is confirmed.

## Examples

### 1. Successful Safe Recovery (Viewer Opener Failure)

- **Observed:** `open_schematic_viewer` returns `browser_launch_error` because
  no desktop opener is available, alongside a valid `url`.
- **Classification:** Read-only service started; browser launch failed, `url` is
  usable.
- **Action:** Do not retry with `open_browser: true` or treat the schematic
  session as blocked. Share `[Open schematic viewer](<url>)` with the user
  (noting loopback port forwarding if remote) and continue the schematic task.

### 2. Setup Failure Requiring User Action (Stale Binary and Guidance Drift)

- **Observed:** A newly added tool or behavior is missing. `get_installation_info`
  shows `installation.binary_on_disk.newer_than_running: true` and `guidance`
  status `out_of_sync`.
- **Classification:** Setup/process state mismatch; no design mutation occurred.
- **Action:** Do not restart the server or run `copperpour init` silently.
  Explain that the running MCP server is older than the binary on disk, ask
  the user to restart the MCP client, and note that they can run
  `copperpour status` (and back up any local skill edits) before running
  `copperpour init`. After they restart, call `get_installation_info` to verify
  the new running version.

### 3. Uncertain Mutation Requiring Inspection Before Retry

- **Observed:** A PCB IPC mutation or sync fails with `ipc_outcome_unknown`
  (`board_state: unknown`, `retry_safe: false`).
- **Classification:** Uncertain after possible mutation; live board may contain
  all, some, or none of the changes.
- **Action:** Stop retries immediately. Preserve the full error, ask the user
  to inspect the live board in KiCad (or inspect via read-only queries once
  IPC is healthy), reconcile what landed, and only then issue a fresh request
  for any remaining unapplied changes.
