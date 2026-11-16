### `get_installation_info` reports the IPC endpoint the tools dial now (minor release)

`get_installation_info` reported only the address resolved at startup, as
`source: "resolved_server_config"`. With socket rediscovery, a KiCad started or
restarted after the server is found on every IPC call, so the report could say
`endpoint: null` while every tool was talking to KiCad, or name a socket the
calls no longer used.

The `ipc` block now reports what the next IPC call dials, from the same
decision the calls make:

- `endpoint`: that address, redacted as before, or `null` when nothing is
  dialled.
- `source`: `resolved_server_config` (the startup address, as before),
  `detected_at_call_time` (a socket found now), `ambiguous` (several sockets
  qualify and not exactly one answered a Ping, so none is dialled), or
  `unresolved` (nothing to dial).
- `candidates`, new: the redacted sockets an `ambiguous` detection found;
  empty otherwise.
- `configured` is unchanged: whether an address was resolved at startup.

With rediscovery on, the report looks for KiCad's socket the way an IPC call
does: file metadata, and a Ping to each candidate only when several qualify.

A caller that read `source` as always `resolved_server_config` should accept
the three new values; one that read `endpoint: null` as "KiCad is not
reachable" can now rely on it meaning that nothing is dialled.
