### Board writers write a layer's display name by its canonical name, and refuse `In01.Cu` (patch release)

`add_board_text`, `import_svg_logo`, `add_zone` and `add_copper_pour` accept
KiCad's display names for a layer (`F.Silkscreen`, `User.Drawings`,
`User.Comments`), but their board-file fallback wrote the name as given, and
KiCad 10 refuses to load a board whose item names a layer that way
([#99](https://github.com/Copperpour/Copperpour/issues/99)). They now write
the canonical name (`F.SilkS`, `Dwgs.User`, `Cmts.User`), and the response's
`layer` reports that name on both the IPC and the file path. A caller that
compared `layer` with the display name it sent should compare with the
canonical name.

The same four tools now refuse a copper or user layer number KiCad never
writes, such as `In01.Cu`, `In+1.Cu` or `User.01`, as `invalid_argument`
naming `layer`, before KiCad or the file is touched. They used to read it as
`In1.Cu` or `User.1`, and the file fallback wrote it verbatim. Write `In1.Cu`
or `User.1`.
