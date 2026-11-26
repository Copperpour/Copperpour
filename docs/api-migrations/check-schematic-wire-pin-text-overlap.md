### `check_schematic_overlaps` reports wires crossing pin names and numbers (minor release)

The read-only check now returns `wire_pin_text_overlap` for each wire segment that
enters the run of a drawn pin name or number. Touching an edge does not count. A wire
ending on a pin's tip does not count unless it runs on into the text. A wire can report
`wire_overlap` for a body and `wire_pin_text_overlap` for the same symbol's pin text.

`a` gives the wire UUID and endpoints. `b` has `kind: "pin_text"`, the placed symbol's
`reference`, `unit` and `symbol_uuid`, the `pin_number`, `role` (`name` or `number`), the
drawn `text`, and the pin's tip in `tip_x` and `tip_y`. `pin_uuid` is the saved pin's
UUID when no other drawn pin shares its number, otherwise `null`. `bounds_b` gives the
text run's bounds in millimetres. A selected alternate's name replaces the library
name. Hidden pins, hidden names or numbers and empty text are not checked.

The response has a new key, `pin_text_unresolved`. It lists each symbol whose pin text
could not all be measured, with `reference`, `unit`, `symbol_uuid` and `reasons`, for
example `unsupported_stacked_pin_number` or `undefined_pin_alternate`. Such text is not
reported as clear. `bounds_unresolved` is unchanged and still covers body bounds only.

`compare_schematic_layout` counts the new type as `wire_pin_text_overlap` in the text
and pin visibility tier. It reports `pin_text_unresolved` as a diagnostic. If any pin
text is unresolved, that term is `null` and in `not_compared`. The other terms are
not affected.

Existing arguments and finding types are unchanged. Callers that group findings by type
should handle `wire_pin_text_overlap`. Previously clean sheets can now report findings,
which are included in `overlap_count`. The authoring tools' clearance reports do not
include the new type.

`check_schematic_page_fit` now places pin names inside at KiCad's default 0.508 mm when
a library symbol gives no `(pin_names (offset …))`, as KiCad does. It used to place them
outside, so its answer can change for such symbols.
