# Stub obstacles fixture

`stub_obstacles_kicad10.kicad_sch` puts another pin where a `connect_to_net`
or `batch_connect_to_net` stub ends, or on the stub's span (#117).

## Provenance

Built through Copperpour (`main` @ `4759e5d3`) against KiCad's stock `Device`
library with `create_schematic`, `add_schematic_component` and
`add_schematic_net_label`, then force-resaved by KiCad 10.0.6 beside a minimal
`stub_obstacles_kicad10.kicad_pro`:

```text
kicad-cli sch upgrade --force stub_obstacles_kicad10.kicad_sch
```

The file is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, SHA-256 `b24d9639…67f5`. Every part is `Device:R`, at
rotation 0 unless the table says otherwise, so pin 1 is at `y − 3.81`, pin 2 at
`y + 3.81`, and pin 2's stub goes `down` under `auto`. The sheet has no wires.

| Group | Parts | Stub | In the way |
|---|---|---|---|
| issue | R1 (50.8, 50.8), R2 (50.8, 60.96) | R1.2 (50.8, 54.61), 2.54 down | R2.1 at the stub end. The issue's reproduction. |
| span | R3 (76.2, 50.8), R4 (76.2, 63.5) | R3.2 (76.2, 54.61), 7.62 down | R4.1 (76.2, 59.69) mid-span |
| boxed | R5 (101.6, 50.8), R6 (101.6, 60.96), R7 (104.14, 58.42), R8 (99.06, 58.42); label `OTHER` on R6.1 | R5.2 (101.6, 54.61), 2.54 | R6.1 (net `OTHER`) down, R7.1 right, R8.1 left |
| same | R9 (127, 50.8), R10 (127, 60.96); label `N` on R10.1 | R9.2 (127, 54.61), 2.54 down | R10.1, already on net `N` |
| left | R11 (152.4, 50.8) at rotation 90, R12 (146.05, 54.61) | R11.1 (148.59, 50.8), facing left, 2.54 left | R12.1 at the stub end |

`kicad-cli sch export netlist` names `/N` (R10.1) and `/OTHER` (R6.1); every
other pin is `unconnected-…`. `kicad-cli sch erc --severity-all` reports 22
`pin_not_connected` and 2 `isolated_pin_label`.

## The oracle: KiCad's netlist of each result

Each call ran on a copy of the fixture, driven over stdio. Nets with a single
pin and no name are omitted; `/N` (R10.1) and `/OTHER` (R6.1) are omitted where
unchanged.

| Call | `main` @ `4759e5d3` | This change |
|---|---|---|
| `connect_to_net` R1.2 `N2` | `down`; `/N2` R1.2 **R2.1** | `right`, wire (50.8, 54.61)→(53.34, 54.61); `/N2` R1.2 |
| the same by `pin_x`/`pin_y` | as above | as above |
| the same, `direction: "down"` | `/N2` R1.2 **R2.1** | `plan_blocked`, nothing written |
| `connect_to_net` R3.2 `N3`, `stub_length` 7.62 | `down`; `/N3` R3.2 **R4.1** | `right`, wire (76.2, 54.61)→(83.82, 54.61); `/N3` R3.2 |
| `connect_to_net` R5.2 `N5` | `down`; `/N5` R5.2 **R6.1**, and `/OTHER` is gone | `plan_blocked`, one reason each for `down`, `right`, `left`; nothing written |
| `connect_to_net` R9.2 `N` | `/N` R9.2 R10.1 | the same |
| `connect_to_net` R11.1 `N11` | `left`; `/N11` R11.1 **R12.1** | `up`, wire (148.59, 50.8)→(148.59, 48.26), label rotation 180 so its text runs left, away from R11; `/N11` R11.1 |
| `batch_connect_to_net` `N2` down 2.54, R1.2 R5.2 | `/N2` R1.2 **R2.1** R5.2 **R6.1** | both skipped and reported in `errors`, `isError` true, nothing written |
| `batch_connect_to_net` `N2` down 2.54, R1.2 R2.1 | `/N2` R1.2 R2.1 | the same: R2.1 is one of the pins asked for |

The bold pins are the ones `main` joined without being asked, while reporting
success. The reasons for R5.2:

```text
the down stub would join R6 pin 1 and label OTHER at (101.6, 57.15) on net OTHER
the right stub would join R7 pin 1 at (104.14, 54.61)
the left stub would join R8 pin 1 at (99.06, 54.61)
```
