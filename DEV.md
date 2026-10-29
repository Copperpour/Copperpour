# Developer Guide — Copperpour

Implementation reference for the Rust port. New to the codebase? Start with
[docs/DEVELOPER_OVERVIEW.md](docs/DEVELOPER_OVERVIEW.md). Naming, public API, branch
and PR rules: [docs/NAMING_CONVENTIONS.md](docs/NAMING_CONVENTIONS.md). Before
implementing or reviewing a tool, read [the reliability contract](docs/RELIABILITY_CONTRACT.md):
it sets argument, mutation, result and recovery handling, the evidence a behavior change
needs, and the planned catalogue-wide checks. Reuse the existing structured errors,
document-binding, transaction and readback helpers, and document remaining gaps.

## Quick Start

```bash
# protoc is required; without PROTOC, protoc on PATH is used (see Build Requirements).
set PROTOC=C:\path\to\protoc.exe   # or: choco install protoc

cargo check                          # ~15s
cargo test --workspace --lib --tests # all tests
cargo build --release -p copperpour     # MCP server binary
cd crates/schematic-viewer && cargo build --release   # viewer (separate crate)
```

### Nix

`flake.nix` gives a reproducible build and dev shell on Linux (`x86_64`/`aarch64`):
`nix build .#copperpour`, `nix run .#copperpour`, `nix develop` (pinned toolchain,
protoc, kicad-small). The toolchain comes from `rust-toolchain.toml` and the version from
the workspace `Cargo.toml`. Not tracked automatically: `cargoBuildFlags`
(`-p copperpour --bin copperpour`) and `preCheck` (exports `COPPERPOUR_STATE_DIR`).
**Renaming the binary, adding a workspace member the server depends on, or adding a test
that needs another env var breaks only the Nix build** — the `Nix flake` CI job catches
that, so treat its failure as a real break.

## Architecture

```
crates/
├── copperpour/                  # Binary + cdylib
│   └── src/  main.rs (CLI: --config, subcommands) · lib.rs/ffi.rs (C ABI:
│             kicad_plugin_init/version/shutdown) · config.rs (TOML + JSON, socket
│             auto-detect) · transport/stdio.rs (JSON-RPC lines, default) ·
│             transport/http.rs (streamable HTTP: POST + GET/SSE on /mcp)
├── copperpour-core/             # All tool logic
│   └── src/
│       ├── mcp/       protocol.rs (JSON-RPC types) · handler.rs (initialize,
│       │              tools/list, tools/call) · server.rs (session state) · error.rs
│       ├── router/    mod.rs (ToolRouter load/unload) · registry.rs (toolset
│       │              metadata, tools_for()) · meta_tools.rs (meta-tools, Unix reload)
│       └── tools/     one module per toolset (counts: registry.rs, tool-directory.md)
│           ├── mod.rs              ToolDef, ToolContext, tool!, arg helpers, kicad_config_dir()
│           ├── board_source.rs     live-vs-saved board reads + provenance
│           ├── live_board.rs       what KiCad's answer proves about a board (read + write gates)
│           ├── launch.rs           start viewer/KiCad detached; report exit during startup
│           ├── cli.rs              kicad-cli v10 wrapper (verified against the binary)
│           ├── svg_import.rs       SVG parse + Bezier flatten (usvg) for import_svg_logo
│           ├── sch_components.rs   placement with lib_symbols embedding
│           ├── sch_wiring.rs       incl. connect_pins, power symbol embedding
│           ├── sch_analysis.rs     union-find net graph; sch_connectivity.rs = NetGraph
│           ├── sch_batch.rs        single-read/single-write atomic batches
│           ├── sch_hierarchy.rs    typed Sheet model, sheet CRUD, page/title block, pins
│           ├── title_block_args.rs title-block args/response/readback (board + sheet)
│           ├── pcb_sync.rs         update_pcb_from_schematic: pure planner + one-commit IPC apply
│           ├── pcb_footprint_update.rs  library refresh planner + one-commit IPC apply
│           ├── pcb_board.rs        S-expr editing, IPC fallback, SVG logo
│           ├── pcb_components.rs   IPC real-time + safe headless single placement
│           ├── footprint_{graphics,metadata,models}.rs  validated atomic footprint edits
│           └── project, sch_bus, sch_export, pcb_routing, pcb_export, library,
│               integration, verification, config, design_review, templates,
│               manufacturing, placement, editor_navigation
├── copperpour-sexp/             # S-expression engine, no KiCad dependency
│   └── src/  parser.rs (nom) · writer.rs (SexpEdit, apply_edits, write_atomic) ·
│             schematic.rs (SymbolInstance, LibPin, pin_endpoint) · geometry.rs
│             (CANONICAL pin math) · net.rs (KiCad-10 vs legacy net nodes) · layers.rs ·
│             command.rs (reversible edits) · transaction.rs (multi-file journal) ·
│             title_block.rs · error.rs (SexpError incl. Conflict)
├── copperpour-schematic-editor/ # Typed schematic model: parse → mutate → write
│   └── src/  schematic/ · sexp/ (indent-preserving, mixelpixx/Konnect#210) ·
│             library.rs · types.rs (fmt_f64, …)
├── copperpour-ipc/              # KiCad 10 IPC client
│   └── proto/ (from KiCad v10) · build.rs (prost) · src/ gen.rs · client.rs (NNG
│       req/rep) · builders.rs (protobuf helpers, mm→nm) · transform.rs (footprint
│       children are ABSOLUTE board coords, mixelpixx/Konnect#23) · types.rs
└── schematic-viewer/            # Tauri app, outside the workspace
    └── capabilities/default.json (core:default grant — without it event.listen() is
        silently denied) · src/main.rs (multi-sheet watcher, snapshot-isolated
        incremental kicad-cli SVG render) · frontend/index.html (pan/zoom, sheets)
plugin/      Python launcher inside KiCad: __init__.py (PCB-editor ActionPlugin settings
             dialog) · settings_dialog.py · native_bridge.py (see Plugin) · plugin.json
packaging/   build-pcm.{ps1,sh} · metadata.json · validate-pcm.py · schema/ · resources/
.github/workflows/
    ci.yml        check+test (3-OS matrix), clippy, fmt, viewer, Python plugin, Nix,
                  PCM validation — 9 check runs per PR
    e2e-kicad.yml real KiCad 10.0.5: e2e suite, demo-corpus conformance, PCM; weekly,
                  manual, release gate, opt-in PR label
    release.yml   real-KiCad gate, build (4 targets), PCM (3 platforms, macOS lipo)
```

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
- Multi-file changes use project-local `.konnect-transaction-*.json` write-ahead
  journals holding full before/after images — sensitive project data. Unix creates them
  `0600` regardless of umask; elsewhere keep the project directory private.
- New design files and exports get `0666 & ~umask` on Unix. A replacement keeps the
  destination's mode, so a project an earlier Konnect created stays `0600` until
  `chmod`ed.

`copperpour_schematic_editor::Schematic` separates creation from replacement:
`save(new_path)` is create-only; `save(loaded_path)` and `overwrite()` replace only if
the file still matches the loaded source, so KiCad autosave yields a conflict the caller
resolves by reloading and reapplying. Intentional replacement uses the revision-aware
writer/command APIs — never delete the destination or weaken `save()` into an
unconditional overwrite.

Journals: `copperpour transaction status | recover`, and the force-gated `copperpour
transaction abandon` ([Troubleshooting](docs/TROUBLESHOOTING.md#transaction-recovery-is-blocked-by-divergent-content)).

### Which board a read answers about

A read-only PCB tool answerable by KiCad or the saved file asks `tools/board_source.rs`
instead of choosing itself. It takes `board_source` = `auto` (default) | `live` |
`saved`, and reports `sources` (per domain: `ipc`, `saved_board`, `project_file`,
`derived`, `unavailable`) and `source_evidence` (`board_state`,
`excludes_unsaved_editor_state`, machine-readable `reason`, prose `detail`).

It and the write gates in `pcb_board.rs` both classify KiCad's answer through
`tools/live_board.rs` — including that a board this process watched KiCad hold is never
treated as safely absent later, however KiCad stops answering. The policies differ (a
stale read is disclosable, a stale write destroys work); the evidence reading does not.

The rule: once KiCad positively identified the board as live, a failed query **is
returned as a failure**, never as an answer read off a possibly older file. A watched
board whose IPC endpoint disappears is refused under `auto`; explicit `saved` may still
read the snapshot and says so in `reason`. First consumers: `get_layer_list`,
`get_netclasses`; inventory in [#58](https://github.com/Copperpour/Copperpour/issues/58).

### kicad-cli v10 (subprocess)

Verified: `sch erc`, `sch export svg/pdf/bom/netlist`, `pcb drc`,
`pcb export gerbers/drill/pdf/svg/step/vrml/pos/ipcd356`, `pcb render`. Removed in v10:
`sch annotate` (reimplemented in Rust), `pcb sync`, `pcb export/import specctra`.
Version format: `20250610`.

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
| `invalid_argument` | Required argument missing/malformed |
| `file_not_found` | Referenced file or project-discovery directory missing or unreadable |
| `conflict` | File changed, a write would replace existing paths, or schematic project ownership is not provably unique — carries the paths; ownership conflicts include the schematic dir and all candidate roots |
| `wrong_document` | Requested PCB is not among KiCad's positively identified open documents — carries `requested`, `open_documents`; path-bearing IPC never substitutes another board |
| `ambiguous_target` | Several observed UUIDs, reference/units, fields, instances or PCB identities match, or instance records conflict across projects, units or hierarchy paths — carries `target`, stable `candidates`; the caller must choose |
| `stale_target` | A bound PCB is no longer uniquely open, saved instance metadata disagrees with the proven hierarchy, or prospective state lacks the expected document, UUID/reference, unit, library, hierarchy, position, rotation or property — carries `target`, `reason`; refused before writing |
| `mutation_outcome_uncertain` | Edit committed but readback could not prove it — carries `operation`, `path`, `reason`; reload and inspect the file (or live board) before retrying |
| `ambiguous_open_board` | KiCad's open-document list is not a complete set of comparable board identities — carries `path`; neither live nor file path may run |
| `invalid_configuration` | `config.json` or `.copperpour/project.json` exists but is unusable — carries `path`, `reason` (`malformed_json:` / `unreadable:`); defaults are never substituted, since the next save would overwrite the user's settings |
| `handler_error` | Catch-all for unmigrated `anyhow::Error` |

In a handler:

```rust
if !path.exists() {
    return Ok(CallToolResult::error_kind(
        ToolErrorKind::FileNotFound { path: path.display().to_string() },
        format!("Project file not found: {}", path.display()),
    ));
}
```

New kind: add the variant in `mcp/error.rs` and its `short_code()` arm;
`short_code_matches_serialized_kind_field` fails if they drift.

Dispatch errors (not-loaded, unknown, panic) and **every input-schema error** are
structured, by three mechanisms in call order:

1. **Schema gate.** Every advertised Draft 2020-12 schema is compiled with its tool
   definition, cached, and reused by `handler.rs::dispatch_tool`. Domain and meta-tool
   calls are refused before the handler for missing required fields, wrong types,
   fractional integers, failed bounds, or unknown keys in a closed object, as
   `invalid_argument` naming nested fields like `components[0].unit`. The legacy
   presence check runs first so a missing field keeps its schema order and message;
   explicit `null` counts as absent.
2. **Typed helpers** in `tools/mod.rs` (`require_str`, `require_f64`, `require_array`,
   `require_u64`, `opt_u32`, `opt_positive_f64`, domain validators) return
   `InvalidArgument { field, reason }`, so direct handler calls — most tests — refuse
   the same way.
3. **`get_path`** returns `anyhow::Result<PathBuf>` (171 call sites use `?`) with a
   `MissingArgument` marker that `dispatch_tool` downcasts. Classify by downcasting,
   never by message text — as with `copperpour_ipc::TransportUnreachable`.

A path that is *present but unusable* (missing on disk, wrong extension) is **not** an
argument error: it stays `handler_error` or `FileNotFound`, so "forgot an argument" and
"file is not there" remain distinguishable.

The gate exists because an audit found 25 sites in 18 tools reporting success on
substituted values, and wrong-typed options, fractional integers, bounds and closed
nested objects still slipped through. Catalogue tests compile every schema and drive real
tools through dispatch, including a KiCad footprint fixture that must stay byte-identical
on refusal. Most in-handler errors still use `CallToolResult::error("…")` or bubble
`anyhow::Error`; migration is incremental (`project.rs::handle_get_project_info` shows
the pattern).

### Unrepresentable layers crash KiCad

`builders::layer_from_name` maps unknown names to `BL_UNDEFINED`. KiCad 10.0.5 does
**not** validate that field on incoming items; it indexes its layer bitset with it and
dies with an access violation, losing the unsaved board, while Copperpour sees only an
NNG timeout. Any path sending a layer to KiCad must use **`builders::try_layer_from_name`**,
which refuses. `client.rs::build_footprint_item` validates the root, every pad and every
graphic layer before building a child; KiCad's own `*.Cu`/`*.Mask`/`*.Paste` wildcards
are expanded, not mapped. The infallible version is only for read paths that filter
`BL_UNDEFINED`. "No error returned" and "the editor is still alive" are different
questions.

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

`tools/list` does not expose all 244 tools (251 tools with the 7 meta-tools, ~23K tokens):

- **Startup** loads only `registry.rs::STARTER_KIT` (`project`, `config`): with the
  meta-tools, 21 tools ≈ 2K tokens.
- **On demand**: `list_toolboxes` → `load_toolset(name)`; `unload_toolset(name)` prunes.
  Each sends `notifications/tools/list_changed`.
- **Recovery**: calling an unloaded tool returns an error naming its toolset, so the LLM
  loads and retries in one hop.
- **`auto_load_toolsets`** (default `false`): a miss in `dispatch_tool` loads the
  toolset and runs the call in the same hop. Toolsets then accumulate for the session (a
  call reloads what `unload_toolset` pruned), so it is off by default. Set in
  `konnect.toml`/`settings.json` or `ServerConfig`.
- **`eager_toolsets`** (default `false`): `ToolRouter::load_all` at startup, so the
  *first* `tools/list` is the full catalogue (~25K tokens). For clients that cache the
  first list and ignore `list_changed`: they can never call a tool missing from it, since
  `load_toolset` returns names, not schemas. `auto_load_toolsets` does not help there —
  it fires on a call, which needs the name already.

Router: `crates/copperpour-core/src/router/mod.rs`.

## Build Requirements

- Rust pinned by [`rust-toolchain.toml`](rust-toolchain.toml) (1.96.0); CI uses the same.
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
- Schematic viewer: Tauri 2 prerequisites (WebView2 on Windows, usually present). At
  runtime it finds `kicad-cli` in the standard install paths, then PATH, or
  `--kicad-cli <path>`. On Windows, if `cargo` is missing in a fresh shell, run
  `set PATH=%PATH%;%USERPROFILE%\.cargo\bin`, and close the viewer before rebuilding (a
  running `.exe` is locked, so linking fails).

## Test Suite

Run all: `PROTOC=<path> cargo test --workspace --lib --tests`

| Location | What |
|----------|------|
| `copperpour-sexp` unit | Parser, writer, geometry transforms |
| `copperpour-core` unit | Router, starter kit, registry invariants, observability, error taxonomy, arg helpers |
| `copperpour-core` integration | Fixtures: parse, edit, write, observability, structured errors |
| `copperpour-schematic-editor` | Typed model + round-tripping |
| `copperpour-ipc` unit | Protobuf builders, client messages, rigid-body child transform |
| `crates/copperpour/tests/` | stdio protocol, doc/asset count guards, transaction CLI, `#[ignore]`d live-KiCad tests |

`schematic-viewer` is in `[workspace] exclude`, so `--workspace` never builds it; CI's
`viewer` job runs its `cargo check` and `cargo test`. Locally:
`cd crates/schematic-viewer && cargo test`. Its unit tests cover the pure logic
(`walk_sheet_tree`, `compute_watch_dirs`, `snapshot_tree`, `drain_until_quiet`,
`files_needing_render`, `render_all` errors); the `kicad-cli` call and Tauri plumbing stay
thin and untested, like other `kicad-cli` callers.

## Adding a New Tool

`ToolDef::new` closes fixed input records before the schema is advertised and compiled.
Declare every accepted field in `properties`, including fixed nested records.
Caller-keyed maps must set `additionalProperties: true` or a value schema, and a new one
is registered in `fixed_records_are_closed_and_only_reviewed_maps_are_extensible` with a
compatibility test; a typed map's fixed value records stay closed. Untyped caller data
(e.g. configuration `value`) stays unrestricted. Meta-tools follow the same policy. Test
typo/no-write regressions through served dispatch; direct handler calls still need
checked readers for domain rules.

1. Add the `tool!(...)` definition to the toolset's `tools()` vec.
2. Write the `async fn handle_*()` below it.
3. Update `tool_count` in `registry.rs::ALL_TOOLSETS` (shown by `list_toolboxes`).
4. If it belongs in the default set, add its toolset to `STARTER_KIT`.
5. `cargo check`, then `cargo xtask fix-doc-counts` and re-run the tool-directory
   extraction (see the `tool-directory.md` header).

## Current Stats

- **21 toolsets, 244 tools** + 7 meta-tools (4 routing + 2 observability + 1 runtime
  diagnostic — see `tool-directory.md`). The standalone Unix executable adds
  `reload_server` over stdio only — not embedded, HTTP, mixed transport or Windows.
- Baseline `tools/list`: 21 tools / ~2K tokens. Full catalogue: 251 tools (244 registered + 7 meta) / ~25K tokens.
- **0 IPC stubs**, **0 unimplemented tools**.
- **Specctra DSN/SES are PCB-editor operations**, not `kicad-cli` commands:
  `export_specctra_dsn` makes a revision-bound job from the live editor,
  `route_specctra_dsn` delegates to Freerouting's local native MCP server, and
  `plan_specctra_ses_import` / `apply_specctra_ses` validate and apply the result in one
  KiCad undo transaction.
