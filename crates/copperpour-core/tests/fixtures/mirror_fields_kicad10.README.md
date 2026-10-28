# Mirror field-text fixture

`mirror_fields_kicad10.kicad_sch` carries the cases a reflection has to get
right for a symbol's Reference and Value text, so `Symbol::set_mirror` can be
checked against an answer that is not its own.

A field's `(at …)` is an absolute sheet coordinate, not an offset from the
body. The sheet is therefore built in **twins**, like
`rotate_fields_kicad10`: a symbol, and the same symbol placed with the target
reflection outright 25.4mm below it. Reflecting the first must land its fields
exactly on the second's, less that 25.4mm.

## Provenance

Built through Copperpour against KiCad's stock `Device` and `Regulator_Linear`
libraries — `create_schematic`, `add_schematic_component` (with `mirror`,
mixelpixx/Konnect#485) and `edit_schematic_component` — then parsed and force-resaved
by KiCad
10.0.6:

```text
kicad-cli sch upgrade --force mirror_fields_kicad10.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`, KiCad's own library
records, field positions and UUIDs. KiCad left every field position exactly
where Copperpour had written it.

## Cases

| Ref | Symbol | Body | Mirror | Twin | Role |
|---|---|---|---|---|---|
| `D1` | `Device:LED` | (101.6, 50.8) 0° | — | `D2`, `x` | anchors above and below the origin trade sides |
| `U1` | `Regulator_Linear:AP2112K-3.3` | (139.7, 50.8) 0° | — | `U2`, `y` | an anchor off both axes: only X may change |
| `U3` | `Regulator_Linear:AP2112K-3.3` | (177.8, 50.8) 90° | — | `U4`, 90° `x` | a turned body: reflecting agrees with placing reflected |
| `U5` | `Regulator_Linear:AP2112K-3.3` | (215.9, 50.8) 0° | `x` | `U6`, `y` | swapping axes is two reflections |
| `U7` | `Regulator_Linear:AP2112K-3.3` | (139.7, 101.6) 0° | `y` | `U8`, none | clearing the mirror reflects back |
| `D3` | `Device:LED` | (101.6, 101.6) 0° | — | — | Reference dragged 10mm right and 10mm up via `edit_schematic_component`: a manual offset must reflect, not reset |

## Oracle

`kicad-cli sch export svg` renders each field as an invisible `<text>` element
carrying its anchor, so KiCad itself says where the text lands. Applying each
case's reflection with `set_mirror` and re-exporting puts every Reference on
its twin's, to the last digit KiCad prints:

| Field | anchor as placed | after `set_mirror` | twin's anchor | twin − reflected |
|---|---|---|---|---|
| `D1` → `x` | (101.6000, 48.8950) | (101.6000, 53.9749) | `D2` (101.6000, 79.3749) | (0, 25.4000) |
| `U1` → `y` | (136.0012, 45.7200) | (143.3988, 45.7200) | `U2` (143.3988, 71.1200) | (0, 25.4000) |
| `U3` → `x` | (172.0850, 55.1338) | (172.0850, 47.7362) | `U4` (172.0850, 73.1362) | (0, 25.4000) |
| `U5` → `y` | (212.2012, 57.1499) | (219.5988, 45.7200) | `U6` (219.5988, 71.1200) | (0, 25.4000) |
| `U7` → none | (143.3988, 96.5200) | (136.0012, 96.5200) | `U8` (136.0012, 121.9200) | (0, 25.4000) |

The last column is the offset between each twin's origin and its own, and
nothing else, so the comparison needs no constant for justification or
baseline. Those are identical between a symbol and its twin, because both
carry the same reflection when they are drawn.

The render proves only that KiCad draws each field where the file says. The
twins are Copperpour placements, so their field positions come from the same
placement transform the fix assumes. `U3` against `U4` therefore shows that
reflecting agrees with placing reflected. It cannot show that the transform
rotates before it mirrors, since a transform with that order wrong would move
both sides alike.

That order, and the reflection itself, are checked against sheets KiCad drew:

- `project_ownership/complex_hierarchy.kicad_sch`: `P102` is a `CONN_2` placed
  `(mirror y)` at x = 41.91, whose library anchors put Reference at local
  x = −1.27 and Value at +1.27. Eeschema wrote them at 43.18 and 40.64, on the
  reflected side, and clearing the mirror must restore 40.64 and 43.18.
- KiCad's `video` demo, `video/modul.kicad_sch`: `L2` is turned 270° and
  `(mirror x)`. Its library anchors the Value at (2.54, 0), and eeschema wrote
  it 2.54mm above the origin, where rotate-then-mirror puts it and
  mirror-then-rotate does not. Clearing the mirror must move it 2.54mm below.
  Across the 25 demo sheets in KiCad 10.0.6 that place a symbol mirrored at
  90° or 270°, 206 Reference and Value fields sit on a library anchor that the
  two orders place differently; all 206 match rotate-then-mirror. The demos
  are CC BY-SA 4.0, so the test reads this sheet from the installed KiCad
  (`KICAD_DEMOS`, then the standard install paths, as `conformance_test.rs`
  does) and skips when none is found or L2 is no longer placed that way.

## ERC

`kicad-cli sch erc --severity-all` reports 62 violations on the sheet as
committed: 38 `pin_not_connected`, 16 `power_pin_not_driven` and 8
`pin_not_driven`. Every symbol here is placed to be measured, not wired, so
this is the expected state, not a defect the fixture hides.
