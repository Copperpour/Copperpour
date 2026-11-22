# Direct lanes through a symbol field

`direct_lanes.kicad_sch` is an A4 one-sheet project. Two facing connectors, J1 and J2,
carry three aligned signal lanes. The top lane is AC coupled through C1; the other two are
one straight wire each. C1's stock Value `100nF` crosses both lanes below it, and its
Reference crosses the middle lane: three wire/field crossings. Every pin is wired. The
sheet is a passive interposer with no power symbols, and each lane is already the shortest
route between aligned pins, so there is no bend to remove or wire to shorten.

It backs two proposals: reporting a wire drawn through Reference or Value text, and
previewing clear Reference and Value placement. Moving C1's text into the open space above
the top lane should clear all three crossings without changing the wires, the symbols or
any pin's net.

## Provenance

Drawn by tool calls, not by hand, then resaved with `kicad-cli sch upgrade --force` (KiCad
10.0.8) with the project file beside it. The `.kicad_pro` is the project file
`create_project` wrote; the symbol instances name that project.

## Expected results (KiCad 10.0.8)

`kicad-cli sch export netlist`: `Net-(J1-Pin_1)` C1.2 J1.1; `Net-(J2-Pin_1)` C1.1 J2.1;
`Net-(J1-Pin_2)` J1.2 J2.2; `Net-(J1-Pin_3)` J1.3 J2.3. Four wire segments, 68.58 mm in
total. `kicad-cli sch erc --severity-all` reports nothing. ERC counts leave out
`lib_symbol_issues` and `footprint_link_issues`, which depend on the machine's KiCad
library tables.

`check_schematic_overlaps` on `main` returns zero findings on this sheet, because it does
not compare wires with field text.
