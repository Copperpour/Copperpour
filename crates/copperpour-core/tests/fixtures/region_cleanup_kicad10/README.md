# Region cleanup fixtures

Three standalone one-sheet projects for an explicit "clean up this block" workflow: the
proposed cleanup preview, the single-write apply that follows it, and the layout score
that measures the result. Each `.kicad_pro` is the project file `create_project` wrote;
the symbol instances name that project, so keep each sheet beside it.

## Provenance

Drawn by tool calls, not by hand (`create_project`, `add_schematic_component`,
`add_power_symbol`, `batch_add_wire`, `add_schematic_net_label`, `set_schematic_page`)
against KiCad 10's stock `Device`, `power`, `Connector_Generic` and `Regulator_Linear`
libraries, then resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8) beside each
`.kicad_pro`, so each carries `(generator "eeschema")` and `(version 20260306)`. Nothing
was edited by hand. The resave kept every item, UUID, coordinate, property and symbol
instance (with its project name); it added KiCad's default fields, pin UUIDs and the
hidden `Intersheetrefs` field of the `SIG_A` global label. kicad-cli 10.0.8 on Linux gives
the same netlist, ERC and plotted drawing before and after the resave.

## `crowded/crowded.kicad_sch` (A4)

| Item | Placement | Notes |
|---|---|---|
| `U1` `AMS1117-3.3` | `(127, 76.2)` | VI (3) at `(119.38, 76.2)`, VO (2) at `(134.62, 76.2)`, GND (1) at `(127, 83.82)`. `+5V`, `+3V3` and `GND` symbols sit on the pin tips. |
| `C1` `10u` | `(63.5, 109.22)` | Input decoupling, about 64 mm from U1. Its `GND` symbol `#PWR05` sits on pin 2 at `(63.5, 113.03)`. |
| C1.1 to U1.3 | six wires | `(63.5,105.41)` → `(63.5,96.52)` → `(88.9,96.52)` → `(88.9,63.5)` → `(106.68,63.5)` → `(106.68,76.2)` → `(119.38,76.2)`: 110.49 mm against a Manhattan distance of 85.09 mm. Branch-free: no label, junction or third pin along it. |
| `SIG_A` global label | `(83.82, 81.28)`, angle 0 | On J1.1. The run's vertical leg at x = 88.9 passes through its lettering without touching the anchor. |
| `C2` `10u` | `(139.7, 80.01)` | Output decoupling, already beside U1 on a 5.08 mm wire. The control: nothing to improve. |
| `J1` `Conn_01x02`, `#FLG01`, `#FLG02` | `(66.04, 81.28)` mirrored; flags at x = 45.72 | Give the rails a driver so ERC stays quiet. |

## `constrained/constrained.kicad_sch` (A4)

The same U1, C1 and six-wire run, without J1 and C2. `R2` at `(67.31, 88.9)`, rotated 90°,
puts pin 1 at `(63.5, 88.9)`, on the vertical-first L route from C1.1 to U1.3. `R3` at
`(101.6, 109.22)` puts pin 1 at `(101.6, 105.41)`, on the horizontal-first one. Both pins
carry a local `SIG_B` label, so either route would join `SIG_B` to `+5V`. Treat C1 as
explicitly placed (not movable) here.

## `sparse/sparse.kicad_sch` (A3)

An R1/R2 divider from `+5V` to `GND` with `SIG_C` on the midpoint, all within 16 × 23 mm
in the top-left corner of an A3 sheet. Every connection is already straight. Nothing
should change.

## KiCad's answer (kicad-cli 10.0.8)

`kicad-cli sch export netlist`:

| Sheet | Net | Pins |
|---|---|---|
| crowded | `+5V` | C1.1 U1.3 |
| crowded | `+3V3` | C2.1 U1.2 |
| crowded | `GND` | C1.2 C2.2 J1.2 U1.1 |
| crowded | `SIG_A` | J1.1 |
| constrained | `+5V` | C1.1 U1.3 |
| constrained | `+3V3` | U1.2 |
| constrained | `/SIG_B` | R2.1 R3.1 |
| constrained | `GND` | C1.2 R2.2 R3.2 U1.1 |
| sparse | `+5V` / `/SIG_C` / `GND` | R1.1 / R1.2 R2.1 / R2.2 |

`kicad-cli sch erc --severity-all`: crowded reports one `isolated_pin_label` warning
(`SIG_A` reaches only J1.1); constrained and sparse report nothing.

### After a three-call cleanup with today's tools

On `crowded`, with `main` at `456a8a66`: `batch_delete_schematic_wire` (the six wires),
`move_region` with `include: "all"` over `(58, 100)`-`(69, 118)` by `(50.8, -29.21)`, then
`connect_pins C1 1 U1 3`. KiCad's netlist of the result differs from the one above only in
its `source` and `date` lines. Component `tstamps` and `sheetpath` are unchanged, so are
the symbol UUIDs and instance paths, and ERC still reports the same single warning. C1
ends at `(114.3, 80.01)` and `#PWR05` at `(114.3, 83.82)`; nothing else moved. The new
wire runs `(114.3, 76.2)`-`(119.38, 76.2)`, and `connect_pins` adds a junction dot at
U1.3, where the wire, the pin and the `+5V` symbol's pin meet.
