### `export_schematic_svg` honours the `output` filename (minor release)

`export_schematic_svg` used `output` only as a directory: kicad-cli names SVG
files after the schematic (`<stem>.svg`, one per sheet), so the caller's
filename was discarded and two exports of same-stem schematics into one
directory overwrote each other. The tool now behaves like
`export_schematic_pdf`: `output` names the root SVG file, and each child sheet
of a hierarchy is written beside it as `<output stem>-<sheetname>.svg`,
keeping kicad-cli's per-sheet convention under the caller's name
([#25](https://github.com/Copperpour/Copperpour/issues/25)).

The response keeps `exported`, now the requested root path, and adds `files`:
every published path, root first, then child sheets sorted by name. Callers
that reconstructed `<schematic stem>.svg` must read `exported` or `files`
instead. `get_schematic_view` also writes into its per-schematic
`<temp>/copperpour-schematic-views/<hash>/<stem>.svg` slot directory rather than
the shared parent directory, so two schematics with the same file stem no longer
overwrite each other's view.