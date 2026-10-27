### `edit_schematic_component` removes custom fields (minor release)

`edit_schematic_component` accepts an optional `remove_fields` array of
property names and deletes each one from every placed unit of the component,
in the same atomic write as the call's other edits, `dnp` included. Omitting it
reproduces the previous behavior exactly. An explicit `null` is refused as an
invalid argument on `remove_fields`, as the advertised schema refuses it.

KiCad's mandatory fields (Reference, Value, Footprint, Datasheet and
Description) are refused in any case, as eeschema reads them in any case. So is
a name also given in `fields` or `field_placements`. Both refusals come before
anything is written. A name is matched whole, so removing `MP` leaves `MPN`.
A stray property named `DNP` may be removed here; `fields` still refuses to
write one, and the native attribute is set with `dnp`.

When `remove_fields` names at least one field, the response adds
`removed_fields`, the names deleted from at least one unit, and `absent_fields`,
the names no unit carried. A call in which every name is absent and nothing
else changes writes nothing. The readback checks that each named field is gone
from every unit, before the write and after it.

To carry a field over to a new name, set the new name in `fields` and remove
the old one in the same call. The new field is added hidden at each unit's
anchor with default text effects, as `fields` adds any field; give
`field_placements` in the same call to place or show it. A field KiCad marks
private, written `(property private "NAME" …)`, refuses the whole call as an
invalid argument on `remove_fields`, naming the field and the component, before
anything is written: none of the call's other edits is applied.
