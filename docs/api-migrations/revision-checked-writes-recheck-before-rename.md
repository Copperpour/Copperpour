### Recheck revision-checked writes before replacement (patch release)

Tools that use the shared revision-checked file writer now compare the target's
bytes again after staging and just before replacement. An intervening save or
deletion detected by that check stops the write. The writer returns a conflict
for changed bytes, a missing file, or a target that becomes non-regular before
replacement. Tools keep their respective conflict response kinds.

A target that is not a regular file at the first check fails before staging
with a message naming the cause, not a conflict. Use the actual file path
rather than a symbolic link to the file; reloading will not fix the path.
Shared dispatch reports `invalid_write_target`. Configuration saves report
`invalid_configuration` and leave the link and its target untouched.
A case-only `rename_footprint` also checks its target before renaming.

A directory sync failure after replacement leaves the outcome uncertain.
Inspect the saved file before retrying. A no-write conflict and an uncertain
outcome after replacement require different recovery.

This does not change the guarantees of other write modes. A writer that ignores
Copperpour's lock can still save between the last check and the rename.
