### `create_footprint` writes the KiCad 10 format, and its outlines follow the body (minor release)

[#711](https://github.com/Copperpour/Copperpour/issues/711), as agreed on #704.

#### Format

`create_footprint` writes the format KiCad 10's footprint writer writes, the one
`upgrade_library` produces, so `kicad-cli fp upgrade --force` gives the file
back unchanged, UUIDs and the generator aside:

- `(version 20260206)` and `(generator_version "10.0")`, tab-indented as
  KiCad lays files out; `(generator "copperpour")` stays.
- Reference and Value as `(property …)` fields instead of KiCad 7's
  `(fp_text reference|value …)`, beside hidden, empty Datasheet and
  Description properties, as KiCad adds them.
- `(duplicate_pad_numbers_are_jumpers no)`, `(embedded_fonts no)`, a UUID on
  every item, `(fill no|yes)` instead of `(fill none|solid)`.
- Graphics and pads in the order KiCad sorts them, pad layers in KiCad's order
  (`"F.Cu" "F.Mask" "F.Paste"`), `(remove_unused_layers no)` on a plated hole,
  a pad rotation in [0, 360) (-90 is written as 270), lengths in whole
  nanometres without trailing zeros (`1.85`, not `1.8500`).
- No `(descr "")` when the description is empty.

A caller that read the file as text, for example looking for `fp_text`, reads
the properties instead. KiCad reads both, and an older KiCad cannot open the
new file; Copperpour targets KiCad 10 only.

#### Outlines

With `body_width` and `body_height`:

- the courtyard goes around the pads **and** the body, plus the clearance
  (it went around the pads only, so a wider body stuck out of it);
- the silk runs 0.11 mm outside the body, as line segments broken where they
  would come within 0.2 mm of a pad (it was a rectangle 0.15 mm outside the
  pads, under a wider body).

Without a body, both follow the pads as before. Pad rotation now counts in the
pad envelope: a pad rotated by 90° spans its height along X.

#### `body_height`

`body_height` was described as the body's height, but it has always been the
body's size along Y, seen from above, and it still is. Its description now says
so. Pass the 3D height to the model, not here.
