# Copperpour agent instructions

Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup and review,
[DEV.md](DEV.md) for implementation,
[the reliability contract](docs/RELIABILITY_CONTRACT.md) for tool behavior,
and [the naming conventions](docs/NAMING_CONVENTIONS.md) for identifiers and API names.

## Definition of done

A PR is done when it meets [the pull request checklist](CONTRIBUTING.md#pull-request-checklist).
Test what the PR changes, not every corner case
([Tests and fixtures](CONTRIBUTING.md#tests-and-fixtures)). The `PR shape` CI job
checks the issue reference and the migration note.

## Before you push

Run the checks for the changed paths in [.github/workflows/ci.yml](.github/workflows/ci.yml).
Use the following commands for the Rust baseline.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test -p copperpour-core --locked reliability_contract
cargo test --workspace --locked --lib --tests
cargo test --workspace --locked --doc
```

## Public API

Preserve MCP tool and toolset names, arguments, CLI flags, environment variables,
config keys, JSON keys, and documented paths. For a public rename or repurpose,
provide an alias or deprecation period and a [migration note](docs/api-migrations/README.md).
Update tool rows in [tool-directory.md](tool-directory.md) when tools change.
Counts move at release.

## Schematic nets

Compare `NetGraph` roots with `root_at` in `tools/sch_connectivity.rs`.
One root can have several names. Use `name_of_root` and `net_at` for reporting.
`seed_net_graph` joins points by geometry, then `merge_named_nets` joins points
by name. For scalar names on a saved sheet, `driver_priority` ranks global labels,
global power symbols, local power symbols, local labels, and hierarchical labels in
that order. Ties use ascending name order. The mixed-power and label cases were
checked against KiCad 10.0.6 and 10.0.8 netlists.

## File writes

Use [revision-checked writes](DEV.md#s-expression-file-editing-schematic-offline).
On `SexpError::Conflict`, reload and reapply. Preserve create-only `save()`
for new paths and keep the destination in place.

`SexpError::Conflict` proves the writer stopped before replacement.
`SexpError::ReadbackMismatch` follows replacement and maps to
`mutation_outcome_uncertain`. Propagate it or use `tools::readback_mismatch_result`.

Use `parse_sexp` and `parse_at` for post-write readback. Follow
`sch_wiring.rs::observed_no_connect_moves` to avoid per-item stderr output from
`Schematic::from_source`.

## Parallel sessions

Give each concurrent task its own git worktree and `CARGO_TARGET_DIR`.
