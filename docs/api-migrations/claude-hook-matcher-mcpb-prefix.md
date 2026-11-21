### Claude `PreToolUse` hooks and bundled agents match `mcp__Copperpour__*` (patch release)

`copperpour init` now writes Claude `PreToolUse` hook matchers of the form
`mcp__(copperpour|Copperpour|konnect|Konnect)__(...)` instead of
`mcp__(copperpour|konnect)__(...)`, and both bundled Claude agents
(`kicad-schematic-build-agent` and `kicad-design-review-agent`) now allow
`mcp__Copperpour__*` and `mcp__Konnect__*` alongside the lowercase prefixes
([#737](https://github.com/Copperpour/Copperpour/issues/737)).

Claude Desktop's `.mcpb` extension registers tools under its capitalized
`display_name` (`mcp__Copperpour__<tool>`) in Claude Desktop's Code tab, which
missed the previous case-sensitive lowercase-only matchers and agent tool
allowlists. Unmodified installs carrying a matching install manifest upgrade
automatically on the next server start; older or customized installs report
`stale_matcher` / `different` and refresh on `copperpour init`.
