### `set_board_page` (minor release)

A new `pcb_board` tool sets the paper size and orientation of a board KiCad
does not hold open, the board counterpart of `set_schematic_page`
([#257](https://github.com/Copperpour/Copperpour/issues/257)):
`set_board_page(board, size, portrait?)`. `size` is one of the sizes
`set_schematic_page` takes (A0–A5, A–E, USLetter, USLegal, USLedger); a `User`
size is not offered. `portrait` defaults to `false`, landscape. Nothing on the
board moves.

The saved file's `(paper …)` token is replaced whole, whatever its form, or,
on a board that has none and so is A4 to KiCad, inserted directly after
`(general …)`, written as KiCad 10 writes it: `(paper "A3")`,
`(paper "A3" portrait)`. A KiCad 5 board's `(page …)` node is replaced the same
way. The write takes the board's lock and is read back. A board whose file
already holds that token is not written, and returns `changed: false`.

KiCad 10 has no command that sets a live board's page settings, so a board
KiCad holds open is refused with `unsupported_capability`, its `capability`
`set_board_page`: change the page in the PCB Editor (File > Page Settings), or
close the board and retry. The live-board gate the other board writers use
decides this, so its own refusals stand too: KiCad running with no PCB editor,
and a board this session saw KiCad hold, which stays refused once KiCad no
longer answers for it.

The response gives `paper` and `paper_size_mm` as `get_board_info` reports
them, read back from the written file: `paper` names the size, and
`paper_size_mm` is `null`, as it is for every named size. `portrait` gives the
orientation read back, which `get_board_info` does not report. With them come
`changed`, `file`, `source: "file"`, `saved: true`, and the `fallback_reason`
and `warning` of `set_board_title_block`'s file writes. A file that does not
read back as written returns `mutation_outcome_uncertain`.

No existing tool's arguments or responses change.
