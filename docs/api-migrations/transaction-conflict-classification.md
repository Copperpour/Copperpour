### Transaction conflict classification across project and schematic tools (minor release)

`SexpError::TransactionConflict` now carries a typed `TransactionConflictKind` (`BeforeImage`, `Recovery`, or `AfterImage`) so callers and MCP dispatch distinguish why a durable multi-file transaction failed:

- `create_project` still returns `conflict` naming the colliding project file when a target already exists (`BeforeImage`), now returns `conflict` naming the stuck journal and `copperpour transaction status` / `recover` / `abandon` when an older journal in the directory cannot be recovered (`Recovery`), and returns `mutation_outcome_uncertain` when after-image durability verification fails (`AfterImage`).
- `rename_project`, `set_schematic_title_block`, `annotate_schematic` (`hierarchy`), `upgrade_library`, and other transaction-backed schematic tools (`add_hierarchical_sheet`, `duplicate_sheet`, `renumber_sheet_pages`, sheet mutations) now classify `BeforeImage` as `conflict`, `Recovery` as `conflict` naming the stuck journal and recovery command, and `AfterImage` as `mutation_outcome_uncertain` instead of `handler_error` or a stale-target message.

Callers branching on `error.kind` should handle `conflict` (safe refusal; inspect `message` / `paths` for a stuck journal) and `mutation_outcome_uncertain` (reload and inspect before retrying).
