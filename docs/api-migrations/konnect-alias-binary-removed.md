### The `konnect` executable is gone (minor release)

This completes the removal that "Renamed to Copperpour" announced.
Source builds, the standalone archives and the Nix flake no longer include
`konnect`, the shim that ran the `copperpour` beside it, and the flake drops its
`konnect` package and app. An MCP client configuration or hook that starts
`konnect` must name `copperpour` (`copperpour.exe` on Windows) instead. An
archive unpacked over an older one leaves the old `konnect` in place: delete it,
or it keeps such configurations working until the next clean install. A Nix
profile that installed `#konnect` needs `nix profile remove` and an install of
`#copperpour`. The PCM package never shipped it. `copperpour init` still replaces the Claude hooks an
install through the alias wrote, `uninstall` still removes them, and `status`
still reports them as `alias_handler`
([#296](https://github.com/Copperpour/Copperpour/issues/296)).
