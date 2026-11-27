### IPC recovery steps and PCB guidance now recommend `open_board` (patch release)

When a live PCB tool fails because no PCB editor is listening at the endpoint
(`no_pcb_editor_at_endpoint`), no PCB documents are open (`no_open_documents`),
a different board is open (`wrong_document`), or the previously bound board is
no longer uniquely open (`stale_document`), `recovery.next_steps` now includes
an `agent` step recommending `open_board` (in the `verification` toolset)
ahead of the manual `user` fallback, instead of `open_project`.
`UnreachableReason::NoListener` also notes `open_board` alongside
`check_kicad_ui` and `launch_kicad_ui`. Error kinds, causes, `retry_safe`, and
non-target recovery branches are unchanged.

The bundled `kicad-pcb` skill and `copperpour` operating/troubleshooting
guidance likewise direct agents to call `open_board` before live PCB work when
the target board is not yet open.
