### `get_board_info` exposes live query failures and source evidence (minor release)

`get_board_info` now uses the shared board-reader source policy. Once the exact
board is identified in KiCad, failures reading its title block, enabled layers or
nets return an error instead of a successful answer from a possibly older saved
file. Retry the live read, or explicitly request `board_source: "saved"` to inspect
the saved snapshot. The optional selector accepts `auto` (default), `live`, and
`saved`, matching other board readers.

Existing metadata fields and `source: "ipc" | "file"` remain. New `sources` keys
identify `metadata` and `paper` independently, and `source_evidence` explains the
board observation and any saved-file fallback. Paper settings still come only
from the saved file; on a live answer with an unreadable file their provenance is
`unavailable` and their values remain null. `board_source: "live"` requires live
metadata, not live paper settings.

This supersedes the `get_board_info` fallback-policy exception documented in
`require-net-response.md` for #464/#469.
