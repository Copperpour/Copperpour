# Rotation field-text fixture

`rotate_fields_kicad10.kicad_sch` carries the cases a turn has to get right
for a symbol's Reference and Value text, so `rotate_schematic_component` can
be checked against an answer that is not its own.

A field's `(at …)` is an absolute sheet coordinate, not an offset from the
body. The sheet is therefore built in **twins**: a symbol placed unrotated,
and the same symbol placed at 90° outright 25.4mm below it. Turning the first
must land its fields exactly on the second's, less that 25.4mm.

## Provenance

Built through Copperpour against KiCad's stock `Device`, `Regulator_Linear`
libraries — `create_schematic`, `add_schematic_component`,
`edit_schematic_component`, `add_wire`, `add_schematic_net_label`,
`add_no_connect` — then parsed and force-resaved by KiCad 10.0.6:

```text
kicad-cli sch upgrade --force rotate_fields_kicad10.kicad_sch
```

What is committed is that Eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`, `(generator_version "10.0")`, KiCad's own library
records, field positions and UUIDs. KiCad left every field position exactly
where Copperpour had written it, so the resave changed the file's shape and not
its geometry.

## Cases

| Ref | Symbol | Body | Mirror | Role |
|---|---|---|---|---|
| `D1` | `Device:LED` | (101.6, 50.8) 0° | — | the LED whose anchors sit above and below the origin, where a 90° body goes |
| `D2` | `Device:LED` | (101.6, 76.2) 90° | — | `D1`'s twin, placed turned |
| `U1` | `Regulator_Linear:AP2112K-3.3` | (139.7, 50.8) 0° | `x` | an anchor off **both** axes, reflected — the only shape that tells a quarter turn from its reverse |
| `U2` | `Regulator_Linear:AP2112K-3.3` | (139.7, 76.2) 90° | `x` | `U1`'s twin |
| `U3` | `Regulator_Linear:AP2112K-3.3` | (177.8, 50.8) 0° | — | the same part unreflected: the control that makes the reflection case bite |
| `U4` | `Regulator_Linear:AP2112K-3.3` | (177.8, 76.2) 90° | — | `U3`'s twin |
| `D3` | `Device:LED` | (101.6, 101.6) 0° | — | Reference dragged to 10mm right and 10mm up of its anchor, via `edit_schematic_component` — a manual offset must turn, not reset |
| `R1` | `Device:R` | (101.6, 139.7) 0° | — | stands across the `NETJ` wire with a no-connect on pin 2: one turn owes fields, junctions and markers together |
| `R2` | `Device:R` | (93.98, 143.51) 0° | — | anchors `NETJ` at the wire's left end so the net survives |

`D3`'s and `R1`'s Reference fields are the two that are not on a library
anchor: `D3`'s was moved by hand, and `Device:R` anchors its Reference at a
stored angle of 90°, which the turn must leave alone.

## Oracle

`kicad-cli sch export svg` renders each field as an invisible `<text>` element
carrying its anchor, so KiCad itself says where the text lands. Turning `D1`,
`U1` and `U3` to 90° and re-exporting puts each one's anchor on its twin's, to
the last digit KiCad prints:

| Field | anchor as placed | anchor after the turn | twin's anchor | twin − turned |
|---|---|---|---|---|
| `D1` Reference | (101.6000, 48.8950) | (99.0600, 51.4349) | `D2` (99.0600, 76.8349) | (0, 25.4000) |
| `U1` Reference | (136.0012, 57.1499) | (133.9850, 47.7362) | `U2` (133.9850, 73.1362) | (0, 25.4000) |
| `U3` Reference | (174.1012, 45.7200) | (172.0850, 55.1338) | `U4` (172.0850, 80.5338) | (0, 25.4000) |

The last column is the 25.4mm that separates each twin's origin from its own,
and nothing else — so the comparison carries no constant for justification or
baseline, which are identical between a symbol and its twin.

`U1` and `U3` are the same part given the same turn, and their Reference ends
up on opposite sides of its own origin: the file has −5.08mm for the reflected
body against +5.08mm for the plain one. A turn that ignores the `(mirror x)`
token lands the reflected one where the plain one goes, which is what the
control test asserts. That pair is compared in file coordinates rather than
rendered ones, because KiCad flips a left-justified field's justification with
the reflection, so the two renders do not share a constant the way a twin pair
does.

`kicad-cli sch export netlist` is the oracle for `R1`'s turn:

| | `/NETJ` |
|---|---|
| as committed | `R2.1` |
| `R1` → 90°, pin 1 mid-span on the wire | `R2.1`, `R1.1` |

The no-connect on `R1` pin 2 travels from (101.6, 143.51) to (105.41, 139.7),
which is past the wire's right end at 102.87 — so the marker moves without
meeting the junction it would otherwise have to suppress. That boundary is
`no_connect_carry_kicad10`'s, not this fixture's.

## ERC

`kicad-cli sch erc --severity-all` reports 38 violations on the sheet as
committed: 24 `pin_not_connected`, 8 `power_pin_not_driven`, 4
`pin_not_driven`, 1 `isolated_pin_label` and 1 `unconnected_wire_endpoint`.
Every symbol here is placed to be measured, not wired, so that is the expected
state and not a defect the fixture is hiding.
