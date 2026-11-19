### `get_symbol_info` refuses `project_dir` with `library_path` (patch release)

`project_dir` resolves a `lib_id`'s library through the project's symbol
library table. With `library_path`, which names the file itself, it was
accepted and ignored. It is now refused as `invalid_argument` on
`project_dir`, as `lib_id` with `library_path` and `symbol_name` with `lib_id`
are. Leave `project_dir` out when naming the symbol by `library_path`.
