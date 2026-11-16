# Cross-sheet transfer

`move_schematic_objects_between_sheets` in `sch_hierarchy` moves existing objects within one saved project. It does not copy symbols or generate new symbol or pin UUIDs. Reference designators, multi-unit membership, properties, footprint assignments and BOM/DNP settings travel with the original blocks. Coordinates and field positions receive the same offset in millimetres. Unselected object blocks stay unchanged.

## Consolidating a sparse sheet

Close the schematic and project PCB editors cleanly. Use the project's entry schematic beside its same-named `.kicad_pro`. Inspect the hierarchy, symbol instance records and object UUIDs first. A sheet instance path starts with its top-level instance UUID, followed by the chain of child sheet-symbol UUIDs. Use the project-declared top-level UUID when present, not the sheet file's own UUID. Do not use page numbers or child-file root UUIDs.

For a two-part startup limiter going beside its controller, select both symbols and their wires, junctions, labels, no-connects and annotations. Include every placed unit of each selected reference. Request a plan first:

```json
{
  "schematic": "/projects/controller/controller.kicad_sch",
  "source_sheet_instance": "/ROOT_UUID/STARTUP_SHEET_UUID",
  "destination_sheet_instance": "/ROOT_UUID/PD_SHEET_UUID",
  "object_uuids": ["PART_1_UUID", "PART_2_UUID", "WIRE_UUID", "LABEL_UUID"],
  "offset_x_mm": 50.8,
  "offset_y_mm": -25.4,
  "dry_run": true
}
```

Replace the illustrative UUIDs with the actual saved UUIDs. Resolve any reported boundary wiring explicitly, inspect the new plan, then repeat with `"dry_run": false` and `"expected_plan_revision"` set to the dry run's `plan_revision`. Apply rebuilds and verifies the plan against fresh files and refuses as `stale_target` unless it would write exactly what that dry run reported.

The response lists selected objects, library definitions added, affected files, the placement offset and `path_mapping`. When KiCad checks run, `connectivity_impacts` lists any old and new net names observed in its exports. A dry run does not change project files or recover pending transactions.

## Hierarchy and PCB association

Symbol instance paths change from the source sheet instance to the destination, which may belong to a different top-level schematic in the same project. The symbol UUID at the end of the full component path stays the same. `path_mapping` contains the full old and new component paths.

This tool does not write the PCB or project settings. It reads the same-named PCB, when present, and refuses a move that would leave an existing footprint path stale. A net-name change must preserve KiCad's effective netclasses and must not affect saved PCB nets or explicit project net assignments. If a same-project `.kicad_dru` exists, a net rename refuses. Schematic verification does not evaluate custom PCB rules, including rules that depend on a footprint's sheet membership. Run PCB DRC after any later update from the schematic. Alternate boards and other projects are not searched; reconcile them explicitly before moving circuit objects in a multi-board workflow.

## Connectivity checks and limits

Supported objects are symbols, wires, junctions, local and global labels, no-connect flags, text and text boxes. The operation refuses:

- Source or destination files placed more than once, nonexistent instances, stale symbol paths, foreign project instance records, UUID or reference conflicts, and selections that split a multi-unit component.
- Physical source boundary connections or destination electrical overlaps.
- A selected local label whose name occurs outside the selection, including destination local or hierarchical label collisions.
- Moving hierarchical labels, sheet ports or sheet objects. Rewire the boundary separately.
- Grouped selections. Ungroup objects explicitly before transferring them.
- Buses or netclass directives on either affected sheet, a legacy top-level symbol-instance table on any project sheet; derived library definitions; conflicting destination definitions; incomplete or cyclic hierarchies; and sheets outside the project directory.
- Pending project transactions, a KiCad project lock that exists or cannot be inspected, or a PCB editor that is open or cannot be checked. Copperpour checks the project lock again after verification and before writing.

Copperpour reuses matching library definitions and copies missing definitions unchanged. It keeps the source definitions. Global labels and power nets retain their scope. A local label gains its destination sheet scope. The tool does not rename labels or convert local labels to global ones.

Text and fields keep their saved text. Sheet-dependent variables such as `${SHEETNAME}` may display a different value after the move. Netlist export and ERC do not verify how annotations render; inspect them in the destination sheet.

Before writing, Copperpour stages the complete saved hierarchy and project settings in private temporary directories. It requires `kicad-cli` to export both netlists and run both ERC checks. Component UUIDs, references, reported component classes and assembly settings such as inherited DNP and BOM exclusions, and pin-to-net groupings must match. No new ERC rule, severity and item combination may appear.

Multi-root verification requires KiCad 10.0.7 or newer. Earlier versions of `kicad-cli` load only the supplied top-level schematic. Copperpour checks that both exports and both ERC reports cover the project's sheet instances; a partial report cannot authorize a write.

Project-relative `.kicad_sym` libraries can be staged for verification. Project-relative footprint libraries cannot yet be staged. KiCad's ERC may check footprint links, so Copperpour refuses rather than checking a snapshot with missing footprint libraries.

Copperpour compares saved ERC item UUIDs directly when they identify a single sheet instance. When KiCad generates a transient pin UUID for a symbol without saved pin records, Copperpour matches it by saved symbol reference, pin number and observed endpoint. A finding on a repeated sheet that reuses the same item UUID is ambiguous, so Copperpour refuses rather than guessing.

Generated net codes and hierarchy-qualified local net names may change. Transfer does not rewrite netclass pattern rules or guess different classes.

If `kicad-cli` is missing, a dry run still returns a plan with `verification.status: "not_run"` and `connectivity_impacts: null`. Apply refuses without `kicad-cli`. After making the CLI available, run a fresh dry run to see and bind the verification result before applying. A check that ran but failed or returned unreadable output is a refusal, not a pass.

## Failure and recovery

Apply locks and revision-checks all inspected project files. It includes unchanged files and the expected absence of optional inputs in the journal but never rewrites or creates those guarded files. If a PCB appears after an interrupted move, recovery refuses rather than risking stale footprint paths. Journals with absence guards need a Copperpour version that supports version 2 journals.

The source and destination share one write-ahead journal. A failure before that journal is written changed nothing and is reported as a refusal: `invalid_argument` for a malformed request, `stale_target` when the project changed since the dry run, `unsafe_file_fallback` for a KiCad project lock, `conflict` when a file changed under the write, or `plan_blocked` for anything the transfer cannot do safely. An unfinished older journal in the project directory is also a `conflict`; it names that journal, which you inspect and recover or abandon first.

If a write fails after the journal is saved, `mutation_outcome_uncertain` names the file where it stopped and the journal. A file saved by another program after the move was planned is left as that program saved it. Run `copperpour transaction status` on the project directory first. `copperpour transaction recover` completes the verified move while every file still holds its before or after image. A divergent guard or target leaves the journal in place. If the transaction completed but its readback failed, the result gives the transaction ID instead. Inspect the saved project before retrying.

Source sheet deletion and hierarchy cleanup are always separate operations.
