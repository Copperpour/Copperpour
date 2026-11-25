# PCB sync multi-unit and keyed-header fixture

A small public schematic reproducing the two refusals of `update_pcb_from_schematic`
on a real board: a multi-unit symbol whose footprint is linked to a later unit, and
a keyed header whose footprint has no pad at the key, where the schematic pin has a
no-connect flag.

Built with Copperpour MCP tools (`create_project`, `add_schematic_component`,
`connect_pins`, `add_no_connect`, `edit_schematic_component`) against stock
KiCad libraries, then resaved and netlisted with KiCad 10.0.8:

```bash
kicad-cli sch upgrade --force kk_sync.kicad_sch
kicad-cli sch export netlist --format kicadsexpr -o kk_sync.net kk_sync.kicad_sch
```

`kk.pretty/KK254_1x04_Keyed.kicad_mod` is KiCad's
`Connector_Molex:Molex_KK-254_AE-6410-04A_1x04_P2.54mm_Vertical` with pad 2 removed
and renamed, resaved with `kicad-cli fp upgrade --force kk.pretty`. `fp-lib-table`
names it `kk`.

## Schematic

| Ref | Symbol | Footprint | Placed units |
| --- | --- | --- | --- |
| `U1` | `Comparator:LM339` (`LM339`) | `Package_DIP:DIP-14_W7.62mm` | 1 (`d6286f98-cc91-4832-97c0-cec9c712e8d3`), 2 (`ea566a33-6106-497d-ab3a-228e365858d7`), 5, the power unit (`1951944c-c6bb-40ae-8bca-507fb207334e`) |
| `R1` | `Device:R` (`2k2`, `MPN` `RC0603FR-072K2L`) | `Resistor_SMD:R_0603_1608Metric` | 1 |
| `P1` | `Connector_Generic:Conn_01x04` | `kk:KK254_1x04_Keyed` (pads 1, 3, 4) | 1 |

Wires: `R1` pin 1 to `U1` pin 2, `R1` pin 2 to `P1` pin 1, `P1` pin 3 to `U1` pin 4.
`P1` pin 2 has a no-connect flag; `P1` pin 4 is wired to nothing.

## External oracle (`kicad-cli sch export netlist --format kicadsexpr`)

- `U1`'s `(tstamps …)` lists the three placed units' UUIDs, in the order above,
  across two lines.
- `P1` pin 2 is alone on `unconnected-(P1-Pin_2-Pad2)` with pin type
  `passive+no_connect`, and `P1` pin 4 alone on `unconnected-(P1-Pin_4-Pad4)` with
  pin type `passive`. Every unwired pin of `U1` is alone on an `unconnected-…` net.

On a board made from it, with `U1`'s footprint linked to unit 2, KiCad 10.0.8's
`kicad-cli pcb drc --schematic-parity` reports one warning: `net_conflict`, "No pad
found for pin 2 in schematic", on `P1`. It reports nothing about `U1`.

## SHA-256

| File | SHA-256 |
| --- | --- |
| `kk_sync.kicad_pro` | `2dbe50017a4a0162cd98e637548532d8f50a91aa6afd3d61377ad26003ccf18a` |
| `kk_sync.kicad_sch` | `30b15ec2912105d15a935ce6d320f97fd958f7ddc720106074c267929c45ada9` |
| `kk_sync.net` | `67299eb7288350f86530f395e2298037b896532428e97becdc5e74df39b48f32` |
| `fp-lib-table` | `aa053536fc9a8d3107be1390e9a4aeaa4a7ffe26e170bea73e0c3572aabfcb4e` |
| `kk.pretty/KK254_1x04_Keyed.kicad_mod` | `ceb8dc3c910dac82b88e1e359967b4d96874313d8f5dc8b4d8da84346da30998` |
