# Schematic note plot edge cases

`schematic_note_plot_edges_kicad10.kicad_sch` was created with Copperpour's
`create_schematic` and `add_schematic_text`, then resaved with
`kicad-cli sch upgrade --force` in KiCad 10.0.8. The resave preserved the text,
positions, UUIDs, and font settings.

The regression uses `kicad-cli sch export svg` from KiCad 10.0.8 with default plot
settings. The expected bounds enclose the plotted paths plus half the stroke width.

| Saved note | What KiCad plots |
| --- | --- |
| `Ends here\n` | No extra pitch from the terminal newline, even with bottom justification |
| `Thin stroke` | The 0.005 mm thickness plots with a 0.0847 mm pen |
| `Automatic pen` | The 0.0001 mm thickness plots with the 0.1524 mm default pen |
| `Custom pen` | The explicit 0.3 mm thickness, not the default or implicit bold thickness |
| `Expression @{1 + 2}` | `Expression 3`, not the literal source |
