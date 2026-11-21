### Atomic PCB updates for `move_schematic_objects_between_sheets` (minor release)

The tool now updates matching footprint paths, sheet metadata, and renamed PCB nets in
the project's same-named `.kicad_pcb` while moving the schematic objects. Footprint UUIDs,
placement and routing stay the same. Inspect `affected_files`, `pcb_path_mapping` and
`pcb_net_name_mapping` in a new dry run before applying. The PCB net mapping is `null`
when a board is present but native verification could not run; apply still requires
that verification.

The tool guards the same-named `.kicad_dru` or its absence and never rewrites custom
rules. A PCB net rename refuses if a rule condition depends on net names, for example
through `NetName` or `inDiffPair`, or if that independence cannot be determined. The
refusal names the file and the nets. A move without a PCB net rename does not trigger
this check.
