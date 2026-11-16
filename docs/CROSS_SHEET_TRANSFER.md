# Cross-sheet transfer

`move_schematic_objects_between_sheets`, in `sch_hierarchy`, moves UUID-selected symbols,
wires, junctions, labels, no-connect flags, text and text boxes from one sheet to another
in the same saved project. It moves the original objects instead of copying them, so
symbol and pin UUIDs, reference designators, unit membership, properties, footprint
assignments and BOM/DNP settings stay the same. Every moved coordinate, including field
positions, shifts by the offset you give in millimetres. Objects you don't select stay
where they are.

## Moving objects to another sheet

1. Close the schematic editor and the project's PCB editor cleanly.
2. Set `schematic` to the project's entry schematic, the one beside its same-named
   `.kicad_pro`.
3. Look up the sheet instance paths and object UUIDs. A sheet instance path is the
   top-level instance UUID followed by the UUID of each child sheet symbol on the way
   down. If the project file declares a top-level UUID, use it instead of the sheet file's
   own UUID. A project with a single top-level sheet whose declared UUID differs from its
   file's UUID is refused, because KiCad's command-line loader uses the file's UUID. Page
   numbers and child-file root UUIDs don't work.
4. Select everything that belongs together. To move a two-part startup limiter beside its
   controller, select both symbols with their wires, junctions, labels, no-connects and
   text, and every placed unit of each reference.

Request a dry run first, using the real saved UUIDs:

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

If the dry run reports wiring that crosses the selection, rewire it and run the dry run
again. Then apply with `"dry_run": false` and `"expected_plan_revision"` set to that dry
run's `plan_revision`. Apply rebuilds the plan from the saved files and checks it again,
so it can refuse for any reason a dry run can. If the plan revision no longer matches the
one you passed, for example because the project or the verification result changed, apply
refuses with `stale_target`, and you start again with a new dry run.

The response lists the moved objects, the library definitions it adds, the affected files,
the offset and `path_mapping`, which pairs each old full component path with its new one.
When the KiCad checks run, `connectivity_impacts` lists net names that change. A dry run
never changes project files and never recovers a pending transaction.

## What the KiCad checks compare

Before writing, Copperpour copies the saved hierarchy and project settings to private
temporary directories and runs `kicad-cli` netlist export and ERC on the project as it is
and as it would be after the move. The move goes ahead only if, in what KiCad reports:

- component UUIDs, references, component classes and assembly settings, such as inherited
  DNP and BOM exclusions, are the same;
- every pin connects to the same set of pins as before;
- no ERC finding appears with a new combination of rule, severity and items, and no
  existing finding appears more often.

Net codes and hierarchy-qualified local net names may change. The tool doesn't rewrite
netclass pattern rules or pick different classes.

A project with several top-level schematics needs KiCad 10.0.7 or newer, because earlier
versions of `kicad-cli` load only the schematic they are given. Copperpour checks that
both exports and both ERC reports cover every sheet instance, and refuses a report that
misses one.

ERC findings are matched by saved item UUID. KiCad makes up pin UUIDs for a symbol without
saved pin records, so Copperpour matches those findings by reference, pin number and
position. A finding on a repeated sheet that reuses an item UUID can't be matched to one
placement, so the move is refused.

Without `kicad-cli`, a dry run still returns the plan, with
`verification.status: "not_run"` and `connectivity_impacts: null`, and apply refuses. Once
`kicad-cli` is available, run a new dry run and apply with its revision. A check that
fails or returns output Copperpour can't read refuses the move.

## What the tool refuses

- Source or destination sheet files placed more than once, sheet instances that don't
  exist, stale symbol paths, symbol instance records from another project, UUID or
  reference conflicts, and selections that include only some units of a part.
- A wire, pin or junction that connects a selected object to an unselected one on the
  source sheet, or a moved object that would touch an existing connection on the
  destination sheet.
- A label or power symbol name that occurs both inside and outside the selection when
  either occurrence is sheet-local, on the source or the destination sheet.
- Hierarchical labels, sheet pins and sheets. Rewire the sheet boundary as a separate
  step.
- Objects in a group. Ungroup them first.
- Buses or netclass directives on either sheet, a legacy top-level symbol-instance table
  on any sheet, derived or conflicting library definitions, incomplete or cyclic
  hierarchies, and sheets outside the project directory.
- A move between sheets that inherit different simulation exclusion from their parent
  sheets.
- A pending project transaction, a KiCad lock on the project or its PCB that exists or
  can't be inspected, or a PCB editor that is open or can't be checked. Copperpour checks
  the project lock again just before writing.
- Project footprint libraries listed with project-relative paths. KiCad's ERC can check
  footprint links, and Copperpour can't yet copy footprint libraries for that check.
- Absolute library paths into the project, described under
  [Libraries and labels](#libraries-and-labels).

## PCB and project settings

The tool doesn't write the PCB or project settings. If the project has a same-named PCB,
the tool refuses a move that would leave a footprint pointing at a stale schematic path,
or whose destination path or new net name the PCB already uses.

A net-name change must keep KiCad's effective netclasses, must not rename a saved PCB net
and must not affect explicit netclass assignments in the project file. A net rename is
also refused when the project file has a netclass pattern other than `*`, because
Copperpour can't prove the patterns still match the same nets. It is refused when the
project has a `.kicad_dru` custom rules file too, because the schematic checks don't
evaluate custom PCB rules, including rules that depend on which sheet a footprint belongs
to. Run PCB DRC after you next update the PCB from the schematic.

The tool doesn't look at other boards or projects. In a multi-board workflow, reconcile
them yourself before moving objects.

## Libraries and labels

Regular `.kicad_sym` files listed in `sym-lib-table` with project-relative paths, either
`${KIPRJMOD}/...` or a plain relative path, are included in the KiCad checks, and
Copperpour makes sure they haven't changed when it writes. The tool refuses an absolute
library path that:

- resolves inside the project, including through a symlink or a platform alias;
- passes through a symlink inside the project, even one that points elsewhere;
- is a link that can't be resolved.

A project-relative path through such a link is refused too. Point the table straight at
the external library, or keep a regular library file in the project. KiCad reads libraries
outside the project during the checks, but Copperpour doesn't guard them against changes.

The destination reuses a matching library definition and gets a copy of any it lacks. The
source keeps its definitions.

Global labels and global power nets keep their scope. Local labels and local power symbols
take the destination sheet's scope. The tool doesn't rename labels or turn local labels
into global ones.

Text and fields keep their saved text. Sheet variables such as `${SHEETNAME}` may show a
different value on the new sheet. The KiCad checks don't look at how text renders, so
check it on the destination sheet.

## If something goes wrong

The tool writes both sheet files through one journal. Before writing, it checks every
project file it read, and that files it expects to be missing, such as a PCB, rules file,
library table or project lock, are still missing. It never rewrites or creates those
files.

A refusal before the journal is saved changes nothing:

- `invalid_argument`: the request is malformed.
- `stale_target`: the plan revision no longer matches the dry run, for example because the
  project or the verification result changed. Run a new dry run.
- `unsafe_file_fallback`: a KiCad lock on the project or its PCB is present or can't be
  inspected.
- `conflict`: a file changed after Copperpour read it, or Copperpour inspected an older
  unfinished journal in the project directory. For a journal, the response names it.
  Inspect it, then recover or abandon it first.
- `plan_blocked`: the plan failed validation, or the move can't be done safely for another
  reason, such as a pending journal that can't be read. The reason says why.

If something fails after the journal is saved, the result is `mutation_outcome_uncertain`.
Usually it names the journal and, when available, the file where the write stopped. If the
move finished but reading the files back didn't match, it gives the transaction ID
instead; inspect the saved project before retrying. A file another program saved after the
move was planned is left as that program saved it.

To finish an interrupted move:

1. Run `copperpour transaction status` on the project directory. If no journal remains,
   there is nothing to recover; inspect the saved project before retrying.
2. If the journal remains, run `copperpour transaction recover`. Recovery checks both
   sheets: each must match either its contents before the move or its contents after it,
   and recovery writes only the sheets that still have their before contents. Every other
   file it read must be unchanged, the files it expects to be missing must still be
   missing, and no KiCad editor lock may be present on the files involved. Otherwise it
   stops and keeps the journal.

Journals from this tool record files that must stay missing, so recovering one needs a
Copperpour version that reads version 2 journals. Recover any pending journal before
downgrading.

Deleting the emptied source sheet and other hierarchy cleanup are separate steps.
