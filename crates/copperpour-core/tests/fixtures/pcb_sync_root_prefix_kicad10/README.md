# PCB sync root-prefix identity fixture ([#82](https://github.com/Copperpour/Copperpour/issues/82))

A small public hierarchical schematic fixture built with Copperpour MCP tools
(`create_project`, `add_schematic_component`, `add_hierarchical_sheet`,
`annotate_schematic` with `schematic_scope: "hierarchy"`) against stock KiCad
libraries (`Device:R`, `Device:C`), then resaved and netlisted with KiCad 10.0.8:

```bash
kicad-cli sch upgrade --force root.kicad_sch
kicad-cli sch upgrade --force stage.kicad_sch
kicad-cli sch upgrade --force leaf.kicad_sch
kicad-cli sch export netlist --format kicadsexpr -o root.net root.kicad_sch
```

## Hierarchy

- Root sheet `root.kicad_sch` (UUID `8631f762-28ca-4b46-9a77-580c49a715b8`) places:
  - `R1`: assigned `Device:R` (`10k`, `Resistor_SMD:R_0603_1608Metric`)
  - `R2`: unassigned `Device:R` (`UNASSIGNED`, empty `Footprint`)
  - Two instances of `stage.kicad_sch`: `StageA` (`6fdcbd51-c009-4892-813a-cd643e2b2f7a`)
    and `StageB` (`c311205d-f32d-47da-97d6-904ff928174e`).
- Reused sheet `stage.kicad_sch` places:
  - One `Device:R` (`1k`, `Resistor_SMD:R_0603_1608Metric`, symbol UUID `bc8b0001-33da-4719-a210-a76cec91947d`)
  - Two instances of nested sheet `leaf.kicad_sch`: `Leaf1` (`e7c7a854-5cc1-403d-8f70-a4e07226d83f`)
    and `Leaf2` (`b07d3f2c-88ec-482e-bf85-1c19a77fdf29`).
- Reused nested sheet `leaf.kicad_sch` places:
  - One `Device:C` (`100n`, `Capacitor_SMD:C_0603_1608Metric`, symbol UUID `111d511c-4af3-4705-a8cc-cf287640ecc3`).

## External oracle (`kicad-cli sch export netlist --format kicadsexpr`)

`kicad-cli` omits the root sheet UUID (`8631f762-28ca-4b46-9a77-580c49a715b8`) from
every exported `(sheetpath (tstamps ...))`, whereas schematic `(instances ...)` records
and rooted PCB footprint paths carry it:

| Ref | Value | Footprint | Sheet names | Exported root-relative path | Equivalent root-prefixed path |
| --- | --- | --- | --- | --- | --- |
| `R1` | `10k` | `Resistor_SMD:R_0603_1608Metric` | `/` | `/cb73633d-0588-4c05-8386-c6f9a836d1b8` | `/8631f762-28ca-4b46-9a77-580c49a715b8/cb73633d-0588-4c05-8386-c6f9a836d1b8` |
| `R2` | `UNASSIGNED` | *(unassigned)* | `/` | `/564f8de9-7101-4413-aa04-9c4146998ec0` | `/8631f762-28ca-4b46-9a77-580c49a715b8/564f8de9-7101-4413-aa04-9c4146998ec0` |
| `R3` | `1k` | `Resistor_SMD:R_0603_1608Metric` | `/StageA/` | `/6fdcbd51-c009-4892-813a-cd643e2b2f7a/bc8b0001-33da-4719-a210-a76cec91947d` | `/8631f762-28ca-4b46-9a77-580c49a715b8/6fdcbd51-c009-4892-813a-cd643e2b2f7a/bc8b0001-33da-4719-a210-a76cec91947d` |
| `R4` | `1k` | `Resistor_SMD:R_0603_1608Metric` | `/StageB/` | `/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d` | `/8631f762-28ca-4b46-9a77-580c49a715b8/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d` |
| `C1` | `100n` | `Capacitor_SMD:C_0603_1608Metric` | `/StageA/Leaf2/` | `/6fdcbd51-c009-4892-813a-cd643e2b2f7a/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3` | `/8631f762-28ca-4b46-9a77-580c49a715b8/6fdcbd51-c009-4892-813a-cd643e2b2f7a/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3` |
| `C2` | `100n` | `Capacitor_SMD:C_0603_1608Metric` | `/StageA/Leaf1/` | `/6fdcbd51-c009-4892-813a-cd643e2b2f7a/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3` | `/8631f762-28ca-4b46-9a77-580c49a715b8/6fdcbd51-c009-4892-813a-cd643e2b2f7a/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3` |
| `C3` | `100n` | `Capacitor_SMD:C_0603_1608Metric` | `/StageB/Leaf1/` | `/c311205d-f32d-47da-97d6-904ff928174e/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3` | `/8631f762-28ca-4b46-9a77-580c49a715b8/c311205d-f32d-47da-97d6-904ff928174e/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3` |
| `C4` | `100n` | `Capacitor_SMD:C_0603_1608Metric` | `/StageB/Leaf2/` | `/c311205d-f32d-47da-97d6-904ff928174e/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3` | `/8631f762-28ca-4b46-9a77-580c49a715b8/c311205d-f32d-47da-97d6-904ff928174e/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3` |

## SHA-256

| File | SHA-256 |
| --- | --- |
| `root.kicad_pro` | `31e674d04379de6f4c2629de463898fb863ef2bfd93aa61e9056f90c7764b999` |
| `root.kicad_sch` | `98b8a33debfccd41993cf8f0b9f90ef493275bed1325e0e85870f3d274aa688c` |
| `stage.kicad_sch` | `b343322c1c317031923aa5c8c26a99159cf9c0cee11026820adc24a13d76c85a` |
| `leaf.kicad_sch` | `a8e19308c4456e0dcee895decf4f912846dc53208a60f5c5f496c2e0df7ba03c` |
| `root.net` | `0d2cf730147b25dcf9ecc9992b6661826fadcddcdef826526f3543fd81e3d8ed` |
