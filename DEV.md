# Developer Guide — Copperpour

Start with the [developer overview](docs/DEVELOPER_OVERVIEW.md).
For tool changes, use [Developing Tools](docs/DEVELOPING_TOOLS.md),
the [reliability contract](docs/RELIABILITY_CONTRACT.md), and the
[naming conventions](docs/NAMING_CONVENTIONS.md).

## Quick Start

See [build requirements](#build-requirements) for Rust and Protobuf setup.

```bash
cargo check --workspace --locked
cargo test --workspace --locked --lib --tests
cargo test --locked -p copperpour-core -p copperpour --no-default-features --lib --tests
cargo build --release --locked -p copperpour
```

### Nix

On Linux, use `nix build .#copperpour`, `nix run .#copperpour`, or `nix develop`.
Review `cargoBuildFlags` and `preCheck` in `flake.nix` when changing workspace
dependencies, the server binary, or test environment variables. Keep
`dontCargoUseSystemLibs` so the build uses vendored AWS-LC and SQLite.

## Architecture

See [Architecture](docs/ARCHITECTURE.md) for crate ownership and runtime flows.
These modules are common entry points for implementation changes.

| Module | Ownership |
|---|---|
| `copperpour-core/src/tools/board_source.rs` and `live_board.rs` | Live and saved board provenance and editor authority |
| `copperpour-core/src/tools/cli.rs` | KiCad CLI checks and exports |
| `copperpour-core/src/tools/sch_connectivity.rs` | Schematic net identity |
| `copperpour-core/src/tools/pcb_sync.rs` and `pcb_footprint_update.rs` | Pure plans and one-commit IPC apply |
| `copperpour-sexp/src/geometry.rs` | Canonical schematic pin geometry |
| `copperpour-sexp/src/writer.rs` and `transaction.rs` | Revision-checked writes and multi-file journals |
| `copperpour-schematic-editor/src/sexp` | Indent-preserving schematic serialization |
| `copperpour-ipc/src/builders.rs` and `transform.rs` | Protobuf builders, mm-to-nm conversion, and footprint children in absolute board coordinates |

## KiCad 10 Integration

### IPC API (PCB editor, real time)

NNG over IPC sockets (Windows named pipes), protobuf3 `ApiRequest`/`ApiResponse`.
Socket path from `KICAD_API_SOCKET` (set by KiCad for plugins). Full CRUD on board items,
layers and design rules. Schematic-editor IPC is export-only (SVG, PDF, BOM, netlist) —
no item CRUD.

### S-expression file editing (schematic, offline)

- Direct `.kicad_sch` read/write; symbols embedded from KiCad 10's `.kicad_symdir`,
  power symbols from `power.kicad_symdir`.
- Existing-file edits are revision-checked atomic replacements: read the exact source,
  take a cooperative lock, reject any intervening KiCad or Copperpour change, write a
  unique sibling scratch file, fsync, rename.
- Locks live in `COPPERPOUR_STATE_DIR/locks` when that absolute override is set, else
  the platform local-data dir (`konnect/locks`). Reads never create files in the project.
- Writes refuse while KiCad's `~<name>.kicad_sch.lck` exists. It records only user and
  host, so staleness is unprovable: valid, foreign, empty and malformed locks all fail
  closed. Checked before the journal is created and again at the final write.
- Multi-file changes use project-local `.copperpour-transaction-*.json` write-ahead
  journals holding full before/after images. Unix creates them with mode `0600`.
  On other platforms, keep the project directory private. Recovery also accepts
  `.konnect-transaction-*.json` journals.
  `FileTransition::guard` locks and checks a dependency without rewriting it.
  Equal before and after images let recovery detect divergence and stop.
  Resolve a pending journal before replanning a guarded commit.

  `FileTransition::guard_absent` checks that a path stays absent without creating
  a placeholder. Transactions with absence guards use journal version 2.
  Other transactions use version 1. Recover pending version 2 journals with
  a supporting release before downgrading.

  Apply and recovery recheck each replacement target against its before image after
  staging the write, just before renaming it into place. A mismatch leaves the target
  untouched and retains the journal. Earlier replacements are not rolled back. The
  locks coordinate Copperpour writers only. An outside writer can still save between
  the final check and rename, or after the final readback and before journal removal.
- New design files and exports get `0666 & ~umask` on Unix. A replacement keeps the
  destination's mode.

`copperpour_schematic_editor::Schematic` separates creation from replacement:
`save(new_path)` is create-only; `save(loaded_path)` and `overwrite()` replace only if
the file still matches the loaded source, so KiCad autosave yields a conflict the caller
resolves by reloading and reapplying. Intentional replacement uses the revision-aware
writer/command APIs — never delete the destination or weaken `save()` into an
unconditional overwrite.

Journals: `copperpour transaction status | recover`, and the force-gated `copperpour
transaction abandon` ([Troubleshooting](docs/TROUBLESHOOTING.md#transaction-recovery-is-blocked-by-divergent-content)).

### Which board a read answers about

PCB tools that can read either source use `tools/board_source.rs` for
`board_source = auto | live | saved`. The default is `auto`.
It reports per-domain `sources` and `source_evidence`, including whether the
result excludes unsaved editor state. Readers and the write gates in
`tools/pcb_board.rs` share `tools/live_board.rs` for document identity.

Once KiCad positively identifies a board as live, a failed query remains a
failure. If its endpoint disappears, `auto` refuses. Explicit `saved` can read
the snapshot and reports that choice in `reason`.

### kicad-cli v10 (subprocess)

Use `tools/cli.rs` for ERC, DRC, rendering and supported exports.
Schematic annotation runs in Rust. PCB synchronization and Specctra exchange
use the PCB editor path.

### Plugin

Install via the **PCM zip**, to
`C:\Users\<YOU>\Documents\KiCad\10.0\3rdparty\plugins\com_github_copperpour_copperpour\`;
it ships `__init__.py` (SWIG ActionPlugin) and `plugin.json` (IPC exec plugin).
`native_bridge.py` is a KiCad-10-only, opt-in bridge exposing only authenticated status
and native Specctra export on an ephemeral loopback port; the plugin owns and removes
the artifact and the caller cannot choose its path. Do not grow it into a general Python
RPC surface or a KiCad 11 architecture. The Rust exporter stays the default; callers
must choose `prefer` or `require` to use the bridge.

## Structured Errors

Failures are typed by `ToolErrorKind` (`crates/copperpour-core/src/mcp/error.rs`). MCP's
`CallToolResult` has no top-level `data`, so the error rides in the text content as JSON,
with `is_error: true`:

```json
{
  "message": "Tool 'place_component' is in toolset 'pcb_components' — call load_toolset('pcb_components') first, then retry.",
  "error": { "kind": "toolset_not_loaded", "toolset": "pcb_components", "tool": "place_component" }
}
```

Plain clients show `message`, structured ones match `kind`; the observer's `error_kind`
column comes from `extract_error_kind()`, so JSONL logs share the vocabulary.

| `kind` | When |
|--------|------|
| `toolset_not_loaded` | Tool exists but its toolset isn't loaded |
| `unknown_tool` | No toolset has that tool |
| `tool_not_permitted` | `call_tool` may not run the target — carries `tool`, `reason` (`writes_not_enabled`, `never_through_call_tool`); nothing of the target ran, and it stays callable under its own name |
| `invalid_argument` | Required argument missing/malformed |
| `file_not_found` | Referenced file or project-discovery directory missing or unreadable |
| `conflict` | File changed, a write would replace existing paths, or schematic project ownership is not provably unique — carries the paths; ownership conflicts include the schematic dir and all candidate roots |
| `wrong_document` | Requested PCB is not among KiCad's positively identified open documents — carries `requested`, `open_documents`; path-bearing IPC never substitutes another board |
| `ambiguous_target` | Several observed UUIDs, reference/units, fields, instances or PCB identities match, or instance records conflict across projects, units or hierarchy paths — carries `target`, stable `candidates`; the caller must choose |
| `stale_target` | A bound PCB is no longer uniquely open, saved instance metadata disagrees with the proven hierarchy, or prospective state lacks the expected document, UUID/reference, unit, library, hierarchy, position, rotation or property — carries `target`, `reason`; refused before writing |
| `mutation_outcome_uncertain` | Edit committed but readback could not prove it — carries `operation`, `path`, `reason`; reload and inspect the file (or live board) before retrying |
| `ambiguous_open_board` | KiCad's open-document list is not a complete set of comparable board identities — carries `path`; neither live nor file path may run |
| `invalid_configuration` | `config.json` or `.copperpour/project.json` exists but is unusable — carries `path`, `reason` (`malformed_json:` / `unreadable:`); defaults are never substituted, since the next save would overwrite the user's settings |
| `malformed_file` | Referenced KiCad file exists and was read, but its structure is malformed so the operation refused to mutate it — carries `path`, `reason` |
| `handler_error` | Catch-all for unmigrated `anyhow::Error` |

In a handler:

```rust
if !path.exists() {
    return Ok(CallToolResult::error_kind(
        ToolErrorKind::file_not_found(&path),
        format!("Project file not found: {}", path.display()),
    ));
}
```

New kind: add the variant in `mcp/error.rs` and its `short_code()` arm;
`short_code_matches_serialized_kind_field` fails if they drift.

Dispatch compiles and caches each advertised Draft 2020-12 schema. Missing
required fields, malformed values, bounds and closed-object violations return
`invalid_argument` with a field path such as `components[0].unit`. Required
`null` is treated as absent.

Use checked readers from `tools/mod.rs` in handlers, including direct-call
paths. `get_path` returns a typed `MissingArgument` marker for dispatch to
downcast. An argument naming a missing file or unusable path remains a file
or handler error. Classify errors by typed marker, as with
`copperpour_ipc::TransportUnreachable`, and use `project.rs::handle_get_project_info`
as a structured-error example.

### Unrepresentable layers crash KiCad

Use `builders::try_layer_from_name` on writes. Unknown layers map to
`BL_UNDEFINED` in the infallible reader, which can crash the editor and lose
unsaved work if sent over IPC. `client.rs::build_footprint_item` validates
footprint, pad and graphic layers and expands KiCad's `*.Cu`, `*.Mask` and
`*.Paste` wildcards. Read paths that use the infallible mapper must filter
`BL_UNDEFINED`.

## Observability

`McpHandler::execute_tool` wraps every `tools/call` with:

- a ring buffer of the last 100 `CallRecord`s (`get_recent_calls`);
- per-tool counters: totals, errors, duration, last status/error (`server_stats`);
- a JSONL line per call in `<copperpour dir>/logs/calls.jsonl` — Windows
  `%APPDATA%\copperpour`, macOS `~/Library/Application Support/copperpour`, Linux
  `~/.copperpour`;
- `tracing` events `tool_call_start`/`tool_call_end` with `call_id`, `tool`, `toolset`,
  `status`, `dur_ms`.

`CallRecord`: `call_id`, `ts` (unix ms), `tool`, `toolset` (`None` for meta-tools),
`dur_ms`, `status` (`ok`/`error`/`not_found`), `error_kind`, `args_bytes`,
`result_bytes`. Built once by `McpHandler::new` and shared with `ToolContext` for the
meta-tools. JSONL IO failures never fail a call (`tracing::warn!`, dropped). Tests use
`ToolContext::new(...)`, in-memory only. Source: `crates/copperpour-core/src/observability.rs`.

## Tool Routing

`router/registry.rs::STARTER_KIT` loads `project` and `config` at startup.
`auto_load_toolsets` loads a tool's owner on demand when the tool is called.
`eager_toolsets` exposes the full catalogue on the first
`tools/list` for clients that cache that response. Both default to `false`.

See [Tool System](docs/TOOL_SYSTEM.md) for dispatch, list-change notifications,
and [the `call_tool` policy](docs/TOOL_SYSTEM.md#call_tool).

## Specctra export ownership

`export_specctra_dsn` creates a revision-bound job from the live editor.
`route_specctra_dsn` delegates to Freerouting's local MCP server.
`plan_specctra_ses_import` and `apply_specctra_ses` validate and apply the
result in one KiCad undo transaction.

`specctra::prepare_export` captures the import-owned board model without invoking
the Rust DSN writer. `RoutingSnapshot::identity` supplies the existing native
correlator directly and has fixture coverage against the DSN-derived identity.
`export_rust` owns Edge.Cuts outline geometry and flattening
checks. `adopt_native` preserves the actual native DSN and rewrites only its
reverse-manifest identifiers/outline label after validation.

`handle_export_specctra_dsn` keeps one stable IPC snapshot/rules capture, runs
common checks, and selects native first for `prefer`/`require`. It rechecks the
live revision after receiving a native DSN. Only `prefer` falls back; `require` reports
a native-path failure. Custom-rule presence and unsupported reverse-manifest
features stay common refusals. See [the mode contract](docs/KICAD_INTEGRATION.md).

## Build Requirements

- Rust pinned by [`rust-toolchain.toml`](rust-toolchain.toml).
  The pinned version IS the MSRV: bump it deliberately, in its own commit, after the full
  local gate passes on it.
- `protoc`, from `PROTOC` or PATH
  ([releases](https://github.com/protocolbuffers/protobuf/releases)). Well-known-type
  includes (`google/protobuf/any.proto`) come from `<protoc>/../../include` after
  resolving the binary, which fits both `bin/protoc` beside `include/` and
  `/usr/bin/protoc` → `/usr/include`. Set `PROTOC_INCLUDE` when they live apart —
  notably Chocolatey and scoop shims. Debian/Ubuntu need `protobuf-compiler` **and**
  `libprotobuf-dev`, Fedora `protobuf-compiler` and `protobuf-devel`; otherwise the build
  fails with `google/protobuf/any.proto: File not found`.
- Browser schematic viewer: included in the MCP server; needs a browser and `kicad-cli`.
  Configure the server’s `kicad_cli` or let normal CLI discovery find KiCad.
  No separate viewer binary or WebView runtime is required.

## Build Features

`copperpour-core` has a feature for each integration that reaches the network or
an outside program, and `copperpour` forwards them. All are on by default,
and release, PCM and Nix builds use the defaults:

| Feature | Adds | Dependencies |
| --- | --- | --- |
| `jlcpcb` | JLCPCB parts catalog, its download, LCSC and EasyEDA lookups, the order upload | `rusqlite`, `reqwest`, `zip` |
| `freerouting` | Freerouting discovery and headless routing (a Java subprocess) | `zip` (JAR manifest checks) |
| `native-bridge` | KiCad plugin's Specctra export bridge (HTTP to localhost) | `reqwest` |

Cargo features only add code, so the offline build is `--no-default-features`, not
a feature of its own. CI's `offline` job builds, lints and tests it, fails if its
dependency tree has an HTTP client, SQLite or `zip`, or its binaries name an LCSC,
JLCPCB or EasyEDA host, and checks each feature alone. `xtask` keeps the default
features: the documented tool counts are for the default build, and the tests that
check prose against the registry are ignored in other builds.

A tool of a missing feature isn't registered, so a call to it is `unknown_tool`, and
a toolset left with no tools isn't listed. `sourcing::LcscCatalog` stays in every
build: without `jlcpcb` it reports every catalog as missing from the build, so
placement and `estimate_cost` work unchanged.

## Test Suite

Run all: `PROTOC=<path> cargo test --workspace --lib --tests`

| Location | What |
|----------|------|
| `copperpour-sexp` unit | Parser, writer, geometry transforms |
| `copperpour-core` unit | Router, starter kit, registry invariants, observability, error taxonomy, arg helpers |
| `copperpour-core` integration | Fixtures: parse, edit, write, observability, structured errors |
| `copperpour-schematic-editor` | Typed model + round-tripping |
| `copperpour-ipc` unit | Protobuf builders, client messages, rigid-body child transform |
| `copperpour-viewer` unit | Sheet discovery, snapshot rendering, cache selection and event batching |
| `crates/copperpour/tests/` | stdio protocol, doc/asset count guards, transaction CLI, `#[ignore]`d live-KiCad tests |

The viewer engine, browser service and embedded frontend are in the workspace
crate `copperpour-viewer`; workspace checks cover them without GTK, WebKit or a
display. Its HTTP/SSE tests cover the service; `examples/browser_viewer.rs` is
the local development entry point. Browser execution and native renderer smoke
checks remain separate evidence. The MCP launch tool owns viewers through a
registry on `ToolContext` until the server context ends. `open_browser: false`
suppresses the OS opener; successful service starts return a private credential
URL. See [the viewer README](crates/copperpour-viewer/README.md).

## Server-delivered guidance

See [server guidance architecture](docs/ARCHITECTURE.md#server-guidance) for
the shared embedded catalogue, initialization rules, and MCP endpoints. Asset
sources are in `crates/copperpour/assets/skills` and
`docs/RELIABILITY_CONTRACT.md`.

`get_guidance` lists bundle paths or reads one exact path. Markdown links
resolve within the skill's resource directory. The static catalog advertises
`resources: {}` and uses JSON-RPC `-32002` for unknown resources and `-32602`
for malformed requests or continuation cursors.

## Adding a New Tool

Follow the [implementation checklist](docs/DEVELOPING_TOOLS.md#define-and-implement-the-tool).
Keep batches within their domain toolset when they operate on one domain.
Use `sch_batch` for cross-domain schematic operations.

Add or remove the affected row in [the tool directory](tool-directory.md).
`cargo xtask fix-doc-counts` updates totals at release.

## Current Stats

- **26 toolsets, 281 tools** + 9 meta-tools (5 routing + 2 observability + 1 guidance + 1 runtime
  diagnostic — see `tool-directory.md`). The standalone Unix executable adds
  `reload_server` over stdio only — not embedded, HTTP, mixed transport or Windows.
- Baseline `tools/list`: 21 tools / ~2K tokens. Full catalogue: 290 tools (281 registered + 9 meta) / ~25K tokens.
