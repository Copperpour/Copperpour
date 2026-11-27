# Saved label attachment

These sheets retain J2, its embedded `Connector_Generic:Conn_01x02` definition,
and the output trunk from the [op-amp fixture](../block_placement_kicad10/README.md).
The input copies add a local `TOLERANT_OUT` label and were resaved with
`kicad-cli sch upgrade --force` in KiCad 10.0.8 beside the original
`opamp_stage.kicad_pro`. The tests copy that same project beside each sheet.

| Case | Label anchor (mm) | J2.1 in KiCad 10.0.6 and 10.0.8 netlists |
| --- | --- | --- |
| `exact` | (165.1, 88.9) | `/TOLERANT_OUT` |
| `offset` | (165.1, 88.901) | `unconnected-(J2-Pin_1-Pad1)` |
| `endpoint-offset` | (190.5, 88.9004) | `unconnected-(J2-Pin_1-Pad1)` |
| `one-unit` | (165.1, 88.9001) | `/TOLERANT_OUT` |
| `two-units` | (165.1, 88.9002) | `unconnected-(J2-Pin_1-Pad1)` |

The rounded input changes only the exact label's `y` to `88.90004`. Both versions
give it the same netlist and electrical ERC findings as the exact input.
KiCad 10.0.8 resaves it to the exact fixture, so no duplicate sheet is stored.
Both offset inputs give `label_dangling`; the exact and rounded inputs do not.
KiCad 10.0.8 also reports a library-check finding on all copies in an environment
without the project library tables. The electrical findings agree after scaling
the KiCad 10.0.6 ERC coordinates by 100.

The three topology controls extend J2.2 with a second wire run. In `crossing`
the runs cross without a dot, and both versions export only J2.1 on
`/TOLERANT_OUT`. In `junction` a dot connects the runs at the crossing. In
`name-join` the second run stops before the crossing and has its own
`TOLERANT_OUT` label. Both versions export J2.1 and J2.2 on `/TOLERANT_OUT` in
both controls. The controls use the same KiCad 10.0.8 resave.

## Power pin witnesses

`power-rounded-pin` puts a stock GND symbol on J2.1's saved tip at
`(190.5, 88.9)`. `power-wrong-pin-point` moves GND to `(190.5001, 88.9)`.
Both sheets were resaved with `kicad-cli sch upgrade --force` in KiCad 10.0.8.
Both KiCad versions report J2.1 on GND in the first sheet and disconnected
in the second. Adding `0.00004 mm` each to the connector origin and library
pin offset leaves those results unchanged: KiCad rounds them separately.

## Route guard witness

`shallow-wire-route` starts from `exact`, with a shallow diagonal trunk, an
exact junction at `(162.56, 88.9002)`, and a separate `AAA_SOURCE` stub.
KiCad 10.0.8 resaved the sheet. Both 10.0.6 and 10.0.8 export J2.1 on
`/TOLERANT_OUT` before routing and on `AAA_SOURCE` if a vertical wire is
drawn from `(162.56, 80.01)` through the junction to `(162.56, 91.44)`.

### One-unit junction

The `junction-1iu` review probe moves only the junction in `junction` to
`(152.4, 88.9001)`, one saved unit from the horizontal trunk. KiCad 10.0.6
and 10.0.8 resaved it byte for byte alike with `kicad-cli sch upgrade --force`.
Both resaves export J2.1 on `/TOLERANT_OUT` and J2.2 on its unconnected net.

## Diagonal witnesses

The sheets were resaved with KiCad 10.0.8. Moving the trunk's left end to (134.62,
88.9048) gives a hit at label y = 88.9023 and a miss at y = 88.9025 on both 10.0.6
and 10.0.8. In `precision-before`, both versions attach `REVIEW_BOUNDARY` to the
proposed shallow diagonal at the rounding cutoff. The route join calculation
tests that candidate without removing the public router's diagonal refusal.
The existing `shallow-wire-route` fixture exercises the exact junction rule.

## Pin label witnesses

The connector sheets were prepared through Copperpour and resaved with
`kicad-cli sch upgrade --force`. KiCad 10.0.6 and 10.0.8 produced identical files.
With label A on J2.1 or a no-connect, both versions leave J3.1 on B when only one
wire passes. With two overlapping wires, both versions put J3.1 on A and report
`label_multiple_wires`. The route sheet retains two extra wire legs and mirrors J2
so pin 2 does not independently block the route.
