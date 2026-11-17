### Closed-board footprint edits report late KiCad locks as `unsafe_file_fallback` (patch release)

`move_component`, `rotate_component`, `set_component_placements`,
`flip_component`, `place_decoupling_caps`, and
`refine_placement_force_directed` now hand writer and I/O errors from
`ClosedBoardError::into_result` back to dispatch instead of wrapping them in an
untyped error. When a `~<board>.kicad_pcb.lck` file appears between the
pre-write gate and the atomic rename, callers now receive
`unsafe_file_fallback` (matching other board writers). Other failures to read,
parse or write the board surface as `handler_error`. Retry only after confirming no KiCad
process holds the board and the lock is gone.
