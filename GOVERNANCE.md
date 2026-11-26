# How Copperpour is run

One page, so nobody has to reconstruct this from issue threads.

## Maintainers

- **[@pauliuszaleckas](https://github.com/pauliuszaleckas)**, lead maintainer.
  Decides scope, releases, licensing, and anything not settled below.
- **[@mn89h](https://github.com/mn89h)**, maintainer. Started LibreKonnect, the
  fork Copperpour grew from, and owns the JLCPCB, EasyEDA and LCSC code listed
  in [`.github/CODEOWNERS`](.github/CODEOWNERS). Changes there are his call.

`Copperpour/Copperpour` belongs to the Copperpour organization. Both maintainers
can triage, review and merge; only an organization owner can change repository
settings or the `main` ruleset.

## Merging

- **A maintainer may merge their own PR** once CI is green on its exact head and
  every review conversation is resolved. No approving review is needed.
- **Every other PR needs an approving review from a maintainer** on its exact
  head, then green CI, and a maintainer merges it. A push after the approval
  needs a new review. GitHub cannot require approval for some authors and not
  others, so this rule is the maintainers' to keep, not the ruleset's.
- **A change in the other maintainer's area waits for their review**, as
  CODEOWNERS routes it. Otherwise CODEOWNERS is a routing hint, not a veto.
- **A substantive review finding stops the merge**, whoever wrote the PR, until
  the author resolves it and the new head is reviewed again.
- **Merge commits only** (`gh pr merge N --merge`), so authorship survives.
- **The `main` ruleset enforces the rest:** pull requests only, resolved review
  conversations, merge commits only, no force-push or deletion, and these
  required checks: Check & Test on `ubuntu-latest`, Clippy, Format, Plugin
  (Python), Nix flake, PCM packaging validation and Dependency licenses.
  Check & Test on `windows-latest` joins them later (see
  [Where CI runs](#where-ci-runs)). Check & Test on `macos-latest` runs on
  each PR but is not required, because GitHub's macOS runners are often busy
  and the run fails or is cancelled for capacity, not for the code.
- **The required checks on the reviewed head are the merge evidence.** Run the
  full local gate again after a merge only when the merged tree differs from
  what CI tested, or something on `main` gives a specific reason to.

### Where CI runs

PR CI runs on GitHub Actions: `ci.yml`, plus `windows.yml` and `macos.yml`,
which run Check & Test on `windows-latest` and `macos-latest`. All three run on
each PR and each push to `main`; a new push to a PR cancels its older runs.
The required checks are the GitHub Actions app's `ci.yml` checks.

`release.yml` builds every platform's release artifacts on GitHub Actions too,
and `e2e-kicad.yml` installs KiCad on `windows-latest` and runs the real-KiCad
gate: weekly, as part of `release.yml`, on demand, and on a PR when a
maintainer applies the `run:e2e-kicad` label.

- A contributor's first PR waits for a maintainer to approve its workflow runs
  (GitHub's setting for first-time contributors). Fork PRs run with a read-only
  token and no secrets.
- If GitHub Actions becomes unavailable, local CI can post the same check names
  again as the Copperpour Local CI app; a maintainer then points the ruleset's
  required checks at that app.
- **Check & Test on `windows-latest` is not required yet.** It runs on every PR
  on GitHub (`windows.yml`), and becomes required once it has passed on a few.
  Until then, before merging a change to platform-specific code, a maintainer
  checks its Windows result and says so on the PR.

## Branches and pull requests

The contributor workflow is in
[docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md).
In short:

- One reviewable outcome per PR, branched from current `main`.
- A dependent series exposes one mergeable step at a time. A PR stacked on
  another says so in its title and names its base commit in the body.
- After a prerequisite merges, rebuild the next PR on `main` with only its own
  commits. A green run on a cumulative head proves nothing about `main`.
- Release, version and count changes land last, never through the middle of
  open work (see Releases).

## Claiming work

- **Assign the issue to yourself** when you take it. A claim written only in a
  comment is invisible to everyone not reading that thread. Without write
  access, comment `/assign` on the issue and a bot assigns you; `/unassign`
  releases it. An issue already assigned to someone else is not reassigned.
- Agree the approach on the issue for anything non-trivial, then open one
  focused PR.

## Proportionate scope review

Address the demonstrated problem in supported workflows with the smallest
coherent solution. More edge-case handling needs observed demand or a credible
safety, data-loss or security risk. Prefer a clear refusal, warning, documented
limitation or explicit caller input over speculative inference or broad
compatibility machinery. A rare trigger can still justify a small shared
correctness fix; rarity alone is not a reason to reject it.

During triage and review, separate the demonstrated defect from optional
expansion, and keep acceptance criteria bounded to the agreed outcome. When the
boundary needs a decision, say so on the issue: the supported behavior, the
exclusions, the smallest acceptable fix, and who decides. The lead maintainer
decides scope, and @mn89h decides it in his area. Record the decision in the
issue's acceptance criteria. A safe, scoped fix may land while broader
expansion is deferred.

## Releases

- **Tool counts move at release, not in PRs.** Before tagging,
  `cargo xtask fix-doc-counts` rewrites them all from the registry; commit the
  result. Hand-editing them in each PR made every tool-adding PR conflict with
  every other one.
- Version choice: a new tool, a renamed tool, or a changed response shape is a
  **minor**. Fixes — even ones that narrow behaviour nobody could have relied
  on — are a **patch**.
- Release notes state behaviour changes **and** known limitations. A reader who
  sees "fixed" and stops checking has been failed by the notes.
- Before creating the tag, run `release.yml` on the exact candidate ref
  (`gh workflow run release.yml --ref <candidate> -f version=X.Y.Z`). It
  builds every release binary and package and runs the real-KiCad gate,
  without publishing anything. The tag's own run creates a draft release,
  which a maintainer publishes once the written notes replace GitHub's list.
- The pre-release gate is CI, the real-KiCad E2E job, the live IPC tests,
  and an end-to-end benchmark run against the candidate. The benchmark has
  twice found what CI could not; it is a step, not a nicety.

## Evidence

The house rule, and the reason most of this file exists:

- **A response field must be derived from the result, never echoed from the
  request.** Most defects in this project's history are that one mistake.
- **A check that could not run is `BLOCKED`, never a silent pass.** That status
  describes the evidence item; it does not automatically decide whether the
  whole pull request is blocked.
- **Fixtures come from real KiCad output.** A hand-authored fixture tends to
  share the assumption the code got wrong, so it agrees with the bug. A PR
  needs a new one only when it changes how a KiCad file is read or written, or
  claims what KiCad does ([Tests and fixtures](CONTRIBUTING.md#tests-and-fixtures)).
- **A guard is pinned by a test that fails without it.** The reviewer breaks
  the main guards a PR adds and checks that a test fails
  ([Negative controls](CONTRIBUTING.md#negative-controls)); authors may, but
  need not.

### Risk-proportionate validation

Required CI, deterministic regression tests, and evidence for material
safety properties remain hard merge gates. Environment-dependent observations
are shared project work:

- A contributor supplies real-environment evidence from an affected environment
  they reasonably have access to. They are not expected to personally own every
  supported operating system, KiCad version, or hardware configuration.
- CI owns supported-platform regression coverage. Maintainers recruit the
  original reporter or community testers when their environment can resolve a
  remaining uncertainty more directly.
- An unavailable secondary-environment observation may become explicit
  validation debt when the change is focused and reversible, required CI is
  green, deterministic coverage is adequate, and the unobserved path is not a
  credible data-loss, security, or compatibility hazard. Name the untested
  environment in the issue completion record or release checklist.
- Missing evidence blocks the pull request when it is necessary to establish
  the change's core behavior or a material safety property and no adequate test
  or proxy exists. State that specific risk instead of requiring every platform
  by default.

Validation debt is permission to gather field evidence after a safe merge, not
permission to represent an unavailable check as passed or to bypass required CI.

## Wontfix review window

`wontfix` is a reviewed disposition, not an immediate silent closure. Applying
the label to an open issue starts a 30-day evidence window. Automation comments
with the exact UTC closing date so a reporter or contributor has one final,
visible opportunity to identify a supported interface, overlooked evidence or
changed requirement.

- Removing `wontfix` cancels the pending closure.
- Reapplying it starts a new 30-day window.
- If the issue remains open and labeled at the deadline, automation records the
  completed window and closes it as `not planned`.
- A maintainer may remove the label when new evidence changes the decision; the
  ordinary triage and architecture-review process then resumes.

The workflow enforces timing only. It never chooses the disposition.

## Licensing

Copperpour is AGPL-3.0 only, and contributions are accepted under that same
license; see [CONTRIBUTING.md](CONTRIBUTING.md). There is no CLA and no
commercial license.
