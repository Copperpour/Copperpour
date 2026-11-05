### `replace_component` embeds a symbol once per sheet (patch release)

`replace_component` looked for the new symbol's embedded copy by the text
`(symbol "<lib_id>"`, but the copy it and `update_symbols_from_library` write
has the name on the line after `(symbol`. So the second part replaced by one
symbol embedded another copy in `lib_symbols`
([#315](https://github.com/Copperpour/Copperpour/issues/315)). It now finds
the copy among `lib_symbols`' children by name, as
`update_symbols_from_library` does, wherever the name sits. A sheet that
already holds two copies keeps them until KiCad saves it, which keeps one.
Arguments and response are unchanged.
