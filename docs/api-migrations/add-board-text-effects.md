### `add_board_text` adds `thickness`, `mirror`, `bold`, and `italic`, and aligns file/IPC default thickness (minor release)

`add_board_text` now accepts optional `thickness` (mm, positive, default `round6(size * 0.15)`), `mirror` (boolean, default `false`), `bold` (boolean, default `false`), and `italic` (boolean, default `false`), and returns all four resolved fields in both `source: "ipc"` and `source: "file"` responses.

When `thickness` is omitted, the file fallback now writes `round6(size * 0.15)` instead of hardcoded `0.15`, matching the IPC path at all font sizes. Non-positive or non-numeric `size` and `thickness` values and non-boolean style flags are refused with `invalid_argument` before touching IPC or disk. Pass an explicit `thickness: 0.15` if you relied on the old fixed 0.15 mm stroke at non-unit sizes on the file path.
