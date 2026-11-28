# Wire deletion junction fixture

This standalone sheet isolates the saved junctions used by the wire deletion
tests. The cases share a sheet so the tests also check that unrelated dots
remain byte-for-byte equivalent as parsed records, including their UUIDs.

## Provenance

Start with a copy of `wire_end_junctions_kicad10.kicad_sch`. Remove its child
sheet, bus and bus entry. Replace `U1` with a small three-pin test symbol and
move it to `(114.3, 105.41)`. Place three more instances of that symbol for the
nearby-pin, close-pin and unmarked-crossing cases. Keep the original `R1` for the
diagonal netlist check. The two embedded definitions are both in use.

The `Fixture:ClosePins` passive pins have local tips at `(0, 3.815)`,
`(0, 3.81)` and `(0, -3.81)`, in that order. Pin 1 is hidden. Recreate the
wires, labels and dots through Copperpour's schematic tools. Off-grid wires
can be made with a coordinate-based `connect_to_net` stub and its temporary
label removed. For nearby dots, add each away from its neighbor and
move it into position with `move_region`.

The committed bytes are the output of KiCad 10.0.8 on Linux, with no later
text edits:

```sh
kicad-cli sch upgrade --force wire_delete_dots_kicad10.kicad_sch
```

## Cases

| Saved dot (mm) | What the deletion tests distinguish |
|---|---|
| `(114.3, 50.8)` | A cross retains its dot after losing one arm, but loses it after losing two or all four. |
| `(114.305, 50.805)` | A dot within the old tolerance that is off every cross arm. |
| `(38.1, 50.8)` and `(63.5, 50.8)` | A diagonal branch ends at a dot using either endpoint order. |
| `(38.105, 50.8)` | A nearby dot is on the surviving horizontal arm, not the deleted diagonal. |
| `(88.9, 50.8)` | An orthogonal rail passes through the junction rather than ending there. |
| `(114.3, 101.6)` | A surviving diagonal passes through the dot to `R1.1`; the other surviving branch carries `DIAG`. |
| `(101.6, 102.87)` | Removing that diagonal does not select a dot inside its span. |
| `(38.1, 76.2)` | A rail starts 0.006 mm before the dot and continues through it. The surviving branch needs the dot. |
| `(63.5, 88.9)` | The rail starts 0.006 mm before `U3.2`. Tolerant pin deduplication keeps the nearby `U3.1` tip instead of the exact `U3.2` tip. |
| `(88.9, 96.52)` | A rail ends 0.006 mm past the dot. The nearby `U2` pin and its wire lie off both surviving segments. |
| `(88.914, 96.51)` | An unrelated dot is also close to the deleted `U2` pin but does not touch it or the deleted wire. |
| `(88.905, 104.136)` | A corner loses its dot when the same batch deletes the exact `U2.3` pin. |
| None at `(152.4, 105.417)` | `CROSS_H` and `CROSS_V` cross 0.006 mm from each wire's end. Deleting `U4` and an unrelated wire must not connect them through a new dot. |
