# library_upgrade fixtures

Legacy library files written by hand, and KiCad 10.0.8's own upgrade of each,
used to pin `upgrade_library`'s comparison to the conversions KiCad's loader
makes rather than to an assumption.

- `kicad5.pretty/` — two KiCad 5 footprints (`(module …)`) and a KiCad 6
  one, written by hand to hold the legacy forms KiCad converts on load:
  - `THT_Legacy.kicad_mod`: no `(attr …)` (read as through-hole), a Value with
    `~RST~` overbars, a hidden user text `%R`, a pad on `*.Cu F.Mask B.Mask`,
    `(drill oval 0.8 0.8)`, `(drill 0.8 (offset 0 0))`, `(drill 0)` on an SMD
    pad, a hole on `F&B.Cu`, a footprint mask margin and a pad paste margin of
    0, and a model offset in inches.
  - `Fiducial_Legacy.kicad_mod`: `(attr virtual)`, and a pad's mask margin and
    clearance that are not 0.
  - `NetTie_Legacy.kicad_mod`: a KiCad 6 footprint (version 20211014) tagged
    `net tie`, whose pad group KiCad derives from the keywords.
- `kicad10.pretty/` — `kicad-cli fp upgrade -o kicad10.pretty kicad5.pretty`,
  KiCad 10.0.8, unedited.
- `kicad6_symbols/legacy.kicad_sym` — a symbol library at version 20201005,
  written by hand: a `legacy:RST_GATE` identifier, a Value and a pin name with
  `~RST~` overbars, a pin named `~`, a `ki_description`, and a pin name
  offset.
- `kicad6_symbols/legacy.kicad10.kicad_sym` — `kicad-cli sym upgrade -o
  legacy.kicad10.kicad_sym legacy.kicad_sym`, KiCad 10.0.8, unedited.
