### Placement and `update_footprints_from_library` carry user-text justification, mirror, bold, italic, line spacing and non-square sizes (minor release)

[#85](https://github.com/Copperpour/Copperpour/issues/85), part C:

- **Now refreshed:** `update_footprints_from_library`, and Update PCB's
  footprint replacement, take a library footprint whose `fp_text user` has
  `(justify …)` (`left`, `right`, `top`, `bottom`, `mirror`), `(bold …)`,
  `(italic …)`, `(line_spacing …)` or a font `(size height width)` whose
  height and width differ. Before, they refused it by name ("fp_text user
  effects clause 'justify' is not supported losslessly", "fp_text user font
  clause 'bold' …", "fp_text user non-square font size …"). In KiCad 10.0.8's
  stock library that unblocks 89 footprints. A placed text that has lost one of
  these effects plans `graphics`; one placed from an unchanged library plans
  `noop`.
- **Now carried by placement:** `place_component` and the other live placement
  tools give such a text the library's effects, and no longer list those
  clauses in `not_carried`. Before, the text was placed centred, regular and
  square.
- **Back side:** a library text with `(justify mirror)` reads unmirrored on a
  back-side footprint, as KiCad's flip leaves it.
- **Kept upright:** KiCad turns a keep-upright text that a rotation or a flip
  leaves at 180° or more back by 180° and swaps its justification, so `right
  bottom` at 270° is held as `left top` at 90°. The refresh now compares the
  two as the same text, for user texts and for justified fields alike, where it
  used to plan `graphics`, `metadata` or `text_effects` for them.
- **Still refused:** a font `(face …)`, any other `effects` clause, and a
  `justify` that names a side twice, two sides of one axis, or a value KiCad
  does not write (such as `center`).
