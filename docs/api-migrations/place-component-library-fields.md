### `place_component` places Reference and Value as the library has them (patch release)

Placed live, a footprint's Reference and Value fields were built without the
library's text style: no stroke width, a line spacing of 0, not kept upright,
both on the silkscreen, and the Value hidden
([#157](https://github.com/Copperpour/Copperpour/issues/157)). They now carry
the library's layer, visibility, font, stroke, line spacing, justification and
keep-upright, placed as `update_pcb_from_schematic` places the footprints it
adds.

A library footprint whose properties cannot be read, such as one that writes
a property twice, is still placed with the default fields, and the response
then has a `warning` saying so. Placement into a closed board file is
unchanged.
