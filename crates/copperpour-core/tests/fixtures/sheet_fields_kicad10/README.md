# Sheet fields fixture

A root sheet that places one child file four times, as KiCad's multichannel
pattern does, with some of the placements' name and file fields hidden. It
backs `edit_sheet`'s `hide_name` and `hide_file`.

## Provenance

Built through Copperpour with `create_project`, four `add_hierarchical_sheet`
calls naming `channel.kicad_sch`, and `edit_sheet` with `hide_file` on `CH1`,
`hide_file` and `hide_name` on `CH2`, and `hide_name` true then false on `CH4`.
Then each sheet was parsed and force-resaved by KiCad 10.0.6, one file at a time
and with no project file beside it, so the instances read `(project "")`:

```text
kicad-cli sch upgrade --force sheet_fields.kicad_sch
kicad-cli sch upgrade --force channel.kicad_sch
```

What is committed is that Eeschema serialization, `(generator "eeschema")` and
`(version 20260306)`. A second `upgrade --force` changes no byte.

## Fields

| Sheet | Name text | File text |
|-------|-----------|-----------|
| `CH1` | shown | hidden |
| `CH2` | hidden | hidden |
| `CH3` | shown | shown |
| `CH4` | shown | shown |

KiCad writes a hidden field as `(hide yes)` right after the property's
`(at …)`. `kicad-cli sch export svg` of the root draws `CH1`, `CH3` and `CH4`
with their names, `CH3` and `CH4` with their file names, and `CH2` with
neither.

## Oracle

`edit_sheet` run on a copy with `CH3` `hide_file: true`, `CH2` `hide_name:
false` and `hide_file: false`, and `CH1` `hide_name: true`, then
`kicad-cli sch upgrade --force`: the file differs from the fixture by those four
`(hide yes)` lines and no others. The tool writes the sheet it edits in the
editor's own order, `(fields_autoplaced)` ahead of `(exclude_from_sim …)`, which
KiCad's resave puts back.
