### Finish pending version 2 transaction journals before downgrading (minor release)

Copperpour still writes version 1 journals for transactions that do not need to guard an absent path. A transaction that needs to make sure a path stays absent writes a version 2 journal. Earlier Copperpour releases cannot inspect or recover version 2 journals.

Before downgrading, use a release that supports version 2. Run `copperpour transaction status <project-dir>` to check for pending journals. If any remain, run `copperpour transaction recover <project-dir>`, then run `status` again and confirm it reports `No active transaction journals.`

If an editor lock blocks recovery, close KiCad and retry. If `status` lists a file as `divergent`, inspect the file and restore it to the expected state only if you can do so without losing edits. Do not force-abandon a journal just to downgrade: abandoning does not undo writes that already happened. If you already downgraded and the older release rejects the journal, return to a release that supports version 2 to finish recovery.
