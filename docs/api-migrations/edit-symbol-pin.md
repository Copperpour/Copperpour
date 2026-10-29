### `edit_symbol_pin` (minor release)

A new `library` tool sets the electrical type, graphic style, name, number,
position, angle or length of one pin of an existing `.kicad_sym` symbol:
`edit_symbol_pin(library_path, symbol_name, pin_number, unit?, body_style?,
type?, style?, name?, new_number?, x?, y?, angle?, length?)`. `type`, `style`,
`x`, `y`, `angle` and `length` are the keys `create_symbol`'s pins use, `x` and
`y` the pin's connection point in mm. Only the changed tokens are rewritten;
the pin's text effects and the rest of the library stay as written
([#113](https://github.com/Copperpour/Copperpour/issues/113)).

A pin is one drawing in one unit and body style, as KiCad writes it under
`NAME_<unit>_<body style>`; 0 is common to all, so a pin in unit 0 is in every
unit. A number found in more than one place returns `ambiguous_target`, with
the places as `candidates`, until `unit` or `body_style` chooses one. A De
Morgan symbol draws each pin in both body styles with different graphic styles,
so a call never changes both. Two pins with one number in one place cannot be
chosen and are refused. A derived symbol (`extends`) is refused with its
parent's name, since its pins are the parent's.

`new_number` is refused when another pin of the symbol stands for that number,
in any unit, unless the two are in different body styles: the pins KiCad's
symbol checker reports as duplicates, a stacked `[1-3]` number standing for
each number in it. A renumbered pin stays where it is in the file; KiCad's next
save puts it in number order. `angle` is 0, 90, 180 or 270, the directions
KiCad reads, and `length` is not negative. Coordinates and lengths are kept to
KiCad's 0.0001 mm, so a value KiCad would read as the current one is no change.
A pin that writes no `(at …)` or `(length …)`, which KiCad defaults, refuses a
change to it. KiCad keeps no space in a pin name or number and reads one as
`_`, so a space is written as `_`, and `pin_number` is read the same way. In a
library older than version 20250318 KiCad reads a pin named or numbered `~` as
empty: the tool reads it so too, and refuses `~` as a new name or number.

The response gives the changed pin's `unit` and `body_style`, and `changes`:
each changed part with its `previous` and `current` value, as
`set_symbol_properties` reports fields, `x`, `y`, `angle` and `length` as
numbers. A call that changes nothing writes nothing. The write and its readback
are `set_symbol_properties`'s, now one helper for both: a library saved after
the read returns `conflict` with nothing written.

Until now one pin could be changed only by `delete_symbol` and `create_symbol`
with every pin given again. No existing tool's arguments or responses change;
`set_symbol_properties`' refusal of a duplicated symbol name now says
"Copperpour cannot tell which one to change" instead of naming the tool.
`library` rises from 18 to 19 tools.
