### `file_not_found` reports `absolute_path` and `get_installation_info` reports `working_directory` (patch release)

When a tool refuses on a missing file or directory, the structured `file_not_found`
error now reports `absolute_path` alongside the requested `path`. Relative paths
resolve against the server process's working directory (`std::env::current_dir()`),
which the MCP client sets upon launching Copperpour. When `path` is relative,
`absolute_path` reports `cwd.join(path)`; when `path` is already absolute, both fields
report that path.

Additionally, `get_installation_info` now includes `working_directory` in its `runtime`
response section, allowing clients and agents to discover the server's working
directory without guessing.

Callers should pass absolute filesystem paths to avoid resolving against an unexpected
working directory. Existing callers matching on `error.kind == "file_not_found"` or
reading `error.path` require no changes.
