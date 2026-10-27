# Copperpour naming conventions

Consistent names are part of Copperpour's public API. They help contributors search the
repository, let agents infer related operations, and prevent accidental compatibility
breaks. New code and documentation must follow this guide. Existing public names are
changed only with an explicit compatibility plan.

## Product and protocol names

Use the official spelling in prose, comments, errors, and UI text:

| Use | Do not introduce | Notes |
|---|---|---|
| `Konnect` | `konnect` as a product name | Lowercase remains correct for binaries, crates, commands, and paths. |
| `KiCad` | `KiCAD`, `Kicad` | Applies to NEW prose only — the existing codebase uses `KiCAD` widely (~400 occurrences) and is not to be mass-renamed; match surrounding style when editing existing text. |
| `MCP` | `Mcp` in prose | Rust type names use `Mcp`, for example `McpHandler`. |
| `IPC` | `Ipc` in prose | Rust type names use `Ipc`, for example `KiCadIpcClient`. |
| `PCB`, `ERC`, `DRC`, `BOM` | mixed-case variants in prose | Rust identifiers treat each acronym as a word. |
| `JLCPCB`, `LCSC` | informal abbreviations | Part IDs are called `LCSC IDs`. |
| `S-expression` | `S-Expression`, `sexp` in prose | `sexp` remains correct in Rust module and function names. |

Prefer the domain's exact term: `footprint` for a PCB instance or library item,
`symbol` for a schematic item, `reference` for `R1`, and `value` for `10 kΩ`. Use
`component` only for concepts that intentionally span symbols and footprints.

## Rust

Follow the Rust API Guidelines and `rustfmt`, with these repository-specific choices:

- Crates and Cargo packages use `kebab-case`: `konnect-core`, `konnect-ipc`.
- Modules, files, functions, methods, variables, and fields use `snake_case`:
  `pcb_components`, `ensure_board_is_active`, `ipc_address`.
- Types and traits use `UpperCamelCase`. Acronyms are words: `KiCadIpcClient`,
  `McpHandler`, `IpcFootprint`, `UuidCache`.
- Constants and environment variables use `SCREAMING_SNAKE_CASE`: `HOOK_SKILLS`,
  `KICAD_API_SOCKET`, `COPPERPOUR_STATE_DIR`.
- Boolean names describe a true state with `is_`, `has_`, `can_`, or `should_` when
  the prefix adds clarity: `is_error`, `has_pull_up`.
- Fallible `find_*` functions return `Option` when absence is normal. `get_*` functions
  return `Result` when retrieval can fail. Mutation verbs should be precise:
  `create`, `add`, `update`, `move`, `delete`, `write`, or `replace`.

Handlers are named `handle_<tool_name>`. A tool definition named `place_component`
therefore maps to `handle_place_component` in the same toolset module.

## Public MCP tools and JSON

MCP tool names and toolset names are stable public API:

- Use lowercase `snake_case`: `pcb_components`, `place_component`, `get_board_info`.
- Begin tools with a concrete verb. Prefer `get_` for one object, `list_` for a
  collection, `create_` for a new persisted object, and `set_` for replacement.
- Do not encode the transport or implementation in a tool name unless it changes the
  user-visible contract. Describe IPC or file requirements in the tool description.
- Use the same noun across the tool name, schema, response, docs, and tests.
- Never silently rename or reuse a tool. Add an alias/deprecation period and document
  the migration when a public rename is unavoidable.

Tool arguments and Konnect-owned JSON keys use `snake_case`. Protocol-defined JSON-RPC
and MCP fields retain the specification's spelling, such as `jsonrpc`, `tools/list`,
and `serverInfo`. KiCad plugin manifests retain the KiCad schema's field names.

Collection responses use a plural noun plus `count` when useful:

```json
{
  "count": 2,
  "components": []
}
```

Errors name the failed subject and action. Avoid a bare `not found`; prefer
`footprint 'R17' not found on the active board`.

## Units, paths, and identifiers

Make ambiguous values self-describing:

- Append units to non-domain-obvious values: `_mm`, `_nm`, `_degrees`, `_ms`,
  `_bytes`. Coordinates in established PCB/schematic tool schemas are millimetres;
  document that contract in the schema.
- Use `_path` for a file or unresolved filesystem path and `_dir` for a directory.
  A variable named `board` may be a public tool argument for compatibility; local Rust
  variables should prefer `board_path` when they contain a `Path`.
- Distinguish identifiers: `uuid` for a textual UUID, `kiid` for KiCad's item ID,
  `lcsc_id` for an LCSC part number, `net_code` for KiCad's numeric net code, and
  `reference` for a designator such as `U3`.
- Use `_count` for quantities and `_index` for zero-based positions. Avoid `num` when
  either meaning is possible.

## Files, scripts, and documentation

- Rust and Python source files use `snake_case`.
- Command-line and packaging scripts use `kebab-case`: `build-pcm.sh`,
  `validate-pcm.py`.
- User-facing guides use uppercase names for repository-level conventions
  (`CONTRIBUTING.md`, `DEV.md`) and descriptive uppercase or kebab-case names under
  `docs/`. Do not rename an established guide solely to change its case.
- Tests describe observable behavior in `snake_case`, for example
  `delete_items_surfaces_per_item_failure`.
- Fixtures include the behavior or issue they represent; avoid `test1` and `sample2`.
- Generated protobuf code and vendored protocol definitions keep upstream naming.
  Do not hand-edit generated files.

## Branches, commits, and pull requests

Use a short lowercase branch name with a category and topic:

```text
fix/indent-safe-wire-delete
feat/linux-pcm-support
docs/naming-conventions
```

Pull request titles use an imperative Conventional Commit-style prefix:

```text
fix(schematic): preserve tab-indented wire blocks
feat(ipc): place footprints through typed KiCad messages
docs(contributing): define naming conventions
```

Recommended types are `fix`, `feat`, `docs`, `test`, `refactor`, `build`, `ci`, and
`chore`. Keep each pull request focused on one reviewable outcome. Use the body for
context and issue links rather than packing them into the title.

Commit subjects are imperative, specific, and under roughly 72 characters. Remove
`fixup!`, merge noise, generated build output, and unrelated formatting before review.

## Compatibility checklist

Before introducing or changing a name, check:

1. Is it public in MCP, CLI flags, environment variables, JSON, plugin metadata, logs,
   or documented filesystem paths?
2. Does the same concept already have a name elsewhere in the repository?
3. Can users and agents infer its type, unit, and scope without reading the body?
4. Would a rename require an alias, serde alias, migration note, or deprecation period?
5. Are tests, tool-directory metadata, examples, and error messages updated together?

When compatibility and style conflict, preserve compatibility and document the legacy
name. Consistency is valuable; silently breaking users is worse.
