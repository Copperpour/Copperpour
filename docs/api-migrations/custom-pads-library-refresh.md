### The library refresh and Update PCB's replacement take footprints with custom pads (minor release)

As agreed on [#679](https://github.com/Copperpour/Copperpour/issues/679), part 2:
`update_footprints_from_library` and Update PCB's footprint replacement take
footprints with custom pads too. A refresh compares each custom pad's anchor,
size and primitives with the library's, so a footprint placed from its library
plans no change, and a changed pad plans `pads`. Before, both refused them
("pad shape 'custom' is not supported by typed library refresh").

A custom pad with `(clearance convexhull)`, or a primitive the shared model
does not read, is still refused, as in live placement.
