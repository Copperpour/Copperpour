# Global and local power driver witnesses

The seed is KiCad's Eeschema-authored
[`same_local_global_power.kicad_sch`][seed] at tag `10.0.8` (SHA-256
`ac689aa12a5308f0b2906e08a0eb06e0c695b5c377efca4b6ff73a48617e7fb3`). Its
instance project is `ERC_same_name_local_global_power_port`, so the copies use
that basename. They are single root sheets without a project sidecar.

Through Copperpour's served tools, remove the seed's four no-connect markers and
symbols `#PWR02` and `#PWR04`. Leave local `#PWR01` and global `#PWR03`, which
has a saved `lib_name` of `VCC_1`. Place stock `Device:R` at (134.62, 92.71),
(160.02, 92.71) and (121.92, 92.71), in reference order. Join the mixed power
symbols and pins `R1.1` and `R3.1` with horizontal and vertical segments meeting
at saved endpoints. Put the hierarchical label `AAA_PORT` and local label
`ZZZ_LOCAL_ONLY` on a separate run to `R2.1`. Place `ROUTE_DECOY` on `R3.2`.

Copy that sheet to create the variants. Change only the local placed Value for
`after` and `same_name`. Add `AAA_LABEL` at a wire corner for `local_label`; add
`ZZZ_LABEL` there for `global_label`. In `local_power_label` remove the global
power symbol and add `AAA_LABEL` at its old wire endpoint. For `audits`, start
with the local name `AAA_LOCAL`; replace `R1` and `R3` with a registered custom
two-pin witness `U1`. Its `VDD` power input and `RESET` input both reach the rail.
For `local_alias_island`, add another instance of the local `power:GND`
definition (`#PWR05`) on a separate stub to `R4.1`; no wires join the islands.
All edits used served tools. Force-resave every copy with KiCad CLI 10.0.8:

```sh
kicad-cli sch upgrade --force ERC_same_name_local_global_power_port.kicad_sch
kicad-cli sch export netlist --format kicadxml -o witness.xml \
  ERC_same_name_local_global_power_port.kicad_sch
kicad-cli sch erc --format json --severity-all -o witness.erc.json \
  ERC_same_name_local_global_power_port.kicad_sch
```

The XML exports from 10.0.6 and 10.0.8 agree on these names and pin sets:

| Copy | Competing drivers on the rail | Exported name | Exported pins |
| --- | --- | --- | --- |
| `before` | local power `GND`, global power `VCC` | `VCC` | `R1.1`, `R3.1` |
| `after` | local power `ZZZ_LOCAL`, global power `VCC` | `VCC` | `R1.1`, `R3.1` |
| `same_name` | local and global power `VCC` | `VCC` | `R1.1`, `R3.1` |
| `local_label` | same-name pair and local label `AAA_LABEL` | `VCC` | `R1.1`, `R3.1` |
| `global_label` | `local_label` plus global label `ZZZ_LABEL` | `ZZZ_LABEL` | `R1.1`, `R3.1` |
| `local_power_label` | local power `GND`, local label `AAA_LABEL` | `/GND` | `R1.1`, `R3.1` |
| `audits` | local power `AAA_LOCAL`, global power `VCC` | `VCC` | `U1.1`, `U1.2` |
| `local_alias_island` | mixed `GND`/`VCC` plus a disconnected local `GND` stub | `VCC` | `R1.1`, `R3.1`, `R4.1` |

On the separate run each export gives `R2.1` to `/ZZZ_LOCAL_ONLY`, not `AAA_PORT`.
Copperpour reports the sheet-local spelling without the root prefix `/`.
The mixed-name ERC warning names the exported winner on both versions; 10.0.8
also reports `same_local_global_power` for the same-name pair. These are
precedence specimens, not ERC-clean circuits. Both versions also report the
parentless hierarchical label, undriven power input and unused pins.

## Review regressions

- `hierarchy` extends `audits` with an unported child sheet `B`. In a copy of
  `local_power_label`, keep the local power symbol, rename its value `VCC`,
  and replace the resistor on that rail with `C1`, a `10uF` capacitor. Link it
  with `add_hierarchical_sheet`. Both native exports put `U1.1` and `U1.2` on
  root `VCC` and `C1.1` on `/B/VCC`. The child capacitor does not decouple the
  root rail. For the CLI run, keep both files under the working directory.
- `midwire` and `midwire_junction` each extend `before`. Delete the two
  segments to the original global power symbol and delete the symbol.
  Place global power at (134.62, 85.09) and set its value to `VCC`. This is
  mid-span on the unsplit run to `R1.1`. Remove only the new junction with
  `batch_delete` for the `midwire` case. Both native exports put `R1.1` and
  `R3.1` on `/GND` without that junction and on `VCC` with it.
- `passive_alternate` comes from `before` and was saved in Eeschema 10.0.8.
  Use Edit with Symbol Editor on `#PWR03` to add a `PASSIVE_WITNESS` passive
  alternate to pin 1. In Symbol Properties, select it under Pin Functions.
  Both versions export `R1.1` and `R3.1` on `/GND`.

The hierarchy and midwire variants were prepared from the KiCad-saved sheets
above via served Copperpour calls and resaved with `sch upgrade --force` in
KiCad CLI 10.0.8. No fixture was hand-edited. Both CLI versions exported the
exact checked-in bytes without changing them.

[seed]: https://gitlab.com/kicad/code/kicad/-/blob/10.0.8/qa/data/eeschema/same_local_global_power.kicad_sch
