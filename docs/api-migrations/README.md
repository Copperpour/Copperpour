# Unreleased API migrations

A PR that changes a tool's name, arguments, responses or behavior in a way a
caller can see adds one file here, named after the change, for example
`run-drc-drops-tests.md`. Separate files keep two open PRs from conflicting on
the same lines of `../API_MIGRATIONS.md`.

If a tool is unreleased and already has a pending note, update that note instead
of adding a separate entry for a change to the unreleased tool.

The file starts with a `###` heading that names the change and the release it
needs, then says what changed and what callers should do:

```markdown
### `run_drc` no longer accepts `tests` (minor release)

`run_drc` always ran every check, so `tests` never had an effect. Remove it
from calls; filter the returned violations instead.
```

Only the first line may be a `#`, `##` or `###` heading; use `####` inside.
`cargo test -p xtask` checks every file here. At release,
`cargo xtask collect-api-migrations vMAJOR.MINOR.PATCH` moves them, in file-name
order, under `## vMAJOR.MINOR.PATCH` in `../API_MIGRATIONS.md` and deletes them.
It rewrites relative links inside the notes for their new place. While another
file links to a note, it refuses before changing anything: repoint that link at
the note's `../API_MIGRATIONS.md#<section>` first.
