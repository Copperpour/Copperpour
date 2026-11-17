### `update_footprints_from_library` keeps field text and fabrication attributes by default (minor release)

A refresh used to write the library's Datasheet, Description and custom
property text over the placed footprint's, and the library's mounting type and
BOM and position-file exclusions over its attributes. It now keeps them, as
KiCad's Update Footprints from Library dialog does by default
(`dialog_exchange_footprints.cpp`, update mode), and offers two optional
booleans beside `reset_text_sizes_and_styles` and `reset_text_positions`, each
mapped onto one of the dialog's checkboxes:

- `reset_text_content` ("Update text content") gives Datasheet, Description
  and each custom property the library also has the library's text.
- `reset_fabrication_attributes` ("Update fabrication attributes") gives the
  footprint the library's mounting type and BOM and position-file exclusions.

Both are `false` by default. DNP and "not in schematic" are kept either way,
as before. Custom properties the library has still take the library's
position, layer and style; only their text is kept.

What a dry run plans changes: a footprint that differs from its library only
in that text or those attributes is now `unchanged`, so its plan revision
differs too. A caller that relied on a refresh setting them back passes
`reset_text_content: true` and `reset_fabrication_attributes: true`. An unset
flag is left out of the plan revision, so calls that set neither keep their
revision inputs.
