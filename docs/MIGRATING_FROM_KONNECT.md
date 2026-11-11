# Migrating from Konnect

How to move from [Konnect](https://github.com/mixelpixx/Konnect) to Copperpour, its
community fork. Back to the [README](../README.md).

Copperpour is a separate KiCad package from Konnect, with its own identifier, so
KiCad installs it beside Konnect rather than over it.

1. If you ran `konnect init`, run `konnect uninstall` first. It removes the hooks
   that start the old binary, which stop working once its package is gone.
2. Install the Copperpour package, then uninstall Konnect in the Plugin and
   Content Manager.
3. Point your MCP client at the new binary, `copperpour` (`copperpour.exe` on
   Windows), as shown in the [README](../README.md#step-2-register-the-mcp-server).
   Renaming the server entry from `konnect` to `copperpour` is optional: the
   bundled hooks and agents accept both names. A renamed entry changes the tool names to `mcp__copperpour__*`, so update any
   permission rules that name `mcp__konnect__*`.
4. Run `copperpour init` to install the guidance again.

Your files move on first use: `~/.konnect` to `~/.copperpour` on Linux and macOS,
`%APPDATA%\konnect` to `%APPDATA%\copperpour` on Windows,
`~/Library/Application Support/konnect` to `.../copperpour` on macOS,
`~/.config/konnect` to `~/.config/copperpour` on Linux, and each project's
`.konnect` directory to `.copperpour`. If a directory cannot be moved, it is used
where it is. Nothing is moved when the new directory already exists.

`KONNECT_STATE_DIR`, `KONNECT_BRIDGE_DIR` and `KONNECT_BUILD_COMMIT` are now
`COPPERPOUR_*`; the old names are still read when the new ones are unset. A
`konnect.toml` in the working directory is still read, after `copperpour.toml`.
The `konnect` executable that v0.15 and v0.16 shipped beside `copperpour` is
gone, so MCP client configurations must name `copperpour`; delete a `konnect`
left beside it by unpacking a newer archive over an older one. `copperpour init`
still replaces the hooks an install through that `konnect` wrote, and
`uninstall` removes them.
