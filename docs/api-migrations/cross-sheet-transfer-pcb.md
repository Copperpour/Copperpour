### Atomic PCB updates for `move_schematic_objects_between_sheets` (minor release)

Moving schematic objects now also updates matching footprint paths, their saved sheet
names and filenames, and renamed nets on the project's same-named `.kicad_pcb`.
The board and sheets change in one transaction. Footprint UUIDs, placement, and routing
stay the same.

A PCB net rename must keep differential-pair membership and polarity. It must also
keep the project's explicit net assignments and match the same netclass patterns.
The tool refuses patterns it cannot compare reliably unless the wildcard matches
both net names.
Without a PCB, the existing restriction on netclass patterns other than `*` still
applies to schematic net renames.

Run a new dry run before applying. Review `affected_files`, `pcb_path_mapping`, and
`pcb_net_name_mapping`. If a board exists but `kicad-cli` is unavailable, the net mapping
is `null`. Make `kicad-cli` available and plan again; you cannot apply an unchecked plan.

The tool never rewrites the same-named `.kicad_dru`. It requires the file to stay
unchanged, or to stay absent if it does not exist. Use a regular file for custom rules;
the tool refuses symlinks and directories. Before renaming PCB nets, it checks
conditions and assertions for net-name dependencies. Before relinking a footprint
to another sheet, it checks them for sheet dependencies, even if no PCB net changes
names. That includes rules that use component classes when Board Setup can assign
them. If the tool cannot rule out the relevant dependency, it refuses the move and
names the rules file. With no PCB, custom rules do not block a schematic net rename.
