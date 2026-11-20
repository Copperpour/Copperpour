### `export_manufacturing_package` lists JLCPCB hand-fitted parts on their own (minor release)

A footprint with KiCad's `exclude_from_pos_files` attribute that is not DNP is
fitted, but not by the placement machine: a through-hole connector soldered
after JLCPCB assembly, for example. The JLCPCB CPL already left it out, but the
BOM, read from the schematic, still listed it, so the package ended with
`JLCPCB BOM/CPL designators do not match: missing from CPL [...]` and was
reported incomplete.

For `fab_house` `jlcpcb` with assembly, such a part now leaves the BOM as it
leaves the CPL, and is written to `BOM-hand-fitted-<board>.csv` beside it, with
the BOM's columns, so it is still bought. A BOM row grouping both kinds is cut
in two, each with its own quantity. `files_generated` describes the new file
with the type `bom_hand_fitted` and its `designators`; it is not in `files`,
as it is not uploaded, and `generate_jlcpcb_order_link` uploads the BOM without
these parts.

A footprint that also has `exclude_from_bom`, or is not in the schematic (a
mounting hole), is not a hand-fitted part. The BOM is unchanged when the board
has no hand-fitted part, and for every other fab house. A
`BOM-hand-fitted-<board>.csv` left in the output directory by an earlier JLCPCB
export is removed when the board no longer has a hand-fitted part. If the list
cannot be written, the BOM is left as KiCad wrote it, a warning says so and the
package is reported incomplete.

Callers that marked hand-fitted parts DNP to keep them out of the assembly
files can set `exclude_from_pos_files` instead (`edit_component`), and clear
DNP.
