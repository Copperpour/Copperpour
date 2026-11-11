### New always-visible meta-tool `call_tool` (minor release)

`call_tool(name, arguments)` runs any tool by name, whether or not its toolset
is loaded and whether or not the client lists it, and returns that tool's own
result. Arguments are checked against the target's schema as in a direct call,
and the call log records the target's name. `tools/list` does not change. A
client that never re-fetches its tool list after `load_toolset` can reach every
tool through it. No caller needs to change anything.
