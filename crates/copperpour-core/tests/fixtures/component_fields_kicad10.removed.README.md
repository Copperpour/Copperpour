# Component properties fixture, `MPN` removed from `U1`

`component_fields_kicad10.removed.kicad_sch` is
`component_fields_kicad10.kicad_sch` after `edit_schematic_component` with
`reference` `U1` and `remove_fields` `["MPN"]`, so the removal can be checked
against bytes KiCad wrote rather than an expectation built in the test.

## Provenance

The tool's output was saved beside a project file named
`component_fields_kicad10.kicad_pro`, as the source fixture was, and parsed and
force-resaved by KiCad 10.0.6:

```text
kicad-cli sch upgrade --force component_fields_kicad10.kicad_sch
```

The resave is byte-identical to the tool's output, so the committed file is
both at once. The same command run on the unedited source fixture gives back
its bytes unchanged, which is what makes the comparison a fair one.

Without the project file KiCad loads the schematic into its default project,
whose name is empty, and writes `(project "")` in each of the four
`(instances …)` blocks. It does that to the unedited source fixture too, so it
is a property of the resave, not of the removal.

## Cases

Against the source fixture, the only change is the deletion of 33 lines: the
three `(property "MPN" "MPN-679-U1" …)` blocks, one in each of `U1`'s units,
each with its own line's indentation and newline. Nothing is added, and no
other line moves.

| Component | Units | `LCSC` | `MPN` |
|---|---|---|---|
| `R1` `Device:R` | 1 | `C679001` | `MPN-679-R1`, kept |
| `U1` `Amplifier_Operational:LM358` | 1, 2, 3 | `C679002` | removed from every unit |

## KiCad's own answer

```text
kicad-cli sch export netlist --output component_fields.net component_fields_kicad10.removed.kicad_sch
```

| `comp` | `LCSC` | `MPN` |
|---|---|---|
| `R1` | `C679001` | `MPN-679-R1` |
| `U1` | `C679002` | none |
