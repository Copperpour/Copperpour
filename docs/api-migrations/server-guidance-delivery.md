### MCP resource reads now return bundled guidance (minor release)

Every `initialize` response includes short operating rules, followed by any
existing one-time installed-guidance drift notice. Clients should no longer
expect `instructions` to be absent when there is no drift. This sends guidance;
it cannot force a client or model to follow it.

`initialize` advertises the static `resources: {}` capability. `resources/list`
lists bundled skill and reference Markdown at `copperpour://guidance/<path>`.
`resources/read` requires a string `uri` and returns MCP `contents` with `uri`,
`mimeType` and `text`, replacing the invalid empty `resources` read response.
Unknown resource identities return `-32002`; malformed parameters or a supplied
continuation cursor return `-32602`. There are no templates, subscriptions,
list-change notifications or pagination. `resources/templates/list` returns an
empty `resourceTemplates` list. Both list methods accept omitted parameters.

For clients that do not expose resources, the new always-visible `get_guidance`
meta-tool lists exact bundle paths when called with `{}`. Pass, for example,
`{"path":"copperpour/SKILL.md"}` or
`{"path":"copperpour/references/reliability-contract.md"}` to return the same
`contents`. Null/non-string paths, unknown paths and extra arguments are refused
as `invalid_argument`; only omission lists the catalog. Resource paths are exact
catalog identities, not local file paths. Resolve a relative reference from a
skill into that skill’s listed `references/` path before calling the tool.

Serving and native installation use the same embedded catalog. Server delivery
needs neither `init` nor filesystem/network access to guidance. It does not
create, overwrite or remove native skill files. `init` remains opt-in for native
skill discovery, local customization and Claude hooks; its arguments, existing
paths and drift reporting are unchanged. Custom skill roots and setup snippets
are separate later PRs for #461. Reconnect after upgrading the server to obtain
the new build’s catalog.

Protocol shape follows the [MCP 2025-06-18 resources specification](https://modelcontextprotocol.io/specification/2025-06-18/server/resources).
