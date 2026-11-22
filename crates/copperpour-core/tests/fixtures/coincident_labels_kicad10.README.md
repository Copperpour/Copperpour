# Coincident label fixture

`coincident_labels_kicad10.kicad_sch` is `label_move_kicad10.kicad_sch` with a
plain label `PWR` added on the global label `PWR`'s anchor: two labels of one
name at one point, which a net name and a position cannot tell apart.

## Provenance

`label_move_kicad10.kicad_sch` was made the root sheet of a project from
Copperpour's `create_project`, and Copperpour's `add_schematic_net_label` added
the plain label at (114.3, 105.41). The sheet alone was then force-resaved by
KiCad 10.0.8:

```text
kicad-cli sch upgrade --force coincident_labels_kicad10.kicad_sch
```

A second resave changed no byte. The plain label is the only difference from
`label_move_kicad10.kicad_sch`.

## Cases

| Item | uuid | Position |
|---|---|---|
| `label "(at 1 2)"` | `3031ddf4-4f23-44fc-bceb-58bba2ec8575` | (114.3, 97.79) |
| `global_label "PWR"` | `c6653f40-dca2-48f0-85fa-e9e371c6a1b0` | (114.3, 105.41) |
| `label "PWR"` | `0d4d3a43-7e99-4613-b6d3-aa71f4b45928` | (114.3, 105.41) |

## Oracle

`kicad-cli sch export netlist` puts `R1.1` on `/(at 1 2)` and `R1.2` on `PWR`,
as for `label_move_kicad10.kicad_sch`.

## ERC

`kicad-cli sch erc --severity-all` reports 7: 2 `unconnected_wire_endpoint`
("Unconnected wire endpoint"), 3 `isolated_pin_label` ("Label connected to only
one pin"), 1 `same_local_global_label` ("Local and global labels have same
name", the pair), and 1 `lib_symbol_issues` ("The current configuration does
not include the symbol library 'Device'"). The sheet exists to be rotated, not
wired.
