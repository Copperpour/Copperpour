# Contributing to Copperpour

## Before you start

- Read [governance](GOVERNANCE.md) and open an issue for non-trivial changes.
- Keep each pull request focused on one reviewable outcome.
- Follow the [branch and pull request workflow](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md).
  Base independent work on current `upstream/main`. Keep one merge-ready
  PR per dependency chain and rebase the next step after it lands.
- Enable **Allow edits by maintainers** for a fork PR when available. Otherwise,
  the author owns mechanical refreshes. Ambiguous or substantive conflicts
  remain author work.
- Read the [naming conventions](docs/NAMING_CONVENTIONS.md) before changing public names.

## Development setup

```bash
# protoc is required for protobuf code generation (copperpour-ipc crate). The build also
# needs protobuf's well-known .proto files, which some distributions package separately.
# Windows: choco install protoc   (shim installs may also need PROTOC_INCLUDE — see DEV.md)
# macOS:   brew install protobuf
# Debian:  sudo apt install protobuf-compiler libprotobuf-dev
# Fedora:  sudo dnf install protobuf-compiler protobuf-devel

cargo check --workspace
cargo test --workspace --lib --tests
cargo build --release -p copperpour
```

See [DEV.md](DEV.md) for the architecture guide, tool conventions, and how to add a
new tool.

## Pull request shape

Use an imperative title such as `fix(schematic): preserve tab-indented wire blocks`.
Use the [PR template](.github/pull_request_template.md) to describe the problem,
root cause, design, dependencies, unique acceptance criteria, compatibility,
validation, risk, and rollback. Explain how the change uses existing KiCad or
Copperpour capabilities and when any workaround can be retired.

Follow [proportionate scope review](GOVERNANCE.md#proportionate-scope-review)
for proposed expansion. For tool behavior changes, complete the affected rows
of the template's behavior table using the [reliability contract](docs/RELIABILITY_CONTRACT.md).

## From review to merge

Follow [the merge rules](GOVERNANCE.md#merging) and
[review workflow](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md#reviews).
A contributor PR needs a maintainer's approval and required checks on the same
head. Resolve conversations and conflicts before merge. Fold fixes into their
commits and use `--force-with-lease` when rewriting the branch.

For environment-dependent checks, name any missing environment and provide
deterministic and hosted CI evidence. Identify a reporter or tester who can
validate when available. Required CI and material safety evidence remain mandatory.

Keep generated artifacts, personal settings, downloaded catalogs, build output,
and unrelated cleanup out of the diff.

## Negative controls

Reviewers check each guard your change adds: break it, and see which tests fail.
A guard whose removal fails nothing is not pinned by any test, and the review will
ask for one. `scripts/control.sh` runs one such control the way reviews do. Commit
first, then pass a label and the command that breaks the guard:

```bash
CONTROL_TESTS="the_log_is_read_incrementally" scripts/control.sh "fixed log tail" \
  perl -pi -e 's/Some\(offset\) if offset <= len/Some(offset) if false \&\& offset <= len/' \
  crates/copperpour-core/src/autorouting/freerouting/mcp.rs
```

It prints `== <label>: N failed` and the failing tests, then restores the tree. It
tests the edited crates and every crate that depends on them, with `--no-fail-fast`.
`CONTROL_TESTS` limits the run to your new tests, and the full set runs only when
they fail nothing. The script's header lists its options. A table of controls in
the PR body is welcome but not required: reviewers run their own.

## Pull request checklist

- Run the [Rust baseline](AGENTS.md#before-you-push) and the CI jobs affected by
  the diff. See [Testing and Release](docs/TESTING_AND_RELEASE.md) for coverage.
- Verify the branch can merge into current `main`, required checks passed on
  the reviewed head, and the commit list and diff contain only this PR's work.
- For tool changes, follow [Developing Tools](docs/DEVELOPING_TOOLS.md).
  Update the affected row in `tool-directory.md`. At release, `cargo xtask
  fix-doc-counts` updates totals from each toolset's `tools()` list.
- Preserve public API compatibility. Add a separate
  [migration note](docs/api-migrations/README.md) for caller-visible changes.
- If fixture line endings differ locally despite a clean checkout, run
  `cargo xtask check-fixtures`. Its `--fix` option refreshes affected fixtures.

## Licensing your contribution

Copperpour is licensed under the [AGPL-3.0](LICENSE) only, and contributions
are accepted under that same license. You keep the copyright in your work, and
nobody gets a right to relicense it.

By submitting a contribution, you confirm that you have the right to submit it
under the AGPL-3.0. If your employer owns what you write, for example because
you wrote it at work, get its permission first.
