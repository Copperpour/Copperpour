### A file that changed after a tool read it is a `conflict`, not `handler_error` (patch release)

When a file changes between a tool's read and its write, the writer refuses
before replacing it, so the file holds only the other program's save. Tools that
passed the writer's error on unchanged, such as `add_schematic_net_label`,
`add_no_connect`, `batch_add_wire`, `batch_connect_to_net`,
`move_labels_by_offset`, `edit_footprint_pad` and `delete_symbol`, answered
`handler_error` ("write conflict: … changed since it was read", or "loaded").
The closed-board footprint edits (`move_component`, `rotate_component`,
`flip_component`, `set_component_placements`, and `place_decoupling` and
`force_directed` through them) answered it as an untyped error. They now answer
`conflict` with `paths` naming the file, as the tools that refused it themselves
already did: reload the file and reapply the change. A multi-file transaction's
`TransactionConflict` is unchanged.

`create_schematic` and `add_power_symbol` write the file once before their last
write, so a failure of that last write, including a save or KiCad's editor lock
found only then, is now `mutation_outcome_uncertain`, not `handler_error` or
`conflict`: reload and inspect the file before retrying. Issue:
[#480](https://github.com/Copperpour/Copperpour/issues/480).
