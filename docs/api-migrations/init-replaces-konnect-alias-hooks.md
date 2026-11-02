### `copperpour init` replaces the hooks of the `konnect` alias (patch release)

The `konnect` alias beside `copperpour` runs that binary, so `init` now treats
the Claude hooks an older install wrote for it as its own and replaces them
with ones naming `copperpour`. Before, both were kept and each hook ran twice
for a client that still registers the server as `konnect`. `uninstall`
removes them as well. `status` and `get_installation_info` report a hook that
only the alias holds as `different` with the new reason `alias_handler`,
instead of `other_executable`. Hooks naming a `konnect` or `copperpour` in
another directory are left alone
([#244](https://github.com/Copperpour/Copperpour/issues/244)).
