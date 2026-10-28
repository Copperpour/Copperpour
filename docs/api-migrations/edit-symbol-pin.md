### `edit_symbol_pin` (minor release)

A new `library` tool sets the electrical type, graphic style or name of one pin
of an existing `.kicad_sym` symbol:
`edit_symbol_pin(library_path, symbol_name, pin_number, unit?, body_style?,
type?, style?, name?)`. `type` and `style` take the values `create_symbol`'s
pins take. Only the changed tokens are rewritten; the pin's number, position,
length and text effects, and the rest of the library, stay as written
([#113](https://github.com/Copperpour/Copperpour/issues/113)).

A pin is one drawing in one unit and body style, as KiCad writes it under
`NAME_<unit>_<body style>`; 0 is common to all, so a pin in unit 0 is in every
unit. A number found in more than one place returns `ambiguous_target`, with
the places as `candidates`, until `unit` or `body_style` chooses one. A De
Morgan symbol draws each pin in both body styles with different graphic styles,
so a call never changes both. Two pins with one number in one place cannot be
chosen and are refused. A derived symbol (`extends`) is refused with its
parent's name, since its pins are the parent's.

KiCad keeps no space in a pin name and reads one as `_`, so a space in `name`
is written as `_`. In a library older than version 20250318 KiCad reads a pin
named `~` as unnamed, and so does the tool.

The response gives the changed pin's `unit` and `body_style`, and `changes`:
each changed part with its `previous` and `current` value, as
`set_symbol_properties` reports fields. A call that changes nothing writes
nothing. The write and its readback are `set_symbol_properties`'s, now one
helper for both: a library saved after the read returns `conflict` with nothing
written.

Until now one pin could be changed only by `delete_symbol` and `create_symbol`
with every pin given again. No existing tool's arguments or responses change;
`set_symbol_properties`' refusal of a duplicated symbol name now says
"Copperpour cannot tell which one to change" instead of naming the tool.
`library` rises from 18 to 19 tools.
