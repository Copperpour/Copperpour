# Board whose saved state cannot be mistaken for a live one

`board_source_divergence_kicad10.kicad_pcb` is the saved half of the
`board_source` tests for `get_layer_list` and `get_netclasses`. Its stackup and
its nets are chosen so that **no value in it can also be produced by the live
KiCad double** the same tests run against: a test that reads the wrong adapter
gets a visibly wrong answer rather than a coincidentally right one.

## Provenance

Built from `specctra_two_resistors.kicad_pcb` (two `Resistor_SMD:R_0402`
footprints on `F.Cu`) with three deliberate changes:

- the stackup was widened to four copper layers using three different copper
  kinds — `(0 "F.Cu" signal)`, `(4 "In1.Cu" power)`, `(6 "In2.Cu" mixed)`,
  `(2 "B.Cu" signal)`;
- `F.Fab` was given the user rename `SavedOnlyFabName`, and `B.Fab` was added
  without one;
- both pad nets were renamed to `SAVED_ONLY_A` and `SAVED_ONLY_B`.

It was then re-serialized by the installed KiCad with

```text
kicad-cli pcb upgrade --force board_source_divergence_kicad10.kicad_pcb
```

so the committed bytes are pcbnew's own output (`(generator "pcbnew")`,
`(generator_version "10.0")`, `(version 20260206)`, tab-indented). Nothing was
edited after the resave. Measured against **KiCad 10.0.6** on Fedora Linux.

## Oracles

All three were produced by `kicad-cli` from the committed file, and none of
them reads any Copperpour code.

### Enabled layers and their displayed names — `pcb export gerbers`

`kicad-cli pcb export gerbers -o ger board_source_divergence_kicad10.kicad_pcb`
plots one file per enabled layer and names each by the name KiCad *shows*:

| Gerber file | Layer |
|---|---|
| `…-F_Cu.gtl` | `F.Cu` |
| `…-In1_Cu.g1` | `In1.Cu` |
| `…-In2_Cu.g2` | `In2.Cu` |
| `…-B_Cu.gbl` | `B.Cu` |
| `…-F_Adhesive.gta`, `…-B_Adhesive.gba` | `F.Adhes`, `B.Adhes` |
| `…-F_Paste.gtp`, `…-B_Paste.gbp` | `F.Paste`, `B.Paste` |
| `…-F_Silkscreen.gto`, `…-B_Silkscreen.gbo` | `F.SilkS`, `B.SilkS` |
| `…-F_Mask.gts`, `…-B_Mask.gbs` | `F.Mask`, `B.Mask` |
| `…-Edge_Cuts.gm1` | `Edge.Cuts` |
| `…-Margin.gbr` | `Margin` |
| `…-F_Courtyard.gbr`, `…-B_Courtyard.gbr` | `F.CrtYd`, `B.CrtYd` |
| **`…-SavedOnlyFabName.gbr`** | `F.Fab`, under its user rename |
| `…-B_Fab.gbr` | `B.Fab`, which has none |

18 enabled layers. The job file `…-job.gbrjob` states the copper count
independently:

```json
"GeneralSpecs": { "LayerNumber": 4 }
```

and lists exactly four `Copper,L…` files, `L1` through `L4`.

### The same stackup again — `pcb export ipc2581`

`kicad-cli pcb export ipc2581 -o board.xml …` writes one `<Layer …>` element
per enabled layer, with the four conductors in stackup order:

```xml
<Layer name="F.Cu"   layerFunction="CONDUCTOR" side="TOP"/>
<Layer name="In1.Cu" layerFunction="CONDUCTOR" side="INTERNAL"/>
<Layer name="In2.Cu" layerFunction="CONDUCTOR" side="INTERNAL"/>
<Layer name="B.Cu"   layerFunction="CONDUCTOR" side="BOTTOM"/>
```

Neither export encodes a copper layer's `signal`/`power`/`mixed` kind — that
attribute exists only in the `(layers …)` table, which is why `get_layer_list`
reports it as file-backed even when the rest of its answer is live.

### Board nets — `pcb export ipcd356`

`kicad-cli pcb export ipcd356 -o net.d356 …` names the two nets:

```text
SAVED_ONLY_A
SAVED_ONLY_B
```

## Why these values

The live KiCad double in the tests answers with a **two**-copper-layer board
(`F.Cu`, `B.Cu`, `Edge.Cuts`), renames `F.Cu` to `LiveRenamedTop`, and reports
the nets `LIVE_ONLY_P` and `LIVE_ONLY_Q`. No name, count or net is shared with
the file above, in either direction:

| Fact | Saved file | Live double |
|---|---|---|
| copper layers | 4 | 2 |
| `In1.Cu` / `In2.Cu` present | yes | no |
| `F.Cu` shown as | `F.Cu` | `LiveRenamedTop` |
| renamed fab layer | `SavedOnlyFabName` | absent |
| nets | `SAVED_ONLY_A`, `SAVED_ONLY_B` | `LIVE_ONLY_P`, `LIVE_ONLY_Q` |

## Tests using this fixture

`crates/copperpour-core/src/tools/board_source_contract_tests.rs` — the served
`tools/call` contract tests for `board_source` on `get_layer_list` and
`get_netclasses`.
