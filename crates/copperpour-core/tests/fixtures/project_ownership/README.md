# KiCad project-ownership fixture

Copied byte-for-byte from the `complex_hierarchy` demo distributed with KiCad
10.0.5 for Windows (`share/kicad/demos/complex_hierarchy`). The schematics identify
`eeschema` 9.0 as their generator. These are KiCad demo files, not a user design.

The root references `ampli_ht.kicad_sch` twice, with distinct sheet UUIDs. Tests
must preserve both instance paths while resolving one project owner. Embedded
symbols also allow mutation-preflight tests without an installed symbol library.

Original SHA-256:

| File | SHA-256 |
| --- | --- |
| `complex_hierarchy.kicad_pro` | `5fd37556fb32ca1e3ea1ed8376c4dd9a8e3929621c2976aa28c07b39e1842cd8` |
| `complex_hierarchy.kicad_sch` | `67c637f279e1feb2aa9c02784ed3fd567c1d7d825a0aba7a24c699a6e258f88f` |
| `ampli_ht.kicad_sch` | `4d0335e6b09bbdfe5de1766fc697c5e053652cd86aa8511aac6c335a21fa4514` |

Tests relocate files or alter Sheetfile references only in temporary copies.
Missing roots, malformed content, cycles, and ambiguous ownership are deliberate
test corruptions. The checked-in source bytes remain unchanged.
