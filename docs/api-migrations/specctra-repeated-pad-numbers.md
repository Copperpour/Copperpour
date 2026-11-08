### `export_specctra_dsn` exports pads that share a number (minor release)

A footprint whose pads share a number, such as a connector's two `MP` pads, was
refused with `footprint '<reference>' has duplicate pad number '<number>'`
([#338](https://github.com/Copperpour/Copperpour/issues/338)). Each such pad is
now its own DSN pin, on its own net. The first, by pad UUID, keeps the number as
its name, and the others get `@1`, `@2`, as KiCad's own Specctra export names
them: `J1-MP` and `J1-MP@1`. Pads with a number of their own are named as
before.

The reverse manifest's `schema_version` is now 2. Each pin adds `pad_uuid`, the
pad's KiCad UUID, which says which physical pad a DSN pin is.
`plan_specctra_ses_import` and `apply_specctra_ses` now check every manifest pin
against the live board's pads, by UUID, number, net and name, and refuse a
mismatch before anything is planned. They still read a schema 1 manifest,
checked by number. A board with a pad without a UUID, or a UUID two pads share,
is refused, and so is a pad number equal to a name the suffix would give.

With `native_bridge_mode` `prefer` or `require`, a board with such pads is not
adopted from KiCad's native DSN: KiCad names them by file order, which isn't
proven to map one to one. `prefer` uses Copperpour's export; `require` refuses.
