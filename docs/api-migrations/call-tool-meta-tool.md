### New always-visible meta-tool `call_tool` (minor release)

`call_tool(name, arguments)` runs a tool by name, whether or not its toolset is
loaded and whether or not the client lists it, and returns that tool's own
result. Arguments are checked against the target's schema as in a direct call,
and the call log records the target's name. `tools/list` does not change. A
client that never re-fetches its tool list after `load_toolset` can reach the
read-only tools of every toolset through it. No caller needs to change
anything.

By default `call_tool` runs only tools on a reviewed read-only list: queries,
listings and checks that write no file, change nothing in KiCad or in the
server's session, and send nothing over the network. Any other target is
refused before it runs, with the new error kind `tool_not_permitted` (`tool`,
and `reason` `writes_not_enabled`). That includes `load_toolset` and
`unload_toolset`, a tool that plans by default and applies on request, and a
check that can write its report to a path the caller names. Call such a tool
directly under its own name. `call_tool` never runs itself or `reload_server`
(`reason` `never_through_call_tool`).

The new server configuration key `call_tool_allow_writes` (default `false`)
lets `call_tool` run the other tools as well. Only the operator can set it, in
`copperpour.toml` or `settings.json`; no argument of `call_tool` does. An MCP
client asks for approval per tool name, so allowing the gateway may allow
mutating targets without their usual individual approval prompts. Turn the key
on only for a client that cannot call those tools directly and whose user
accepts that.
