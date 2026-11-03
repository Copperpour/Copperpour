### `update_footprints_from_library` stops planning pad changes for corner rounding (patch release)

A dry run planned `pads` for any footprint with a circular or rectangular
pad, though KiCad's DRC reports no `lib_footprint_mismatch` for it
([#254](https://github.com/Copperpour/Copperpour/issues/254)). KiCad
reports its default corner rounding, 0.25, on every pad, where the library
build leaves 0. The comparison now ignores corner rounding on shapes other
than rounded and chamfered rectangles, the only ones that use it, so such
footprints plan no change: a board whose footprints match their libraries
dry-runs to `noop` and an apply leaves them alone. Real differences are
still planned: a footprint whose board copy lacks the library's
attributes, properties or text settings still reports `attributes`,
`metadata` or `graphics`.

Graphics still compare exactly, which is stricter than DRC: KiCad's
library-parity check accepts graphic points within 10 nm. A board copy
whose points drifted by a nanometre or a few still plans a `graphics`
change that DRC does not flag, and the update corrects the drift. A caller
that discounted planned `pads` changes as noise no longer needs to.
