### Schematic component edits preserve private fields (patch release)

`edit_schematic_component` now updates existing private fields in place instead of planning a public duplicate and returning `ambiguous_target`. `batch_edit_schematic_components` also updates existing private fields with the default `create_missing: false`.

When an edit adds a missing copy to another unit, the new copy is private if all existing copies are private. If the existing copies disagree and a copy is missing, the tool reports an error for that field and writes none of its copies. A unit with duplicate field names is still refused. `remove_fields` still refuses to remove a private field.
