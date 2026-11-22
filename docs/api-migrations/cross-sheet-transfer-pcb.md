### Atomic PCB updates for `move_schematic_objects_between_sheets` (minor release)

Moving schematic objects now also updates matching footprint paths, their saved sheet
names and filenames, and renamed nets on the project's same-named `.kicad_pcb`.
The board and sheets change in one transaction. Footprint UUIDs, placement, and routing
stay the same.

A PCB net rename must keep differential-pair membership and polarity. It must also
keep the project's explicit net assignments and match the same netclass patterns.
Without a PCB, the existing restriction on netclass patterns other than `*` still
applies to schematic net renames.

Run a new dry run before applying. Review `affected_files`, `pcb_path_mapping`, and
`pcb_net_name_mapping`. If a board exists but `kicad-cli` is unavailable, the net mapping
is `null`. Make `kicad-cli` available and plan again; you cannot apply an unchecked plan.

The tool never rewrites the same-named `.kicad_dru`. It requires the file to stay
unchanged, or to stay absent if it does not exist. Use a regular file for custom rules;
the tool refuses symlinks and directories. A move that would rename a PCB net
is refused if a rule condition or assertion uses `NetName`, `inDiffPair`, or another
expression the tool cannot prove is independent of net names. The refusal names the
file and the old and new net names. Moves without PCB net renames skip this check.
