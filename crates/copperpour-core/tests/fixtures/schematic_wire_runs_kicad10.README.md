# Schematic wire runs fixture

`schematic_wire_runs_kicad10.kicad_sch` is an A4 sheet with two `Device:R`, seven wires
and one local label. It backs the proposed read-only check that reports long wire runs and
runs that take an indirect path.

## Provenance

Drawn by tool calls, not by hand, using `Device:R`, then resaved with
`kicad-cli sch upgrade --force` (KiCad 10.0.8), so it carries `(generator "eeschema")` and
`(version 20260306)`. The resave kept every item, UUID, coordinate and property. It added
KiCad's default fields and pin UUIDs, and wrote the symbol instances under the project
name `""`, as a save without a project does.

## The sheet

The route between `R1.2` and `R2.1` goes from `(29.21, 50.8)` to `(97.79, 50.8)` mm, with
a 25.4 mm excursion down and back. Its direct orthogonal length is 68.58 mm; its five
drawn segments total 119.38 mm, with 4 bends. No label names that run.

The 228.6 mm `LONG` segment goes from an open endpoint at `(25.4, 101.6)` to a label at
`(254, 101.6)`.

The separate 12.7 mm straight wire is a control: it is below both default thresholds the
proposal uses.

## Expected results

With the proposal's default thresholds, two findings: the meander (length 119.38, direct
68.58, extra 50.8, reasons `long_run` and `orthogonal_detour`, both ends attached) and
`LONG` (length 228.6, extra 0, reason `long_run`, start open and end attached). The 12.7
mm control reports nothing.

KiCad 10.0.6 and 10.0.8 plot all seven wires (360.68 mm in total). Their netlist export
puts `R1.2` and `R2.1` on `Net-(R1-Pad2)` and leaves `R1.1` and `R2.2` on
`unconnected-(…)` nets. `kicad-cli sch erc --severity-all` reports 2 `pin_not_connected`,
1 `wire_dangling` and 1 `label_dangling` error and 3 `unconnected_wire_endpoint` warnings.
The `pin_not_connected` errors are R1.1 and R2.2, the `wire_dangling` is the free control
wire, and the `label_dangling` is the `LONG` label at the end of that run. ERC counts
leave out `lib_symbol_issues` and `footprint_link_issues`, which depend on the machine's
KiCad library tables. The two versions agree on the affected items, but the 10.0.6 ERC
JSON reports positions at 1/100 of the 10.0.8 values while labeling both as mm.

## Review controls

The two companion sheets were generated with Copperpour's schematic writer, then
resaved by `kicad-cli sch upgrade --force` in KiCad 10.0.8. The mixed-power sheet was
resaved again beside a matching `schematic_wire_runs_mixed-power.kicad_pro`, which
names its symbol instances' project `schematic_wire_runs_mixed-power` instead of `""`.
The backtrack sheet has no symbol instances. Resaving either sheet beside a matching
project in KiCad 10.0.6 or 10.0.8 leaves it byte-identical.

- `schematic_wire_runs_backtrack.kicad_sch` has two collinear wires sharing an end.
  The saved path draws 15.24 mm against 5.08 mm direct, with no bends. Both versions
  report `unconnected_wire_endpoint` on the same wire and `label_dangling` for `START`,
  not an unconnected wire endpoint at the label. For the open end, the raw ERC JSON
  positions are `(0.5588, 0.508)` in 10.0.6 and `(55.88, 50.8)` in 10.0.8. The label
  attaches to the wire even though no symbol pin is on that net.
- `schematic_wire_runs_mixed-power.kicad_sch` uses a custom global power symbol with
  a `power_in` pin and a separate passive pin. Both netlists put `R1.1` on `RAIL`
  and `R2.1` on `/SIGNAL`, not on `RAIL`. Copperpour reports the local label as
  `SIGNAL` without the root-sheet prefix.
