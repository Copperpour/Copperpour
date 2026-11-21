### New tool `open_board` in `verification` (minor release)

`open_board(board, display?, timeout_seconds?)` opens a `.kicad_pcb` in
KiCad's PCB editor and waits until a KiCad IPC endpoint reports that exact
board, as asked on [#23](https://github.com/Copperpour/Copperpour/issues/23).

- **Looks first:** every endpoint detection chooses among, or the configured
  one alone, is asked for its open documents and then a read of the board.
  One holding the board is used, and no editor is started.
- **Starts:** otherwise `pcbnew <board>` of the KiCad installation Copperpour
  resolves, and waits for it. `kicad -f pcb` is not used: it exits on a
  segmentation fault in KiCad 10.0.8.
- **Then:** when several sockets answer, as a Project Manager beside a PCB
  editor does, the calls that follow use the one the last `open_board` found
  holding its board, while it is among them; `get_installation_info` reports
  it as `source: "selected_by_open_board"`.
- **Answer:** `requested_board`, `requested_open`, `launched`, `ipc_address`,
  `timed_out`, `timeout_seconds`, `elapsed_ms` and `endpoints`, each with
  `ipc_address`, `requested_open` and `error`; a `note` when none reported
  the board in time.
- **No editor is started** while an endpoint answers `AS_NOT_READY` or gives
  no clear answer (a request timed out, or a reply that does not parse), lists
  the board without serving a read of it, or has a request still running;
  while KiCad's lock beside the board is present or cannot be checked,
  refused as `conflict` with the lock in `paths`; or when a configured
  `ipc_address` or `KICAD_API_SOCKET` answers without the board or is not
  KiCad's own socket, refused as `editor_unavailable`. Each says what to do
  instead.
- **Two endpoints holding the board:** refused as `ambiguous_target`, the
  board as `target` and the endpoints as `candidates`; neither is used.
- **Windows:** detection looks at the default pipe alone, so an editor started
  beside a running KiCad is not found there, and the call times out.
