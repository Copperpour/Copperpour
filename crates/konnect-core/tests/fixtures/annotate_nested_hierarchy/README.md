# Nested reused-sheet fixture ([#42](https://github.com/Copperpour/Copperpour/issues/42))

A project made for these tests in KiCad 10.0.6. Copperpour created the project,
the root's two sheets and every symbol (`create_project`,
`add_hierarchical_sheet`, `edit_sheet`, `batch_place_components`); the two
sheets inside `bank.kicad_sch` were drawn in eeschema, which records a nested
sheet under the hierarchy's root (see
[#49](https://github.com/Copperpour/Copperpour/issues/49)). eeschema then set the pages,
annotated and saved every file, so what is committed is its serialization:
`(version 20260306)`, `(generator "eeschema")`, `(generator_version "10.0")`.

The root, `nested_pages.kicad_sch`, places `bank.kicad_sch` twice, as `north`
above `south`, and `bank.kicad_sch` places `cell.kicad_sch` twice, as `west`
left of `east`. Each `cell.kicad_sch` symbol carries four instance records and
each `bank.kicad_sch` symbol two, all under the project `nested_pages`.

The pages were set by hand:

| Instance | Page |
| --- | --- |
| root | 1 |
| `south` | 2 |
| `south` / `east` | 3 |
| `south` / `west` | 4 |
| `north` | 10 |
| `north` / `west` | 11 |
| `north` / `east` | 12 |

So page order is neither the walk's (by position: `north` before `south`,
`west` before `east`), nor the paths' sorted as strings (`north`'s sorts
first), nor the files' (`north` is first in the root's, `east` in
`bank.kicad_sch`), and the two sheets in `bank.kicad_sch` change order
between their parents.

| File | Symbols |
| --- | --- |
| `nested_pages.kicad_sch` | 2 `Device:R`, 2 `Device:C`, 1 `Device:Fuse`, 2 `power:+3V3` |
| `bank.kicad_sch` | 3 `Device:R`, 2 `Device:D`, 1 `Device:L`, 1 `power:+3V3` |
| `cell.kicad_sch` | 2 `Device:R`, 2 `Device:C`, 1 `Device:Crystal`, 3 `power:+3V3` |

Every `cell.kicad_sch` resistor is left of every `bank.kicad_sch` resistor.

The designators are eeschema's own: Annotate with the scope "Entire
schematic", "Reset existing annotations", "Sort symbols by X position" and
the default numbering. They follow page order, then X, on every instance:
`R1`, `R2` on the root, `R3`..`R5` on page 2, `R6`, `R7` on page 3, and so on
to `R16` on page 12; `C`, `D`, `F`, `L`, `Y` and `#PWR` follow the same
pattern. Each `Reference` property shows its file's first instance in page
order. `kicad-cli` 10.0.6 netlists the root with 37 components, each its own
designator.

SHA-256:

| File | SHA-256 |
| --- | --- |
| `nested_pages.kicad_pro` | `b580202be02ccd79603cbc72d5f130f04d507f5f816f1af64e6fbefc18332d71` |
| `nested_pages.kicad_sch` | `deb87c84fb87d4a6bd15c90d3be365d7ff6a6f302eda57c41312de2d9ecf4589` |
| `bank.kicad_sch` | `ee7ed3a0b3dac6ec296fa8e9a9168548a5f68f92ed86903183d9157949a9b340` |
| `cell.kicad_sch` | `b1c132920fde65d27046c2e45338bd1d82dd814a29a5ae8df28acf0f4ce21310` |

Tests leave designators unannotated, change pages, or add and remove records
only in temporary copies. The checked-in bytes remain unchanged.
