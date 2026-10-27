# Schematic project ownership

Konnect resolves saved schematic ownership by parsing candidate projects' sheet
trees. Directory ancestry alone does not establish ownership (#189). The shared
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
| Unproven or ambiguous project ownership | Preserve #189's `conflict` refusal before writing. |
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

## Unreleased notes (next minor release)

Schematics that previously inherited an unrelated ancestor project's libraries,
or silently fell back to projectless operation despite an unproven candidate,
now return `conflict`. Repair the saved root-to-child sheet references, restore
unreadable project schematics, or place a genuinely independent document outside
the unrelated project. An explicit library table beside a schematic continues
to select its library context. Ownership uses the existing `conflict` kind.

Placement adds `stale_target` and observed hierarchy/readback fields without
removing existing tools, inputs, or response fields. These response changes are
also part of the next minor release (#383/#389); see
[Konnect's API migrations](https://github.com/mixelpixx/Konnect/blob/v0.13.0/docs/API_MIGRATIONS.md#unreleased-complete-schematic-placement-instances-minor-release).

This behavior change belongs in the next minor release, as agreed in #189.
Existing `conflict` clients should inspect `error.paths` and the message before
retrying: reloading alone does not repair an ownership conflict.

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
