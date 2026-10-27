### `set_symbol_properties` sets Value, and renames and removes custom fields (minor release)

`set_symbol_properties` accepts three new optional arguments: `value`, the
symbol's own Value field; `rename_fields`, custom fields as current name: new
name; and `remove_fields`, an array of custom field names. Omitting them leaves
the other arguments as they were, with two differences. A field written as
`(property private "NAME" …)` is now read by its name; before, its name was
taken to be `private`. And `fields` naming Value now points to the `value`
argument, so `set_symbol_properties` no longer leaves Value alone: it changes
Value only through `value`, and still does not change Reference or Datasheet.

A rename replaces only the field's name, so its value, position, visibility and
text effects stay as they were and it keeps its place among the fields, as
KiCad's own resave keeps it. Names match whole and case-sensitively, as KiCad
matches custom field names, so `MANUFACTURER` can be renamed `Manufacturer`.
Mandatory and `ki_*` names, in any case, cannot be renamed, taken by a rename or
removed. A current name the symbol does not carry, or a new name it already
carries, is refused, and a rename to the same name changes nothing. A name may
appear in only one of `fields`, `rename_fields` and `remove_fields`, and a chain
or swap of names takes one call per step. An explicit `null` for `rename_fields`
or `remove_fields` is refused as `invalid_argument` on that argument, as their
schema types say, rather than read as omitted. Every refusal comes before
anything is written. A `value` for a symbol without a Value field is refused
rather than written hidden. On a derived symbol each argument edits that
symbol's own fields, and an empty Value shows its parent's.

In `changes` a rename appears twice, the old name with `current` null and the
new name with `previous` null, and a removal appears with `current` null. When
`remove_fields` names at least one field, the response adds `removed_fields`,
the names deleted, and `absent_fields`, the names the symbol did not carry. A
call in which every name is absent and nothing else changes writes nothing. The
readback checks, before the write and after it, that each removed or
renamed-away name is gone and that each renamed field is its old block with
only the name changed.
