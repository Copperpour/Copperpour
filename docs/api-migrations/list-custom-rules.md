### `list_custom_rules` (minor release)

A new `verification` tool lists the rules in the board's sibling `.kicad_dru`
custom rules file: `list_custom_rules(board)`. `set_custom_rule` and
`delete_custom_rule` write the file, and `get_design_rules` reads only the
`.kicad_pro`, so until now no tool read it
([#685](https://github.com/Copperpour/Copperpour/issues/685)).

Each rule comes in file order with its `name`, its `line` and its `text`, from
its `(` to its `)` as written. The rules are those `set_custom_rule`'s check of
the file reads, the reading both writers use to find a rule by name, and
`faults` gives what the check finds; kicad-cli drops a rules file it cannot
parse. A fault the check cannot read past, such as a rule never closed, hides
the rules after it; a string left open, or a text variable, which KiCad expands
from the board and the check does not, hides them all. A rule with no name is
not listed; its fault gives its line. kicad-cli is not run, so the property
names the rules use come back unchecked in `unchecked_properties`. The
response also gives `rules_file` and `count`; a board with no rules file gives
`exists: false` and no rules. The tool changes no design file.

No existing tool's arguments or responses change.
