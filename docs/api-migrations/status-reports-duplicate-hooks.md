### A hook reads current only when `init` would leave it alone (patch release)

A hook read `current` whenever the command `init` writes was present. Now it
is `current` exactly when `copperpour init` would leave our handler for it
unchanged. Three new reasons for a `different` hook:

- `duplicate_handler`: another handler of ours beside the current one, such as
  the `konnect` alias's hook or the pre-hook-JSON `… skill <name>` form, or a
  second entry for it. `init` removes it.
- `stale_matcher`: our entry's tool matcher is from an older release. `init`
  rewrites it.
- `shared_entry`: ours is only inside an entry with your handlers, under your
  matcher. `init` moves it into an entry of its own.

Any of them makes the guidance `out_of_sync`. User handlers, hooks for a
Copperpour binary in another directory, and entries `init` drops without
touching ours still leave the hook `current`. `copperpour status` scans on
every run. `get_installation_info` and the startup notice reuse the check
recorded for the current install marker, so for guidance installed before
this release they keep the earlier result until the next `copperpour init`
([#245](https://github.com/Copperpour/Copperpour/issues/245)).
