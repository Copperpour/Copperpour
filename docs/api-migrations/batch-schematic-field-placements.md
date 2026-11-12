### Batch schematic edits support field text placement and unit selection (minor release)

Each `batch_edit_schematic_components.edits[]` entry now accepts
`field_placements` and `unit`, with the same schema, placement parser, and writer
as `edit_schematic_component`. This moves field text without moving symbol bodies
or changing field values. For example:

```json
{
  "schematic": "amplifier.kicad_sch",
  "edits": [
    {"reference": "R1", "field_placements": {"Value": {"x": 12.5, "y": 34.5, "rotation": 90}}},
    {"reference": "U1", "unit": 2, "field_placements": {"Value": {"hide": true}}}
  ]
}
```

Coordinates are absolute schematic millimetres, not offsets from the symbol.
Omitted placement attributes retain their existing values. A position (`x` or
`y`) on a part with several placed units requires a positive `unit` selector.
Rotation and visibility changes without `unit` apply to every placed unit.
A supplied unit selects field text only: values, footprints, custom fields, and
DNP remain whole-component edits. A DNP edit with `unit` but no nonempty
`field_placements` is refused for that entry, as in the single-component tool.

Successful placement entries add `unit_count` and `units` from the saved-file
readback. Each unit includes `field_placements`, keyed by field name with
`x`, `y`, `rotation`, and `hide`, plus its UUID and other existing component
readback data. `units` includes every placed unit, even when only one was edited.
Existing batch response fields, including the per-field `fields` array and
`updated_count` (successful edit entries), keep their meanings. Multiple entries
can place text on different units of the same component; their readbacks show the
final saved state, including values or DNP set by later entries.
`updated_unit_copies` and `created_unit_copies` continue to count property-value
writes and creations; text placements are reported through `changes` and `units`
rather than added to those counters.

The batch plans in memory and commits once through the existing revision-checked
atomic writer. Atomic publication does **not** mean every requested edit succeeds:
inspect `errors` as well as `updated`. Missing fields/units, empty per-field placement specs,
and a missing selector for a multi-unit position are reported in `errors`; other
valid field changes and entries can still apply. With `field_placements`,
non-positive or out-of-range integer unit selectors refuse that entry before its
value/DNP edits. Invalid schema types or unknown placement attributes refuse the whole call
before the handler runs. No missing field is created by `field_placements`; use
`fields` with `create_missing: true` to create a custom field in the same batch.

A field on a particular placed unit may receive a placement assignment only once
per batch. An overlapping assignment refuses **all placements in the later
entry**; its otherwise valid value/field/DNP edits may still apply. Combine that
field's position, rotation, and visibility in one placement spec.

Malformed source placement data or a failed prospective readback refuses the
whole write. A mismatch in the component/placement readback after publication returns
`mutation_outcome_uncertain`, naming the schematic and entries carried in the
write. Reload and inspect those entries before retrying; do not replay the whole
batch merely because verification failed. Editor-lock and revision-conflict
protections remain in force, including locks held on a parent/root sheet.
