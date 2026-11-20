### `copperpour init` replaces Claude hooks from other Copperpour executable paths (patch release)

Running `copperpour init` for Claude now replaces existing Copperpour-managed `PreToolUse` hook handlers registered by another `copperpour` or `konnect` executable path (in either current `hook` or legacy `skill` form), rather than appending a second set of hooks alongside them. User-authored neighboring handlers and unrelated commands are preserved.

`copperpour status` and `get_installation_info` correspondingly report `different` (`duplicate_handler`) when a hook from another `copperpour`/`konnect` executable sits beside the current executable's hook. Running `copperpour init` removes the duplicate and restores `current` status. `copperpour uninstall` remains scoped to the current executable's own hook commands.
