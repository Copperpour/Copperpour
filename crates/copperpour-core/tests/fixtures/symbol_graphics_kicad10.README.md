# Symbol graphics fixtures

`symbol_graphics_kicad10.kicad_sym` is KiCad 10.0.8's resave (`kicad-cli sym
upgrade --force`, Windows, line breaks then stored as LF) of a library holding:

- `CUI_PD-30` from KiCad 10.0.8's stock `Connector` library: a rectangle,
  polylines, arcs, circles and texts in unit block `_0_1`, pins in `_1_1`;
- `Antenna_Internal` from the stock `Auxiliary_Items` library: a bezier;
- `BOX`, #867's reproduction: a rectangle `(start -7.62 3.81) (end 7.62 -3.81)`
  in `BOX_0_1`, four pins in `BOX_1_1`;
- `BOX_DERIVED`, which extends `BOX`;
- `NOTE`: a private rectangle with a dashed red stroke and a colour fill, a text
  at 90 degrees (written `900`, in tenths of a degree) and a text box, in
  `NOTE_1_1`.

The last three were written by hand in KiCad's syntax before the resave, so
every byte of the file is KiCad's.

```text
sha256 f1af6837ef054c2991b7b418cf5d8fb12965312e04783275a22fd53e8aa349a4
```

The other files are what `set_symbol_graphics` writes for these calls, each at
the revision `get_symbol_info` gave, then checked with KiCad 10.0.8 (Windows):

```text
rm -f resaved.kicad_sym   # kicad-cli leaves an existing output file as it is
kicad-cli sym upgrade --force <file> -o resaved.kicad_sym
diff --strip-trailing-cr <file> resaved.kicad_sym
```

`symbol_graphics_kicad10.edited.kicad_sym`, from `symbol_graphics_kicad10.kicad_sym`.
KiCad resaves it unchanged:

```text
set_symbol_graphics BOX        unit 0 body_style 1 replace index 0
                    [rect (-7.62, 5.08) (7.62, -5.08), stroke 0.254, fill background]
set_symbol_graphics CUI_PD-30  unit 0 body_style 1 delete index 0
sha256 d674cdb9860e54e9f5d9c5b13da64063d41fbc93eabf9d3ac21e0b8f73888754
```

`symbol_graphics_kicad10.appended.kicad_sym`, from `symbol_graphics_kicad10.kicad_sym`;
`NOTE` has no `NOTE_0_1` before the second call. KiCad resaves it unchanged:

```text
set_symbol_graphics BOX   unit 1 body_style 1 append
                    [poly of 11 points (-6.35 + 1.27 i, ±1.27), stroke 0.254, fill none]
set_symbol_graphics NOTE  unit 0 body_style 1 append
                    [circle (0, 0) r 1.27, stroke 0.254, fill none]
sha256 148406233ad9a9a2e4712a3cd17aaa161f705618f049f4febdf94b31c07a1877
```

`symbol_graphics_kicad10.appended_many.kicad_sym`, from `symbol_graphics_kicad10.kicad_sym`:

```text
set_symbol_graphics CUI_PD-30  unit 0 body_style 1 append
                    [line (-7.62, 8.89) (7.62, 8.89), arc (-2.54, 8.89) (0, 11.43) (2.54, 8.89),
                     circle (0, -8.89) r 0.635 fill outline, rect (-8.89, 8.89) (-7.62, 7.62) fill none]
sha256 025af52b4207c21f85d937065dcc7f9ed2dfc1d8bc1e996cdb22d88e37c15358
```

KiCad's resave of it orders the unit's items by position
(`SCH_ITEM::operator<`), and lists the same items:
`symbol_graphics_kicad10.appended_many.resaved.kicad_sym`, stored as LF.

```text
sha256 15ab62e43ccf23612a732ecf911f1c292b3d11eb967362d3c2cc070a0820cc5c
```

`fiber_optic_kicad7.graphics.kicad_sym`, from `fiber_optic_kicad7.kicad_sym` (KiCad
7's stock library, indented with spaces; see `fiber_optic_kicad7.README.md`):

```text
set_symbol_graphics AFBR-1624Z  unit 0 body_style 1 append
                    [rect (-8.89, 8.89) (8.89, -8.89), stroke 0.254, fill none]
set_symbol_graphics AFBR-1624Z  unit 0 body_style 1 replace index 1
                    [line (6.35, -2.54) (2.54, -6.35), stroke 0.254]
sha256 3bb7cda64a45f7b80b7c121e796d8bd9594f509f359472f88ea4f07b362b3536
```

`fiber_optic_kicad7.graphics.resaved.kicad_sym` is KiCad 10.0.8's resave of it, in
the KiCad 10 format, stored as LF; it lists the same items:

```text
sha256 fa854f44080554a4220c7c12bc897a46f69ffd165b3f9d16de8294da51298193
```
