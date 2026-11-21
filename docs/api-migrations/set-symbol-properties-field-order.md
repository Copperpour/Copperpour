### `set_symbol_properties` reorders custom fields (minor release)

[#717](https://github.com/Copperpour/Copperpour/issues/717). No tool could move a
field: putting `Manufacturer` before `Manufacturer Number` took three calls of
renames, and only worked when both fields looked alike.

`set_symbol_properties` takes `field_order`, a list of custom field names. The
named fields go first among the custom fields, in that order; the others keep
their order after them. Each field moves whole, with its value, position,
visibility and text effects, as KiCad's Symbol Properties dialog moves fields;
KiCad keeps the order a file gives (fields take ordinals in read order).

- **Same call:** the order applies to the fields as the call leaves them, so a
  field set in `fields` or renamed in `rename_fields` can be named.
- **Answer:** `field_order` gives the custom fields' order before and after.
  An order already in place writes nothing.
- **Refused** as `invalid_argument` on `field_order`, before writing: an empty
  list or not a list, an empty name, a name given twice, a mandatory field
  (KiCad always writes them first), a `ki_*` name (written after the custom
  fields), a name also in `remove_fields`, and a name the symbol does not carry
  after the call's other changes.
