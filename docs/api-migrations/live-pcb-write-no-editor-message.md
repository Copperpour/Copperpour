### Live PCB writes with no PCB editor open say so (patch release)

With KiCad running and no PCB editor open, live PCB writes such as
`set_board_title_block`, `add_board_text`, `set_component_placements` and
`update_footprints_from_library` still refuse and leave the board file alone.
The refusal no longer says KiCad "may hold this board open". It now says that
KiCad reports no PCB editor at the endpoint, and asks the caller to open the
board in the PCB Editor and retry. The result is still an error of the same
shape (`update_footprints_from_library` keeps its `conflict` diagnostic).
Callers matching the old sentence should match the new one.
