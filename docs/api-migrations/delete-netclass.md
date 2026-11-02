### `delete_netclass` (minor release)

A new `pcb_routing` tool deletes from the board's sibling `.kicad_pro`:
`delete_netclass(board, name | pattern, netclass?)`. With `name`, the class
goes and every `netclass_patterns` entry that named it names `Default` instead,
as KiCad's Board Setup deletes a class; `Default` itself is refused as
`invalid_argument`. With `pattern`, the entry with exactly that text goes;
`netclass` names the class whose entry goes where several classes hold the same
text; without it that is `ambiguous_target`, the classes as `candidates`. A
class delete moves no pattern into a second copy of one `Default` already
holds, as Board Setup saves it. It writes as `create_netclass` and
`assign_net_to_class` do: refused while KiCad holds the board or the project,
and only while the file holds what was read
([#109](https://github.com/Copperpour/Copperpour/issues/109)).

A class delete returns `patterns_moved_to_default` and
`label_assigned_nets`, the nets that the project's `netclass_assignments`
gives the class. Schematic netclass labels and that cache are not edited:
`run_erc` reports a label still naming a deleted class as
`undefined_netclass`.
