### `create_symbol` refuses a name the library already defines (patch release)

`create_symbol` used to append a symbol whose name the library already held
and report success. The library then had two top-level symbols of that name,
and KiCad's next save kept only one of them, without saying which. Now such a
call is `invalid_argument` on `name` and the library is left as it was
([#123](https://github.com/Copperpour/Copperpour/issues/123)). A name held
only by a unit sub-symbol (`NAME_0_1`) is still free. To replace a symbol,
call `delete_symbol` first. It removes one copy per call, so a library an
earlier release left holding two copies needs it twice. A library the parser
cannot read is now refused too, instead of having the symbol appended to it.

The write is now revision-checked: a library saved by another program between
`create_symbol`'s read and its write, or created there after the call found
none, is refused as `conflict` and left as saved. Read the library again and
retry.
