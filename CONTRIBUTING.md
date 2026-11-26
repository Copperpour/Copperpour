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

## Markdown prose

Aim for 90 columns when adding or editing prose in Markdown files. The
**Markdown prose wrap** CI check annotates added or edited lines as an advisory
during the trial. It skips headings, tables, code blocks, standalone URLs,
inline link destinations and code spans, and the released migration history in
`docs/API_MIGRATIONS.md`. A long bare URL with prose can move to its own line.
For a list item containing only a long URL, use Markdown autolink syntax
(`<https://example.com/...>`).

Use rumdl 0.3.6 to check a file with the same policy as CI. The fix command can
reflow surrounding lines, so review the diff and keep wrapping edits within your change.

```bash
rumdl check path/to/file.md
rumdl check --config 'MD013.reflow = true' --fix path/to/file.md
```

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

## Tests and fixtures

Test what the PR changes, not every corner case:

- One test per behavior the PR adds or changes, and one per new refusal or
  error path. Edge cases need a test only when the issue names them or the
  code branches on them.
- A test asserts what the tool wrote or returned, read back from the result,
  not values echoed from the request
  ([Evidence](GOVERNANCE.md#evidence)).
- A new fixture is needed when the PR changes how a KiCad file is read or
  written, or claims what KiCad does. It is KiCad's own output (saved by
  Eeschema or pcbnew, or resaved with `kicad-cli`), never a hand-written
  file or a Rust string, and gets a `<fixture>.README.md` saying how it was
  made. A narrow inline unit test may sit beside it, not replace it. Other
  changes reuse existing fixtures.
- When the PR claims what KiCad does, show it: a `kicad-cli` netlist, ERC or
  resave of the fixture, or the KiCad source line, quoted in the body.

The reviewer judges whether the tests show the change works. They don't ask
for more cases unless a concrete input breaks the change.

## Negative controls

Reviewers check the main guards your change adds: break one, and see which
tests fail. A guard whose removal fails nothing is not pinned by any test, and
the review will ask for one. Authors don't have to run controls or put a table
of them in the PR body. To try one yourself, `scripts/control.sh` runs it the way
reviews do. Commit first, then pass a label and the command that breaks the guard:

```bash
CONTROL_TESTS="the_log_is_read_incrementally" scripts/control.sh "fixed log tail" \
  perl -pi -e 's/Some\(offset\) if offset <= len/Some(offset) if false \&\& offset <= len/' \
  crates/copperpour-core/src/autorouting/freerouting/mcp.rs
```

It prints `== <label>: N failed` and the failing tests, then restores the tree. It
tests the edited crates and every crate that depends on them, with `--no-fail-fast`.
`CONTROL_TESTS` limits the run to your new tests, and the full set runs only when
they fail nothing. The script's header lists its options.

## Pull request checklist

This is the definition of done for every PR, whoever or whatever wrote it.
The `PR shape` CI job checks the items marked **(CI)**.

1. **Issue reference (CI).** `Closes #N` on a line of its own, or `Part of #N`
   for a partial PR. `Closes` links the issue; `Part of` only refers to it,
   since it is not a GitHub closing keyword. A prose `Issue: #N` line does
   neither. CI checks the body's wording, not GitHub's linked issues. A mechanical change
   (formatting, a dependency bump, a typo) says `No issue: <reason>` instead.
2. **Scope.** The diff does what the issue asks and nothing else. Where it
   departs from the issue's suggested approach, the body says how and why.
3. **Public API.** Tool and toolset names, arguments, CLI flags, environment
   variables, config keys, JSON keys and documented paths keep their names, or
   gain an alias. New names follow the [naming conventions](docs/NAMING_CONVENTIONS.md).
4. **Migration note (CI).** A change a caller can see (a tool's arguments,
   response or behavior) adds a [migration note](docs/api-migrations/README.md),
   or edits the existing note of an unreleased tool. When tool code changes
   with nothing a caller can see, the body says `API migration: none — <reason>`.
   CI checks only tool code (`crates/copperpour-core/src/{tools,router,mcp}/`);
   a change to CLI flags, environment variables, config keys or the plugin
   needs a note too, and review checks it.
5. **Errors.** Failures use a typed `ToolErrorKind` from [DEV.md](DEV.md#structured-errors),
   not `handler_error`. Several matching targets are `ambiguous_target` with
   `candidates` the caller can pass back; a changed file is `conflict`; an
   unproven write is `mutation_outcome_uncertain`.
6. **Writes.** File and IPC writes follow the [reliability contract](docs/RELIABILITY_CONTRACT.md):
   revision-checked writes, the requested board only, and a readback of what
   was written.
7. **Tests.** As in [Tests and fixtures](#tests-and-fixtures), with coverage
   by area in [Testing and Release](docs/TESTING_AND_RELEASE.md).
8. **Docs.** For a tool change, follow [Developing Tools](docs/DEVELOPING_TOOLS.md).
   The tool's description and its `tool-directory.md` row match what
   it now does. Leave the tool counts alone; `cargo xtask fix-doc-counts`
   updates them at release.
9. **Body.** The [PR template](.github/pull_request_template.md), with the
   changed-tool-behavior table for any change a caller can see, the exact
   validation commands and their results, and what was not run.
10. **CI.** The [Rust baseline](AGENTS.md#before-you-push) and the jobs the diff
    affects pass on the exact head, and the branch merges into current `main`.

If fixture line endings differ locally despite a clean checkout, run
`cargo xtask check-fixtures`. Its `--fix` option refreshes affected fixtures.

## Licensing your contribution

Copperpour is licensed under the [AGPL-3.0](LICENSE) only, and contributions
are accepted under that same license. You keep the copyright in your work, and
nobody gets a right to relicense it.

By submitting a contribution, you confirm that you have the right to submit it
under the AGPL-3.0. If your employer owns what you write, for example because
you wrote it at work, get its permission first.
