### `delete_custom_rule` (minor release)

A new `verification` tool deletes one named rule from the board's sibling
`.kicad_dru` custom rules file: `delete_custom_rule(board, name)`, the
counterpart of `set_custom_rule`, which adds or replaces a rule but deletes
none ([#317](https://github.com/Copperpour/Copperpour/issues/317)). The rest
of the file is left as written, comment lines included. A rule that ends its
line goes with the blank space before it, so a rule
`set_custom_rule` appended goes as it came; one that shares its line with the
next goes with the spaces after it. A cut that would change how KiCad reads
the text beside the rule, such as text that would then open its line with `#`,
which KiCad reads as a comment, returns `plan_blocked` and writes nothing.

The file left is checked as `set_custom_rule` checks the file it writes: as
KiCad 10's rule parser and expression compiler read it, then, on a board with
an item, loaded by `kicad-cli pcb drc` on a copy of the board with a probe
rule. A fault in a rule left, or kicad-cli not loading the file, returns
`plan_blocked` and writes nothing; a rule whose fault lies inside it can be
deleted. On a board with no item yet the first check alone decides. A name no
rule has returns `invalid_argument`, naming the rules read and any fault that
may hide one; two rules of the name return `ambiguous_target`, their lines as
`candidates`; a missing file returns `file_not_found`; a file saved after the
read returns `conflict`.

The response gives `rules_file`, `name`, `previous`, the deleted rule's text,
`kicad_checked` and `unchecked_properties`, as `set_custom_rule` gives them.

No existing tool's arguments or responses change.
