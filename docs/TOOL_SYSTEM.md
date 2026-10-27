# Tool System

Copperpour exposes always-visible meta-tools plus domain toolsets loaded on demand.
The implementation is split between `crates/konnect-core/src/mcp`, `router`, and
`tools`.

## Definitions And Context

`ToolDef` in `crates/konnect-core/src/tools/mod.rs` is the unit exposed through
MCP. It carries a public name, description, JSON input schema, and async handler.
The `tool!` macro constructs definitions in each domain module.

`ToolContext` carries `ServerConfig`, the shared `ToolRouter`, the call observer,
and shared integration state. Public tool names and schema fields are API; apply
the naming and compatibility rules in `docs/NAMING_CONVENTIONS.md` and
`CONTRIBUTING.md`.

## Toolsets And Meta-Tools

`crates/konnect-core/src/router/registry.rs` declares `ALL_TOOLSETS`, the starter
set, each toolset's metadata, and the `tools_for` mapping. Registry tests compare
declared per-toolset counts with the definitions returned by the domain module.

`router/meta_tools.rs` defines the always-visible discovery, load/unload, and
observability surface. Meta-tools are outside `ALL_TOOLSETS`. `load_toolset`
accepts either one name or an array; a successful change causes
`mcp/handler.rs` to emit `notifications/tools/list_changed`.

## Dispatch

`McpHandler` in `mcp/handler.rs` dispatches in this order:

1. Handle a meta-tool.
2. Handle a loaded domain tool.
3. If `auto_load_toolsets` is enabled, load the owning toolset and retry.
4. If a registered tool is not loaded, return `toolset_not_loaded`.
5. Otherwise return `unknown_tool`.

This distinction lets a client recover from an unloaded tool without confusing
it with a misspelled or removed public API.

## Argument Contracts

`mcp/handler.rs` compiles and caches each advertised Draft 2020-12 schema and
enforces it before invoking a domain handler or meta-tool. This covers required
fields, present-value types, integer semantics, declared bounds, nested unions
and arrays, and unknown keys only where an object declares
`additionalProperties: false`. Failures return structured `invalid_argument`
responses naming the nested field.

Handlers must still use checked helpers from `tools/mod.rs`. Most unit tests and
some internal paths call handlers directly, so dispatch validation is a shared
boundary, not a substitute for domain validation.

Use the appropriate helper, including:

- `require_str`
- `require_f64`
- `require_array`
- `require_u64`
- `opt_u32` for optional 32-bit integers (`2` and `2.0` accepted; fractions refused)
- `opt_positive_f64` for optional positive dimensions
- `get_path`

Do not use an `unwrap_or` default for a schema-required argument. An explicit
empty array is a value; an omitted required array is an invalid request.

## Response And Evidence Contracts

A response must describe what the backend observed or committed, not merely echo
what the caller requested. This rule is especially important across IPC and CLI
boundaries:

- `tools/pcb_sync.rs` compares the post-commit board with the footprint shapes
  it sent and reports or refuses mismatches.
- `tools/cli.rs` derives DRC results from every category KiCad reports rather
  than treating an absent finding in one category as a clean board.
- `tools/design_review.rs` and `tools/manufacturing.rs` require DRC evidence for
  a positive verdict and distinguish "not checked" from "none found."

When a successful write cannot be trusted from the immediate return value,
prefer a bounded read-back or an independently derived result. If the evidence
is unavailable, return an incomplete/diagnostic state rather than a positive
claim.

## Structured Errors

`CallToolResult` does not provide a separate top-level structured error object.
`crates/konnect-core/src/mcp/error.rs` serializes the error detail into text
content while setting `is_error`.

Common error kinds include `toolset_not_loaded`, `unknown_tool`,
`invalid_argument`, `file_not_found`, `conflict`, `stale_target`,
`mutation_outcome_uncertain`, and `handler_error`. The uncertain-mutation kind
means a commit occurred but readback did not prove its result, so the caller
must reload and inspect the named file before retrying. Add a new kind in
`mcp/error.rs` only when callers need a stable new classification; use
`CallToolResult::error_kind` to return it.

## Documentation Coupling

When tools are added, removed, or renamed, update the domain `tools()` list,
`router/registry.rs`, `tool-directory.md`, and the guarded count locations named
by `CONTRIBUTING.md`. Do not repeat catalogue totals in these map documents;
`crates/konnect/tests/doc_tool_counts.rs` enforces the designated sources and
sweeps documentation for stale totals.
