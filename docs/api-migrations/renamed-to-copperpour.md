### Renamed to Copperpour (minor release)

The server now reports `serverInfo.name` `"copperpour"` instead of `"konnect"`,
and the binary is `copperpour` (`copperpour.exe`). A `konnect` binary still
ships in source builds and the standalone archives and runs `copperpour`; it
will be removed in a later release. The KiCad package has a new identifier,
`com.github.copperpour.copperpour`, so it installs beside Konnect's rather than
over it, and `get_installation_info` recognises only the new identifier as a
PCM install. A version line is read as `copperpour <version>` or
`konnect <version>`.

`COPPERPOUR_STATE_DIR`, `COPPERPOUR_BRIDGE_DIR` and `COPPERPOUR_BUILD_COMMIT`
replace the `KONNECT_*` names, which are still read when the new ones are
unset. `copperpour.toml` is searched before `konnect.toml`. User and project
directories named `konnect` or `.konnect` are moved to `copperpour` or
`.copperpour` on first use; when the new one already exists, nothing moves.

`copperpour init` writes hook matchers of the form
`mcp__(copperpour|konnect)__(...)`, and the bundled agents allow both
`mcp__copperpour__*` and `mcp__konnect__*`, so either MCP server name works. No
tool's arguments or responses change.
