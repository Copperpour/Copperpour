### `launch_kicad_ui(wait_ready)` re-probes and IPC discovery backs off dead sockets (minor release)

`launch_kicad_ui` with `wait_ready: true` previously resolved `ipc_endpoint()` once
before its polling loop. When Copperpour was started before KiCad (unresolved startup
endpoint), the wait loop kept polling the empty address until timeout even after the
launched KiCad created its socket.

Two runtime behaviors change:

- `launch_kicad_ui` now resets any active ping backoff after spawning
  KiCad, and with `wait_ready: true` re-resolves the IPC endpoint on each 500 ms
  poll iteration, so it observes the socket created by the launched KiCad without
  restarting Copperpour.
- IPC rediscovery probes on `ToolContext` are now serialized. When several candidate
  sockets exist and none can be chosen (`Ambiguous`), the same set is re-pinged with
  bounded exponential backoff (250 ms doubling up to 2 s); a changed set, a found
  endpoint, `open_board`'s selection or a KiCad launch probes at once. No socket or
  a single one needs no ping and is never backed off, so a KiCad started after
  Copperpour is seen on the next call.

Explicitly configured `ipc_address` and `KICAD_API_SOCKET` values remain authoritative
and never trigger discovery or backoff. No tool arguments or response schemas changed.
