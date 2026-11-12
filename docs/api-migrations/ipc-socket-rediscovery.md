### A detected KiCad socket is looked for again on every IPC call (minor release)

When `ipc_address` is not configured and `KICAD_API_SOCKET` is unset, the
server used to detect KiCad's socket once at startup and keep that address for
its whole life. It now looks again on every IPC call, reading file metadata,
so KiCad started or restarted after Copperpour is found without restarting the
server. On Linux and macOS the search also takes every `api-<pid>.sock` beside
`api.sock` whose process is running and that passes the same ownership check,
which is where KiCad 10 listens when a stale `api.lock` blocks `api.sock`.
When more than one socket qualifies, the one that answers an NNG Ping is used;
if not exactly one answers, none is adopted and the "not configured" error and
the startup log list every candidate. A configured or `KICAD_API_SOCKET`
address is never replaced. `get_installation_info` still reports the address
chosen at startup.
