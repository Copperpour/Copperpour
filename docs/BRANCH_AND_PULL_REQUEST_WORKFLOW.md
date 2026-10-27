# Branch and Pull Request Workflow

Copperpour accepts independent pull requests, short dependent series, and
maintainer-approved integration branches. Choose the smallest model that makes
every review show one coherent change.

The base branch is part of the review contract. A green check on a branch that
contains obsolete prerequisites does not prove that its unique change works on
current `main`.

**Contributions start from the latest `upstream/main`, not the latest release
tag.** Release tags are stable consumption points for users and packagers; they
do not contain work merged after the release. A PR based on a release can be
green in isolation while omitting fixes and contracts already present on
`main`.

## Current repository mechanics

`main` is protected by a ruleset: pull requests only, resolved review
conversations, the required hosted checks, merge commits only, and no
force-push or deletion. [GOVERNANCE.md](../GOVERNANCE.md#merging) lists the
required checks and says who may merge what. A maintainer's own PR needs no
approval; every other PR needs a maintainer's approving review of its exact
head.

## Reviews

A maintainer records a review of someone else's PR as one GitHub review on the
exact head: `--approve` when it is ready, `--request-changes` with every
finding when it is not. The review names the head SHA. A push after the review
makes it stale; the new head needs its own review.

GitHub does not let authors review their own PR, so a maintainer's own PR
needs none. CODEOWNERS requests the other maintainer's review when the PR
touches their area; that request stays until they answer it.

## Default: one independent change

Use an independent branch when a change can be reviewed and merged without
another unmerged pull request.

```text
git fetch upstream
git switch -c fix/example upstream/main
# edit, test, and commit
git push -u origin fix/example
```

Open the pull request against `main`. Before its first final review:

1. fetch `upstream`;
2. rebase the branch onto current `upstream/main`;
3. resolve conflicts in the branch rather than asking the merge commit to guess;
4. push rewritten history with `--force-with-lease`, never plain `--force`;
5. wait for the required checks to pass on the new head.

Do not use `vX.Y.Z` or another release tag as a development base unless a
maintainer explicitly requests a backport to that release line.

Do not stack independent changes merely because one contributor is developing
them at the same time. Separate branches let either change merge, wait, or be
abandoned without moving the other.

If `main` advances after that review, do not rebase again for its own sake.
Rebase when GitHub reports a conflict, or let a maintainer do it under
[Maintainer refresh of a clean behind branch](#maintainer-refresh-of-a-clean-behind-branch).

## Dependent changes: expose one mergeable step at a time

A dependent series is appropriate only when change B cannot build, test, or be
reviewed meaningfully until change A exists.

Record the complete order on the tracking issue and in every PR description:

```text
#123 -> #124 -> #125
```

Only the next PR in that sequence should be ready for review against `main`.
Keep later work on separate local or fork branches. Link those branches from the
tracking issue if visibility is useful, but do not open cumulative PRs against
`main` that repeat every prerequisite commit.

After the prerequisite merges, reconstruct the next branch so it contains only
its unique work on current `upstream/main`. For a simple one-commit step:

```text
git fetch upstream
git switch -c fix/next-clean upstream/main
git cherry-pick <unique-commit>
# resolve conflicts, test, and inspect the diff
git push --force-with-lease origin HEAD:fix/next
```

For several unique commits, use `git rebase --onto` or cherry-pick the precise
range. In either case, verify both views before requesting review:

```text
git log --oneline upstream/main..HEAD
git diff --stat upstream/main...HEAD
```

The log must contain only the commits this PR owns. The diff must not reintroduce
already merged prerequisites. Old CI results are superseded by any rewritten
head or changed base.

### When a stacked PR base is possible

A PR can target an immediate prerequisite branch only when that base branch
exists in the upstream repository. A branch in a contributor's fork cannot be
used as the base branch of a PR in the upstream repository.

Collaborators may use an upstream prerequisite branch when maintainers agree,
but the deeper PR stays draft until its parent is merged. Afterward, retarget it
to `main`, synchronize it with current `main`, and rerun CI. Do not leave a chain
of ready PRs whose displayed diffs all contain the same unmerged changes.

## Integration branch: an explicit exception

A short-lived integration branch is useful when a tightly coupled program needs
several contributors or individually reviewed steps, but cannot keep restacking
against `main`. It is not the default for a large change.

Before creating `integration/<topic>`, obtain maintainer agreement on:

- the tracking issue and intended user outcome;
- the ordered child PRs and their owners;
- the branch owner and expected deletion date;
- how often current `main` will be incorporated;
- the integration, compatibility, and rollback evidence required at the end;
- which terminal PR will close each issue.

Child PRs target `integration/<topic>` and must show only their unique changes.
They receive the same tests and focused review expected for a PR to `main`.
Unrelated work continues to target `main`; the integration branch must not hold
other work hostage.

After all child PRs land, update the integration branch from current `main`,
resolve drift once, and run the complete gate. Open one terminal PR from the
integration branch to `main`. That final review verifies the combined behavior,
issue accounting, compatibility, release notes, and rollback plan; it is not a
substitute for reviewing the child changes.

Delete the integration branch after the terminal merge. If its scope changes or
it remains open past the agreed lifetime, return to the tracking issue and
reconfirm the plan.

## What is merge-ready

A PR is merge-ready only when all of the following are true:

- its base and dependencies match the documented plan;
- its commit list and diff contain only the change it owns;
- GitHub reports no conflicts with current `main`;
- every required check passed on the exact current head;
- the issue acceptance criteria, compatibility impact, and validation evidence
  are current;
- partial and terminal issue-closing keywords are correct.

A PR is not merge-ready merely because an earlier cumulative head was green.

## Maintainer refresh of a clean behind branch

When a reviewed PR is behind current `main` and conflicts with it, a maintainer
may refresh the contributor branch instead of sending it back for mechanical
integration. This path applies only when all of the following are true:

- the PR is the next step of any series it belongs to;
- the commit list and diff contain only the PR's unique work;
- the contributor permits maintainer edits; and
- no unresolved finding requires the author to change the implementation.

Record the current head SHA and current `upstream/main`, then use the rebase form:

```text
gh pr update-branch N --rebase
```

The ordinary update command creates a merge commit in the contributor branch;
Copperpour uses `--rebase` for its focused branches. Refreshing rewrites the head
and invalidates earlier readiness. Afterward:

1. verify the new commit list and diff still contain only the same unique work;
2. compare the old and new unique commits or patches and investigate any
   substantive difference;
3. wait for the required checks on the new head; and
4. record an exact-head review before merging.

If GitHub reports `DIRTY`, resolve conflicts caused by intervening `main`
changes when the resolution is mechanical and preserves the PR's unique
behavior. Record the conflict resolution and include it in the semantic-diff
comparison. When the semantic diff is unchanged, this can be a focused refresh review that
references the completed substantive review and the current-main CI. It does not
require repeating every original test or review step. An ambiguous conflict,
changed unique diff, failed check, disabled maintainer edits or copied
prerequisite history is author work: say so on the PR and return it.

## Merging

A maintainer merges with `gh pr merge N --merge` once the exact head is
merge-ready (above), reviewed under [Reviews](#reviews), and green. A new
commit, rewritten head, base change or failed check returns the PR to review.
After the merge, check that the issues it closes are closed.

## Responsibilities when `main` moves

A maintainer may refresh a reviewed PR when it qualifies for the
maintainer path above, including mechanical conflict resolution. The PR author
owns ambiguous semantic conflicts, repairing failed checks and reconstructing
cumulative or misleading history. A maintainer may help with that work, but
branch reconstruction is not a standing service.

When a stale PR contains copied prerequisite commits, the preferred correction
is to reconstruct it from current `main` with only its unique commits. A
maintainer may return the PR to draft or request reconstruction instead of
reviewing a misleading cumulative diff.

Do not repeatedly rebase a deep series after every unrelated merge. Wait until
the immediate prerequisite lands, then refresh or rebuild the next PR once.
This keeps work moving while minimizing conflict work for contributors and
reviewers.

## Issue closure in a series

Use `Part of #N` for a partial PR. Use `Closes #N` on exactly one terminal PR
only when that merge satisfies every current acceptance criterion. For an
integration branch, child PRs use `Part of`; the terminal PR to `main` carries
the closing references and the acceptance evidence.

See [GOVERNANCE.md](../GOVERNANCE.md) for claiming, merge authority, required
checks, and when to re-run the local gate after a merge.
