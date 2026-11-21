### File transactions recheck replacement targets before writing (patch release)

Apply and recovery stop if a replacement target no longer matches its recorded
before image at the final check. The changed file is left untouched. Earlier
writes may already have applied, so inspect the retained journal with
`copperpour transaction status <project-dir>` before retrying. No journal format
or command changes are required.

Rust callers of `try_commit_file_transaction` can also receive the older
journal in `TransactionFailure::journal` if implicit recovery hits a conflict
after writing a target. This path does not mean the new transaction started
or that the journal still exists after a cleanup failure.
A KiCad lock that blocks publication after journaling is reported as a
transaction conflict with the journal path. A lock found during preflight
keeps its existing error.

If journal recovery publishes a target and then stops, it reports
`TransactionConflictKind::AfterImage`, and tools report
`mutation_outcome_uncertain`. This includes writes from an earlier journal
in the same recovery call. A divergent journal rejected before a target write
still uses `Recovery` and reports `conflict`. Dry runs do not replay journals.
