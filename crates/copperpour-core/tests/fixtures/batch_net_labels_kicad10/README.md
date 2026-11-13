# Batch net labels fixture

A two-sheet project for `batch_add_schematic_net_label`: the child sheet
`interface` holds a four-pin connector whose pins become the sheet's
hierarchical labels in one call.

## Provenance

Built through Copperpour with `create_project`, `add_hierarchical_sheet` and
`add_schematic_component`, against KiCad 10.0.6's stock `Connector_Generic`
library, then force-resaved by KiCad 10.0.6, one file at a time since `upgrade`
converts only the file it is given:

```text
kicad-cli sch upgrade --force batch_net_labels.kicad_sch
kicad-cli sch upgrade --force interface.kicad_sch
```

What is committed is that Eeschema serialization, `(generator "eeschema")` and
`(version 20260306)`, beside the project file `create_project` wrote, so the
child sheet's instance reads `(project "batch_net_labels")` and its writes are
guarded by the root's editor lock.

## Items

| Item | Sheet | Where | Role |
|---|---|---|---|
| sheet `interface` | root | box at (50.8, 50.8), 30.48 × 25.4 | no sheet pins yet |
| `J2`, `Connector_Generic:Conn_01x04` | root | (152.4, 63.5) | pins 1–4 at x 147.32, y 60.96 to 68.58 |
| `J1`, `Connector_Generic:Conn_01x04` | `interface` | (101.6, 101.6) | pins 1–4 at x 96.52, y 99.06 to 106.68, pointing left |

## Oracle

`interface.labelled.kicad_sch` is `interface.kicad_sch` after one
`batch_add_schematic_net_label` call put a hierarchical label on each of J1's
pins, turned 180 to point away from it: `SDA` bidirectional, `SCL` input, `INT`
output and `EN` with no shape given. The call added those four blocks and changed
no other byte, and KiCad's resave of it changed none.

`kicad-cli sch erc --severity-all` on the project, with KiCad's default checks
(the project file sets none):

| | errors | warnings |
|---|---|---|
| as committed | 8 `pin_not_connected`, J1's and J2's pins | 0 |
| child labelled | 4 `pin_not_connected` (J2) and 4 `hier_label_mismatch`, one per label | 4 `isolated_pin_label` |

Then `import_sheet_pins` on the root (side `right`) gave the sheet four pins of
the labels' shapes, and one `batch_add_schematic_net_label` call put a plain
label on each sheet pin and on each of J2's pins. ERC then reports nothing, and
`kicad-cli sch export netlist` joins J1 and J2 pin for pin in nets `/SDA`,
`/SCL`, `/INT` and `/EN`. That end state is not committed.
