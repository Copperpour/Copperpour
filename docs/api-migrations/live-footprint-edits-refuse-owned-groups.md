### Live footprint edits refuse a footprint that owns groups (patch release)

KiCad 10.0.8 rebuilds a footprint from the message a tool sends when it
updates the footprint over the API, and that message carries no groups, so
every group the footprint owned was dropped while the tool reported success.
On a board open in KiCad, these tools now refuse instead, before anything is
written, when a footprint they would change owns a group:

- `move_component`, `rotate_component`, `set_component_placements`,
  `repeat_layout` (also with `dry_run: true`) and `align_components`;
- `edit_component` with an edit, `set_footprint_field`, `edit_footprint_field`
  and `set_footprint_field_style` (every footprint when `references` is
  omitted);
- `edit_board_footprint_graphic`, `set_pad_net`, and
  `set_placed_footprint_models` with `append`, `replace` or `remove`.

The refusal is `plan_blocked`, with one reason per footprint naming it, how
many groups it owns, and the KiCad PCB editor action that keeps them. A batch
is refused whole, naming every such footprint. `repair_corrupted_footprints`
reports a `footprint_children_not_carried` diagnostic per such footprint, so
its plan is a `conflict`. Membership of a board-level group is not ownership
and is not refused.

Each of these calls now reads the board once with `SaveDocumentToString`
before it writes. `flip_component`, which uses KiCad's own flip, and the
closed-board paths, which edit the board file, are unchanged. Make such edits
in KiCad's PCB editor, or ungroup the footprint's items first.
