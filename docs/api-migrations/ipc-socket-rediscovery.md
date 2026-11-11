### A detected KiCad socket is looked for again on every IPC call (minor release)

When `ipc_address` is not configured and `KICAD_API_SOCKET` is unset, the
server used to detect KiCad's socket once at startup and keep that address for
its whole life. It now looks again on every IPC call, reading file metadata
only, so KiCad started or restarted after Copperpour is found without
restarting the server. On Unix the search also takes `api-<pid>.sock` beside
`api.sock` when that process is running and the socket is newer, which is
where KiCad 10 listens when a stale `api.lock` blocks `api.sock`. A configured
or `KICAD_API_SOCKET` address is never replaced. `get_installation_info` still
reports the address chosen at startup.
