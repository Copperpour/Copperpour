### `annotate_schematic` annotates a whole hierarchy from its root (minor release)

`annotate_schematic` takes an optional `schematic_scope`, as the design-review
tools do. The default, `"file"`, plans one file as before; only the package
rule below changes in it. `"hierarchy"`, given the project's root schematic,
plans every sheet instance of the saved hierarchy together, as eeschema's
Tools → Annotate does with its default "Entire schematic" scope
([#42](https://github.com/Copperpour/Copperpour/issues/42)):

- every `?` on any sheet instance gets the first free number for its prefix
  in the project;
- sheet instances are taken in eeschema's order: page number, after the repair
  eeschema makes to empty or repeated pages when it loads them, then X, then Y;
- a designator duplicated across sheets is reported in `unresolved`, and with
  `resolve_duplicates` the first in that order keeps it;
- a changed symbol's `Reference` property shows its file's first instance in
  that order, the one eeschema saves it from;
- every changed file is committed in one transaction journalled in the root's
  directory.

Like file scope, it follows Annotate's default options: a project's own
numbering method, sort order and `reuse_designators` setting are not read.

A record under the root belongs to the project by its path, and carries the
project's name or none; records under another root are left alone unless they
carry the project's name, which eeschema prunes as obsolete. Nothing is
written when the target is not a proven root (`invalid_argument`, or
mixelpixx/Konnect#189's
`conflict`), when the project lists another top-level sheet or gives the root
another uuid (`invalid_argument`), when any file of the hierarchy has a KiCad
lock (`conflict`), when an unfinished transaction in the root's directory
cannot be settled, or a dry run finds one (`conflict`), when a sheet file is
missing or the walk fails (`file_not_found`, `handler_error`), when a file to
be written lies outside the root's directory (`stale_target`), or when a
placed symbol's records do not match the hierarchy: missing, repeated,
obsolete, or under another project's name (`stale_target`). Nothing is pruned
or created; opening and saving the project in KiCad repairs such records.

The hierarchy response adds `sheets` (`path`, `schematic`, `page`,
`sheet_number`, and `page_in_file` when the repair changed it), `schematic` on
each `assigned` entry, `members` (`schematic`, `path`, `uuid`, `unit`) on each
`duplicates_before` and `unresolved` group, and `remaining_unresolved` after a
write. There, `paths` lists the instances holding the project's records sorted
as strings (`sheets` gives eeschema's order), `other_sheets_consulted` lists
the sheet files below the root, all of them annotated, `other_sheets_unreadable`
is empty because an unreadable sheet refuses the call, and `assigned[].project`
is the name the record carries, `""` where it carries none. Every non-error
response now carries `schematic_scope`, and a file-scope call on a schematic
that places sheets says that its sheets were not annotated. Two new
`unresolved` reasons cover unannotated units of one multi-unit part that
eeschema would put into a package with units elsewhere — on another sheet
instance, or in a package already numbered — which is not reproduced; they
stay unannotated.

In both scopes the units of one package are now recognised by their library
item, as eeschema recognises them, rather than by the name of their embedded
definition: KiCad saves a unit whose definition differs from the library's
with a copy named `Name_1`, and such units were reported as `unresolved`
although they are one part. Their declared unit counts must agree.
Unannotated, such units now take one number between them, where they took one
per embedded copy; when the copies' unit counts differ they stay unannotated
and are reported in `unresolved`, where before each copy was numbered.
