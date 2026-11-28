### Schematic wiring tool refusals carry structured `error.kind` envelopes (minor release)

Refusals from `delete_schematic_wire`, `batch_delete_schematic_wire`,
`split_wire_at_point`, `delete_schematic_net_label`, `rotate_schematic_label`,
`batch_rotate_labels`, `move_labels_by_offset`, `delete_no_connect`,
`batch_delete_no_connect`, `batch_add_no_connect` and `connect_to_net` now
return structured JSON error bodies (`{"message": ..., "error": {"kind": ...}}`)
instead of plain-text `handler_error` messages (#741). The human-readable text
is unchanged in `message`, except that `batch_add_no_connect` called without a
`positions` array now says `Argument 'positions' is invalid: missing or not an
array`, as other tools do.

- `stale_target`: the sheet holds no wire, label or no-connect the call names.
  `reason` repeats `message`. `target` names what the call asked for:
  - `wire <uuid>`, `wire (x1, y1)-(x2, y2)` or `wire through (x, y)`;
  - `label <uuid>`, `label '<net>' at (x, y)`, or `label '<net>'` for
    `move_labels_by_offset`, which takes no position;
  - `no_connect at (x, y)`;
  - `uuids` or `positions` when no item of a batch delete was found.
- `plan_blocked` (`operation: "delete_schematic_net_label"`): several labels of
  the name share the position. The tool takes no `uuid`, so remove the
  duplicates in Eeschema.
- `invalid_argument`:
  - `delete_schematic_wire` given neither `uuid` nor all four coordinates.
    `field` is `uuid` when no coordinate is given, else the first missing one.
  - `connect_to_net` given `reference` without `pin_number`, or the reverse.
    `field` names the missing one.
  - `batch_add_no_connect` without a `positions` array.
  - `batch_delete_no_connect` that deleted nothing while an item lacks numeric
    `x` and `y`: `field` is `positions[i]` for the first such item.
- `malformed_file`: `split_wire_at_point` parsed the wire but could not find
  its block in the text.

`split_wire_at_point` now finds a wire by its endpoints when the text search
for its uuid misses: an unquoted uuid, as KiCad wrote before version 8, or a
uuid an earlier item repeats. It used to refuse those wires.

Callers matching tool error text should read `message` or branch on
`error.kind`.
