### `set_symbol_properties` sets the Reference prefix (minor release)

`set_symbol_properties` takes `reference_prefix`, the value of the symbol's
Reference field (such as `U` or `J`), as `create_symbol` takes it. Only the
field's value changes; the answer reports its previous and current value like
the other fields.

A value with nothing left once trailing digits and `?` are removed (`""`, `1`,
`?`) is refused as `invalid_argument`, as KiCad's Symbol Properties dialog
refuses it. A symbol without a Reference field is refused rather than given a
second one, as for Value.

`fields` still refuses `Reference`, now pointing to the new argument. Before, a
symbol's Reference could only be set when `create_symbol` created it.
