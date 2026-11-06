# Developing Tools

Use this checklist when adding or changing an MCP tool. Start with
`CONTRIBUTING.md`, `docs/NAMING_CONVENTIONS.md`, and the related handler module;
the safest implementation usually follows an existing neighboring tool.

## Choose The Backend And Gate

| Operation | Preferred path | Required safety gate |
|---|---|---|
| Saved schematic mutation | `copperpour-schematic-editor` or `copperpour-sexp` | Revision-aware atomic write; transaction journal for multi-file changes |
| Live board mutation | `copperpour-ipc` | `KiCadIpcClient::ensure_board_is_active` before sending the mutation |
| Board mutation with a safe closed-file implementation | IPC first, file fallback | `attempt_ipc_write` in `tools/pcb_board.rs`; fallback only when IPC is unreachable |
| Board file mutation with no IPC implementation | `copperpour-sexp` or focused parser/edit code | `refuse_if_board_open_in_kicad` before touching the file |
| KiCad-supported check or export | `kicad-cli` through `tools/cli.rs` | Validate exit status, output existence, and parsed result coverage |

Do not fall back to a file edit after a request reached KiCad and was rejected or
timed out. `copperpour-ipc` classifies that differently from an unreachable
transport because KiCad may already hold or have changed the target document.

## Define And Implement The Tool

1. Add the definition to the related `tools()` vector under
   `crates/copperpour-core/src/tools`.
2. Implement the handler near related handlers and reuse their target checks and
   response conventions.
3. Use valid Draft 2020-12 JSON Schema, list every required field, declare
   bounds, and set `additionalProperties: false` only when the object is
   intentionally closed. The catalogue compiles and caches this schema; MCP
   dispatch enforces exactly what is advertised.
4. Read required values with `require_*` or `get_path`, and malformed-present
   options with a checked optional helper. Dispatch validation does not replace
   direct-handler validation. Never turn a malformed present value into an
   omitted value or silent default.
5. Validate the target and all preconditions before the first mutation.
6. Use `CallToolResult::json`, `text`, `image`, or `error_kind` as appropriate.
7. Update `router/registry.rs`, `tool-directory.md`, and the guarded documentation
   named by `CONTRIBUTING.md`.

For an IPC write that accepts a layer name, use `try_layer_from_name` so an
unknown layer is refused before it becomes an invalid KiCad message.

## Evidence Rules

Treat success, counts, and verdicts as claims that require evidence.

- Derive response fields from the parsed result, committed state, or read-back;
  do not populate them from the request merely because the call returned.
- A positive review/manufacturing verdict requires every prerequisite check. If
  a required check could not run, return an incomplete result with the missing
  evidence named.
- Preserve all categories returned by an external tool. The DRC parser in
  `tools/cli.rs` must retain design-rule violations, unconnected items, and
  schematic parity because each category can independently make a board fail.
- After a risky IPC transformation, compare a bounded post-state with what was
  sent. `tools/pcb_sync.rs` is the reference for a commit plus read-back check.

These rules address failures where the transport and KiCad both returned
success but the resulting board was wrong, and where a verdict appeared clean
only because one result category was ignored.

## Errors And Compatibility

Use stable structured errors for caller-actionable failures: wrong arguments,
missing files, conflicts, inactive toolsets, and similar conditions. Do not
classify failures by matching display strings when a typed marker or enum can
cross the layer boundary.

Treat MCP tool names, schema fields, CLI flags, config keys, environment
variables, and documented paths as public API. Renames require compatibility or
an explicit migration note.

## Tests

Add the lowest-level test that can prove the risky behavior, then add handler or
protocol coverage when the public contract is involved:

- parser/writer logic in `copperpour-sexp`;
- typed model behavior in `copperpour-schematic-editor`;
- handler and response behavior in `copperpour-core`;
- IPC message construction/classification in `copperpour-ipc`;
- MCP/CLI behavior in `crates/copperpour/tests`.

Fixtures that parse boards, footprints, symbols, or KiCad reports should come
from real KiCad output. Synthetic fixtures are useful for narrow grammar tests,
but they omit optional or positionless forms that real KiCad emits. Minimize a
real fixture only after the failing shape remains represented.

For a mutating tool, cover the refusal path before the success path: wrong open
board, reachable IPC rejection, stale file revision, unsupported geometry, or
missing evidence. For IPC behavior that mocks cannot prove, add an ignored live
test and record the live validation command in the PR.

## Adding A Vendor Integration

Code that reaches a vendor's service, or a program outside Copperpour, goes
behind a Cargo feature of its own in `crates/copperpour-core/Cargo.toml`. It is
on by default and forwarded by `crates/copperpour/Cargo.toml` (#210). Mark its
dependencies `optional` and enable them from the feature. Gate its module and
its `tool!` entries with `#[cfg(feature = "...")]`, and gate tests that call its
tools. Without it, its tools aren't registered. Code that runs in every build
reads the integration through a seam that reports it missing, as
`sourcing::LcscCatalog` does, rather than through `cfg` at each call site.

CI's `offline` job builds, lints and tests with `--no-default-features`. It also
checks each feature alone. Add the new feature to that loop. If it brings an
HTTP client or names a vendor host, it can't be in the offline build.

## Adding A Fab House

A fab house that differs only in limits or prices is one `FabProfile` in
`PROFILES` (`crates/copperpour-core/src/fab/mod.rs`): its `fab_house` id, its
minimum trace width, a `CostModel`, and `AssemblyFormat::KicadNative`. Give the
`CostModel` a `source` naming where its prices come from and when; without one,
`estimate_cost` marks the estimate approximate. Name the new id in the
`fab_house` descriptions of `export_manufacturing_package`,
`validate_for_manufacturing` and `estimate_cost`, and extend
`profiles_hold_each_fab_houses_limits_and_prices`.

Assembly output of its own, as JLCPCB has in `fab/jlcpcb`, is a new
`AssemblyFormat` variant with a module beside `fab/jlcpcb`, and an arm in each
of its methods. That changes a public tool's output, so it needs tests for each
step and a migration note.
