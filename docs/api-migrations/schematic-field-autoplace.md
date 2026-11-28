### Add `autoplace_schematic_fields` and opt-in `autoplace_fields` (minor release)

Load `sch_readability`, then call `autoplace_schematic_fields` on a saved `.kicad_sch`.
Pass only `schematic` to preview. Each symbol is identified by UUID, reference and unit.
The response lists old positions, proposed positions and skip reasons for its Reference
and Value fields. Review the proposals individually. To apply, pass the returned
`revision` and the selected fields' `approval` strings as `approvals`. A changed sheet
requires another preview. Only approved positions are written, in one revision-checked
write. Some proposed stacks need both fields to move. If an unapproved field would
obstruct an approved placement, the tool refuses the request without writing.

The preview reports whether a field matches the transformed library default or carries
KiCad's existing `fields_autoplaced` marker. Neither is consent to move a saved field.
A `do_not_autoplace` field is never offered for placement. An approved write clears
the symbol's autoplacement marker; Copperpour adds no marker of its own. The apply
response lists in `withdrawn_offers` any unapproved field of that symbol whose only
evidence was the marker; a new preview reports it as `positioned`.

The tool does not move wires or symbols. An eligible dedicated power lead can get a
`move_power_to_pin` suggestion instead. The proposed stub must also be clear of other
wires. Bus or tap contact on either route, unreadable bus or tap geometry, and new
field collisions block the advice. Unrelated readable buses and taps do not.
This is a drawing check, not an electrical bus membership check. Field
clearance covers wires, net labels, measured Reference and Value text, symbol bodies
and pins. It does not check other drawing objects, including free-text notes, sheet
boxes and pins, bus strokes and taps, or pin names and numbers.

An unreadable or non-quarter-turn saved symbol angle prevents field offers and power
advice on the sheet. The preview reports unmeasurable fields as `cannot_measure`.

`add_schematic_component` and `batch_place_components` (per component) accept
`autoplace_fields: true` to place newly created fields in the same write.
Their `autoplaced_fields` reports show saved positions and why a field moved or stayed.
In a batch, the field readback follows request order. Opt-in single placement
uses the same junction checks as batch placement, so it does not join separate
wires at an undotted tee.
An unmeasurable saved angle leaves new fields at their defaults. The new symbol can
still be placed.
With `rotate_schematic_component`, that option first previews the prospective turn.
Repeat the same turn with its `revision`, `turn` token and `approvals` to write both
together. Pass an empty list to approve only the turn. The preview shows each unit's
old and proposed orientation. The apply response reports saved symbol positions
in `placements` and approved field positions in `approved_field_placements`. The
turn refuses newly introduced collisions
involving measured Reference or Value text, including those caused by carrying
unapproved fields, even if the layout score did not worsen. It also refuses a
new component overlap even when another overlap disappears. The layout and
electrical gate still applies. An unmeasurable saved angle blocks an opt-in turn
before writing.
Both apply paths report a conflict if the saved bytes changed since the preview,
even if they no longer parse as a sheet. An unsafe plan returns `plan_blocked`;
invalid approval IDs or turn tokens still return `invalid_argument`. On an uncertain
turn, inspect the saved sheet before retrying. `observed_before_uncertainty` lists
the turn and marker changes that could be verified.
Without the option, placement and rotation behave as before.
Explicit positioning through
`field_placements` clears the symbol's saved autoplacement marker; changing visibility
alone does not clear it.
