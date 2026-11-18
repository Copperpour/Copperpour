### `get_symbol_info` reads a `.kicad_sym` file by path (minor release)

`get_symbol_info` took only `lib_id`, resolved through the project and global
symbol library tables, so a library no table registers could not be read; a
path in `lib_id` was taken as a library nickname. It now also takes
`library_path`, a `.kicad_sym` file, with `symbol_name`, as the library editing
tools do and as `get_footprint_info` takes a `.kicad_mod` path.

- `lib_id` is no longer required: give it, or `library_path` with
  `symbol_name`. Giving both, `library_path` without `symbol_name`, or
  `symbol_name` with `lib_id` is refused as `invalid_argument`; a missing file
  is `file_not_found`.
- An answer read by path carries `library_path` instead of `lib_id` and
  `library`, with the same `name`, `pin_count`, `pins` and `properties`.

Calls with `lib_id` are unchanged.
