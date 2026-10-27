# Contributing to Copperpour

Thanks for your interest! Bug reports, feature requests, and pull requests are welcome.

## Before you start

- Read [GOVERNANCE.md](GOVERNANCE.md) — who maintains what, how work is claimed,
  and what has to be true before a change lands.
- For anything non-trivial, open an issue first so we can agree on the approach before
  you invest time.
- Keep each pull request focused on one reviewable outcome. Split unrelated platform,
  protocol, feature, and documentation changes into a short PR series.
- Follow the [branch and pull request workflow](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md).
  Independent changes branch from current `upstream/main`; dependent work exposes one
  mergeable step at a time instead of opening cumulative PRs against the same old base.
- For a pull request from a fork, leave **Allow edits by maintainers** enabled for the
  PR branch. This lets a maintainer rebase a reviewed PR and resolve mechanical
  conflicts caused by intervening `main` changes. Ambiguous semantic conflicts and substantive changes still return to
  the author. If maintainer edits cannot be enabled, the author owns that refresh.
- Read [docs/NAMING_CONVENTIONS.md](docs/NAMING_CONVENTIONS.md) before adding public
  tools, schema fields, CLI options, environment variables, or user-facing terms.

## Branch and pull request workflow

The PR base and dependency structure are part of the change:

- Branch independent changes separately from current `upstream/main`.
- Use the latest `upstream/main`, not the latest release tag. Release tags are
  consumption points, not contribution bases, unless a maintainer explicitly
  requests a backport.
- If PR B genuinely depends on PR A, document the complete order, keep only A ready,
  and retain B in the contributor's fork until A merges. Then rebuild B on current
  `upstream/main` with only B's unique commits and rerun CI.
- Do not open several cumulative PRs against `main` from branches that all contain the
  same unmerged prerequisite commits. Green checks on those old cumulative heads do
  not establish merge readiness after `main` changes.
- Use a short-lived `integration/<topic>` branch only with maintainer agreement for a
  tightly coupled program. Child PRs target that branch; one terminal, fully validated
  PR merges the combined result into `main` while unrelated work continues normally.
- Resolve conflicts in the topic branch before final review. When rewriting a published
  branch, use `--force-with-lease`, never plain `--force`.

The linked workflow includes exact commands, fork limitations, restacking examples,
merge-readiness criteria, and integration-branch ownership and cleanup rules.

## Development setup

```bash
# protoc is required for protobuf code generation (konnect-ipc crate). The build also
# needs protobuf's well-known .proto files, which some distributions package separately.
# Windows: choco install protoc   (shim installs may also need PROTOC_INCLUDE — see DEV.md)
# macOS:   brew install protobuf
# Debian:  sudo apt install protobuf-compiler libprotobuf-dev
# Fedora:  sudo dnf install protobuf-compiler protobuf-devel

cargo check --workspace
cargo test --workspace --lib --tests
cargo build --release -p konnect
```

See [DEV.md](DEV.md) for the architecture guide, tool conventions, and how to add a
new tool.

## Pull request shape

Include a short architectural-fit answer:

> Does this change extend an existing KiCad capability or shared Copperpour module?
> If it introduces a workaround or duplicates behavior, explain why and when it
> can be retired.

Short answers are sufficient; documentation-only changes may say not applicable. This is a design review
signal, not a new requirement to redesign a subsystem or migrate unrelated
legacy behavior before landing a focused fix.

Keep the proposed solution proportionate to the demonstrated problem. Follow
[proportionate scope review](GOVERNANCE.md#proportionate-scope-review) when a
fix would grow into speculative edge-case handling or a broader subsystem.
A maintainer settles that boundary on the issue; you are not asked to implement
every variation. A narrow safe fix can remain separate
from deferred expansion.

When changing a tool's behavior, follow the
[reliability contract](docs/RELIABILITY_CONTRACT.md) and complete the affected
behavior table in the PR template. It covers input/default handling, target and
source, observed results, failure timing, and safe recovery. Apply it to the
change under review; unrelated legacy migrations are separate work. The contract
distinguishes review requirements from automation still being implemented.

Use an imperative title such as `fix(schematic): preserve tab-indented wire blocks`.
The description should state:

1. the user-visible problem and scope;
2. the root cause and chosen design;
3. the base branch, dependencies, and position in any PR series;
4. which commits and acceptance criteria this PR uniquely owns;
5. compatibility or migration effects;
6. tests run, including intentionally skipped environment-dependent checks;
7. risk and rollback notes for file formats, IPC, packaging, or release changes.

## From review to merge

[GOVERNANCE.md](GOVERNANCE.md#merging) has the rules. In short: a contributor's PR
needs a maintainer's approving review of its exact head and green required checks,
and a maintainer merges it with a merge commit. A push after the review needs a new
review. A maintainer may rebase a reviewed PR that has fallen behind and resolve a
mechanical conflict; ambiguous conflicts, copied prerequisite history, failed checks
and substantive changes come back to you. CODEOWNERS routes JLCPCB, EasyEDA and LCSC
changes to @mn89h, who decides them.

You do not need personal access to every supported operating system, KiCad
version, or hardware configuration. For an environment-dependent check you
cannot run, name the missing environment, provide the deterministic and hosted
CI evidence you can, and identify any original reporter or community tester who
may be able to validate it. Maintainers apply the risk-proportionate validation
rule in [GOVERNANCE.md](GOVERNANCE.md); an unavailable secondary observation is
not automatically a blocked pull request, but required CI and material safety
evidence remain mandatory.

Treat MCP tools, schema fields, CLI flags, environment variables, config keys, and
documented paths as public API. Preserve compatibility or provide an explicit
migration. Keep generated artifacts, personal settings, downloaded catalogs, build
output, and unrelated cleanup out of the diff.

## Pull request checklist

These are exactly the commands CI runs — if they pass locally, CI should be green:

- `cargo test --workspace --locked --lib --tests` passes
- `cargo test --workspace --locked --doc` passes
- `cargo clippy --workspace --locked --all-targets -- -D warnings` is clean
- `cargo fmt --all -- --check` is clean
- GitHub reports no conflicts with current `main`, and the required checks passed on the exact head being reviewed
- Every review conversation is resolved; a post-review commit or base refresh requires
  review of the new exact head. An unchanged unique diff may receive a focused refresh
  review; changed behavior requires substantive re-review
- The commit list and diff contain only this PR's unique work; dependencies and stack
  position are explicit
- New names follow [the naming conventions](docs/NAMING_CONVENTIONS.md); public name
  changes include compatibility handling and migration notes
- **Read required arguments through the helpers**, never `unwrap_or`:
  `require_str`, `require_f64`, `require_array`, `require_u64` (each returns a
  structured `invalid_argument` naming the field), or `get_path` for paths.
  A handler that substitutes a default for a schema-required argument runs on a
  value the caller never supplied and reports success — that was 25 sites across
  18 tools. The dispatch refuses a *missing* required argument before
  your handler runs, but only the helpers catch a wrong *type*.
- **Map layers for a KiCAD write with `try_layer_from_name`**, not
  `layer_from_name`. The infallible one returns `BL_UNDEFINED` for a name it
  does not know, and KiCAD 10.0.5 does not validate that field — it faults and
  the editor dies, taking the user's unsaved board. The fallible one
  refuses at Copperpour's boundary, where a refusal is still possible.
- **Fixtures for anything that parses a board or footprint should come from
  real KiCAD output.** A synthetic one passed 67 tests while `flip_component`
  refused every stock demo board, because KiCAD writes
  `(property ki_fp_filters "…")` — no position, no layer — into every footprint
  it places from a library, and the hand-written fixture had no such thing.
- If you added or removed tools: set `tool_count` in `router/registry.rs`, add the
  tool's row to `tool-directory.md`, then run

  ```
  cargo xtask fix-doc-counts
  ```

  which rewrites every count in every document from the registry. Do not edit the
  numbers by hand. Roughly seven files quote them, so two PRs that both add a tool
  used to conflict by construction, and a release that moved the counts conflicted
  with the whole open queue at once. The guard (`doc_tool_counts`) still fails if
  anything is stale; the command is how you satisfy it.
- If a tool's caller can see the change, add a migration note as its own file in
  `docs/api-migrations/` (see its README), not to `docs/API_MIGRATIONS.md`. Every
  PR used to add its note at the top of that one file, so any two open PRs
  conflicted there.

First PR from a fork? CI workflows may sit at "waiting for approval" until a
maintainer approves the run — that's a GitHub setting for first-time contributors,
not a failure on your part.

## Licensing your contribution

Copperpour is licensed under the [AGPL-3.0](LICENSE) only, and contributions
are accepted under that same license. You keep the copyright in your work, and
nobody gets a right to relicense it.

By submitting a contribution, you confirm that you have the right to submit it
under the AGPL-3.0. If your employer owns what you write, for example because
you wrote it at work, get its permission first.
