### `create_footprint` refuses an existing `.kicad_mod` unless `replace_existing` is true (minor release)

`create_footprint` now refuses with `conflict` when `output` already exists on
disk instead of silently overwriting it. Pass `replace_existing: true` to
replace an existing `.kicad_mod` file (checked with `write_if_unchanged` against
the bytes read before writing). `import_jlcpcb_part` passes
`replace_existing: true` automatically so part re-imports keep working.
