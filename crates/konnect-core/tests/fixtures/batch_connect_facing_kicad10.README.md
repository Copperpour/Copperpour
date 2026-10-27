# Facing stubs fixture

`batch_connect_facing_kicad10.kicad_sch` sets up two ways a
`batch_connect_to_net` stub can end on a spot the net already labels:

- two pins that face each other 5.08 mm apart, so their 2.54 mm stubs meet
- a pin whose stub end already carries a label of the net, with no wire to it

In both cases each pin still needs its own wire.

## Provenance

Built through Copperpour against KiCad's stock `Device` library with
`create_schematic`, `add_schematic_component` and `add_schematic_net_label`,
then force-resaved by KiCad 10.0.6:

```text
kicad-cli sch upgrade --force batch_connect_facing_kicad10.kicad_sch
```

| Item | Placement | Relevant pin |
|---|---|---|
| `R1` `Device:R` | `(63.5, 63.5)`, rotated 90° | 2 at (67.31, 63.5), faces right |
| `R2` `Device:R` | `(76.2, 63.5)`, rotated 90° | 1 at (72.39, 63.5), faces left; 2 at (80.01, 63.5), faces right |
| label `N` | `(82.55, 63.5)` | where R2.2's 2.54 mm stub ends |

`kicad-cli sch erc --severity-all` reports 4 `pin_not_connected` and 1
`label_dangling` on the fixture itself. Nothing is wired, so this is expected.

## Case

`batch_connect_to_net` with `net: "N"`, `stub_length: 2.54` and pins R1.2,
R2.1 and R2.2.

The oracle is `main` @ `b37476b`'s `connect_to_net`, one call per pin, which
draws the three wires below:

| Pin | Wire |
|---|---|
| R1.2 | (67.31, 63.5) → (69.85, 63.5) |
| R2.1 | (72.39, 63.5) → (69.85, 63.5) |
| R2.2 | (80.01, 63.5) → (82.55, 63.5) |

`connect_to_net` also stacks a second label at (69.85, 63.5) and at
(82.55, 63.5). The batch writes one label per spot instead. It adds a label at
(69.85, 63.5) and reuses the fixture's label at (82.55, 63.5).

`kicad-cli sch export netlist` on each result:

| File | `/N` |
|---|---|
| `main` `connect_to_net` × 3 | R1.2 R2.1 R2.2 |
| this branch's batch | R1.2 R2.1 R2.2 |
| the batch at `0feacd2`, before this fix | R1.2 only: R2.1 and R2.2 were marked `deduplicated` and got no wire |
