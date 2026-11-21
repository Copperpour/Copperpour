### Automatic IPC discovery selects the PCB editor socket beside Project Manager (minor release)

When KiCad's Project Manager listens on `api.sock` (answering `GetOpenDocuments` with
`AS_UNHANDLED`) alongside a PCB editor on `api-<pid>.sock`, automatic IPC discovery
(`detect_ipc_endpoint`, used when `ipc_address` and `KICAD_API_SOCKET` are unset) now
probes answering candidates with `GetOpenDocuments(DOCTYPE_PCB)` and selects the unique
PCB editor endpoint instead of refusing as `ambiguous_endpoint` (#87).

When multiple PCB editors are running and discovery remains ambiguous, board-targeted
IPC tools (`update_pcb_from_schematic`, `get_board_info`, `get_board_stackup`, etc.)
probe those candidates in parallel for the exact requested board: a unique holder is
used (when no other candidate is undecided), and multiple holders of the same board are
refused as `ambiguous_target`. Explicitly configured `ipc_address` and
`KICAD_API_SOCKET` values remain authoritative and are never overridden.
