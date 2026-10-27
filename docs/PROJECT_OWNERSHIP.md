# Schematic project ownership

Copperpour resolves saved schematic ownership by parsing candidate projects' sheet
trees. Directory ancestry alone does not establish ownership. The shared
resolver serves symbol placement, library resolution, and ERC root detection.
Relative target paths are anchored at the process working directory before
walking ancestors, so the same saved hierarchy remains discoverable.

## Behavior

| Observed saved state | Result |
| --- | --- |
| Library table beside the target | Library lookup uses that directory before consulting ancestor projects. |
| Exactly one project hierarchy contains the target, and candidate traversal is complete | Use the proven project's directory and hierarchy paths. |
| No ancestor project candidate | Allow the loose schematic and keep its own directory as the library context. |
| Candidates exist but no hierarchy contains the target | Return the existing structured `conflict`. |
| A candidate root is missing, unreadable, malformed, or lacks its root UUID | Retain that candidate in the conflict evidence. |
| Multiple hierarchies contain the target | Return `conflict`; never choose by directory enumeration order. |
| A competing root or intermediate sheet cannot be inspected, or traversal reaches its depth limit | Return `conflict` even if another path to the target was found; incomplete observation does not prove uniqueness. |
| An ancestor directory cannot be enumerated | Return a `file_not_found` refusal naming that directory; do not report the schematic as projectless. |

Conflict `error.paths` contains the schematic directory and every candidate root
schematic, including roots that could not be read. Candidates are listed in
stable project-path order. A successful owner is derived from saved sheet references;
it is not inferred from the requested filename or a sibling project file.

Symbol-loading handlers resolve their library context before mutation. Batch
placement does so once before processing any entries, so ownership conflicts
cannot leave partially placed components. A local library table establishes
library authority, not proof of hierarchy membership for placement metadata.

## Placement instance validation

Single component, batch component, and power-symbol placement share the same
saved hierarchy context. A document-wide placement into a reused child writes
every observed project/path entry, then reloads the committed target and derives
the successful response from that symbol.

| Observed saved state | Placement result |
| --- | --- |
| Unique child with matching saved symbol metadata | Write its exact hierarchy path. |
| Reused child with matching metadata | Write every hierarchy path; preserve existing symbols and other documents. |
| Loose schematic with no candidate project and matching local instance metadata | Use its own project name and root UUID. |
| Missing, foreign, duplicate, malformed, or obsolete symbol paths, references, or units | Return `stale_target` before writing; never silently repair or pick one entry. References must retain the symbol's designator identity, at least one hierarchy entry must match the symbol Reference, and every saved unit must match the placed unit. |
| Unproven or ambiguous project ownership | Preserve the ownership `conflict` refusal before writing. |
| Readback has the wrong document, missing symbol/fields, or inconsistent instance metadata | Return `stale_target` instead of success; the placement may already have been written. |

The saved root sheet tree supplies expected paths. Existing symbol instance
entries supply preflight evidence. Reloaded symbol fields supply the UUID,
library identity, reference, value, coordinates, rotation, unit, project and
paths in the response. Correct the root links and saved symbol metadata together
before retrying a stale preflight. After a readback failure, inspect the file
first to avoid adding the same symbol twice.

A stale refusal is bounded rather than per-symbol. `error.reason` names the
expected instance identity once, counts how many of the sheet's placed symbols
are stale, and groups them by diagnosis, naming a few symbols and a few distinct
diagnoses before counting the rest. Symbols that disagree with each other keep
separate entries, and the instance lists are always given in full — those are
what a repair has to write back. Read it as prose, not as a parsed list: to
enumerate every affected symbol, inspect the file.

## Hierarchy annotation

`annotate_schematic` with `schematic_scope: "hierarchy"`
([#42](https://github.com/Copperpour/Copperpour/issues/42)) walks the saved
sheet tree of the proven root and checks every file's instance records before
it plans anything. A record is the project's when its path starts at the root,
as eeschema saves it, and carries the project's name or none.

| Observed saved state | Annotation result |
| --- | --- |
| The target is the proven root, and the project lists no other top-level sheet | Annotate every sheet instance; commit every changed file in one transaction. |
| The target is a child sheet, or no project owns it | Return `invalid_argument` naming the root or asking for `schematic_scope: "file"`; never annotate the file alone. |
| Unproven or ambiguous project ownership | Preserve the ownership `conflict` refusal before writing. |
| The project lists another top-level sheet, or gives the root another uuid | Return `invalid_argument`: eeschema annotates every top-level sheet as one hierarchy, and keys the root by the uuid listed. |
| A placed symbol lacks a record for a sheet instance of its file, records one twice, records a project path that is not one, records one under another project's name, or records a path under another root under this project's name (eeschema prunes those on load); or two symbols of one file share a uuid | Return `stale_target` before writing; nothing is pruned or created. |
| Records under another root, under another project's name or none | Leave them untouched. |

## Repairing an ownership conflict

A schematic that once inherited an unrelated ancestor project's libraries, or
fell back to projectless operation despite an unproven candidate, now returns
`conflict`. Reloading alone does not repair it: inspect `error.paths` and the
message, then repair the saved root-to-child sheet references, restore
unreadable project schematics, or move a genuinely independent document out of
the unrelated project. An explicit library table beside a schematic still
selects its library context.

## Evidence and limits

The `project_ownership` test fixture is copied from KiCad's complex-hierarchy
demo. Tests exercise its two references to one child, relocation into nested
directories, unrelated candidates, and explicit corruptions of temporary copies.

Resolution observes saved files, not unsaved editor state. Traversal is
cycle-safe and bounded by `MAX_HIERARCHY_DEPTH`; it does not establish ownership
through an untraversable hierarchy. Placement tests use this same KiCad-authored
fixture for unique/reused children, real multi-unit symbols, all three placement
handlers, stale path/reference/unit and ambiguous no-write refusals, and
committed-file readback.
This does not implement live-editor navigation.

Ownership evidence is read during preflight; it is not an atomic snapshot of
every project file. Target-file writes retain their existing revision checks.
