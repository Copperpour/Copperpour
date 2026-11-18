### `set_symbol_properties` sets the Datasheet field (minor release)

`set_symbol_properties` takes `datasheet`, a URL or path for the symbol's
Datasheet field. Only the field's value changes; it keeps its position,
visibility and effects, and the answer reports its previous and current value
like the other fields. A `~` is written as an empty string, as `create_symbol`
writes it.

`fields` still refuses `Datasheet`, now pointing to the new argument, as it does
for Value, Footprint and Description. Before, a symbol's Datasheet could only be
set when `create_symbol` created it.
