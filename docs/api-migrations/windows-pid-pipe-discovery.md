### Windows IPC discovery includes `api-<pid>.sock` pipes and case-insensitive `open_board` path matching (minor release)

Windows IPC endpoint detection (`detect_ipc_endpoint` / `candidate_endpoints`)
now enumerates sibling `api-<pid>.sock` named pipes in `\\.\pipe` alongside
the default `api.sock`, matching Linux and macOS behavior. If two answering PCB
editors bind separate pipes on Windows, detection now reports ambiguity rather
than silently picking `api.sock`.

In `open_board` on Windows:

- A configured `ipc_address` matching KiCad's default socket path is recognized
  case-insensitively and across `/` and `\` separator spellings instead of
  refusing with `editor_unavailable`.
- When `open_board` times out while another responsive endpoint (`api.sock`,
  such as the KiCad Project Manager) answers without the requested board, the
  `note` explains that KiCad 10 on Windows does not bind a fallback
  `api-<pid>.sock` pipe beside `api.sock` and advises opening the board from
  the running Project Manager or closing the other KiCad instance and retrying.
