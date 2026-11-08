### `add_hierarchical_sheet` and `delete_sheet` relink every file below the sheet (patch release)

`add_hierarchical_sheet` recorded the new sheet's instance path on the symbols
of the file it placed, and `delete_sheet` dropped it from them, but neither
reached the files that file places through its own sheets. Placing a file that
holds sheets, re-adding one, or deleting one left every deeper symbol without
a record for the new path, or keyed to the old sheet, and the next write to
those files was refused as `stale_target`
([#126](https://github.com/Copperpour/Copperpour/issues/126)). Both tools now
relink every file reachable below the sheet in the same transaction, one
record per placement, so a file placed twice inside the placed file gains two.
`patched_symbol_instances` and `unlinked_symbol_instances` count those symbols
too. The add refuses, before writing, a file below the placed sheet that
cannot be read, a hierarchy below it deeper than `add_hierarchical_sheet`'s
own limit, and a file that places the parent itself; the delete leaves a file
it cannot read as it is. Not changed: the page records of the sheets inside
the placed file, and each symbol's Reference property, which eeschema writes
from one instance, so a delete can leave it naming the deleted placement's
designator and the file refused as `stale_target` until the property is
rewritten, as for the directly placed file before.
