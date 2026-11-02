### Live PCB writes with no PCB editor open say so (patch release)

With KiCad running and no PCB editor open, live PCB writes such as
`set_board_title_block`, `add_board_text` and `set_component_placements` still
refuse and leave the board file alone. The refusal no longer says KiCad "may
hold this board open". It now says that KiCad reports no PCB editor at the
endpoint, and asks the caller to open the board in the PCB Editor and retry.
When this session has not seen the board live, it also offers quitting KiCad,
which lets the saved file be edited. The result is still a plain error.
Callers matching the old sentence should match the new one.
