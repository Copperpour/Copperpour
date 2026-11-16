# `set_footprint_field_style` fixtures

Two boards derived from `specctra_two_resistors.kicad_pcb` by KiCad itself, so
the tests of `set_footprint_field_style` (#520) need no string-edited board.
Each was loaded, changed and saved by pcbnew 10.0.8's Python on Windows 11:

```python
import pcbnew

def footprint(board, reference):
    found = [fp for fp in board.GetFootprints() if fp.GetReference() == reference]
    assert len(found) == 1
    return found[0]

board = pcbnew.LoadBoard("specctra_two_resistors.kicad_pcb")
footprint(board, "R2").SetReference("R1")
pcbnew.SaveBoard("field_style_duplicate_reference_kicad10.kicad_pcb", board)

board = pcbnew.LoadBoard("specctra_two_resistors.kicad_pcb")
footprint(board, "R1").Reference().SetTextThickness(0)
pcbnew.SaveBoard("field_style_no_thickness_kicad10.kicad_pcb", board)
```

Only the `.kicad_pcb` files are committed; the `.kicad_pro` and `.kicad_prl`
that `SaveBoard` writes beside them are not used.

The same KiCad saving `specctra_two_resistors.kicad_pcb` unchanged writes it
back byte for byte (KiCad 10.0.5 wrote it), so each file below differs from it
only where the change landed. Both keep its CRLF line endings.

- `field_style_duplicate_reference_kicad10.kicad_pcb`: two footprints carry
  the reference `R1`. The one line that differs is R2's
  `(property "Reference" "R2"`, now `"R1"`. Naming `R1` must be refused as
  `ambiguous_target` before the file is written.
- `field_style_no_thickness_kicad10.kicad_pcb`: R1's Reference stores no
  stroke thickness. KiCad writes a zero thickness by leaving the
  `(thickness 0.15)` line out of the field's `(font …)`; that line is the only
  difference. Setting R1's thickness to 0.15 must give back
  `specctra_two_resistors.kicad_pcb` byte for byte, which shows where KiCad
  writes a thickness and with which line ending.
