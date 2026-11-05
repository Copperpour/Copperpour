### `list_schematic_texts`, `edit_schematic_text`, `delete_schematic_text` (minor release)

Three new `sch_batch` tools read and change a sheet's free text items, the
plain `(text …)` items `add_schematic_text` writes
([#56](https://github.com/Copperpour/Copperpour/issues/56)); `sch_batch`'s
description now names them. `list_schematic_texts(schematic)` returns `count`,
`texts` (`uuid`, `text`, `x`, `y`, `rotation`), the sheet's `revision` and
`source: "file"`. Labels, fields, text boxes and text inside library symbols
are not listed. The revision is the `DocumentRevision` of the saved file,
written `<hash>/<bytes>`.

`edit_schematic_text(schematic, uuid, revision, text)` replaces the item's
quoted string and nothing else, so its uuid, position, rotation and effects
stay as written. `text` is written as `add_schematic_text` writes it: line
breaks as `\n`, carriage returns dropped; a text empty once they are dropped,
or the text the item already holds, is `invalid_argument`.
`delete_schematic_text(schematic, uuid, revision)` removes the item and its
line, and returns `plan_blocked` when a group lists it as a member. Both edit
the sheet's file, so on a sheet placed more than once the change reaches every
instance.

Both prepare their text with `SchematicCommand` and `prepare_command`, only on
a sheet that still has the listed revision. They return `stale_target` when the
sheet no longer has it, when no item has the uuid, or when the prepared text
changes a top-level item other than that one, or more of it than its string;
`invalid_argument` for the uuid of another kind of item; `ambiguous_target`
when several top-level items carry it; `conflict` while KiCad holds the sheet or
its project's root schematic open, when the sheet changed after it was read, or
when its project cannot be told; and `unsupported_capability` for a sheet
outside its project's directory. Each reads the sheet back and returns the item
and the sheet's new `revision`; a file that is not the text written is
`mutation_outcome_uncertain`. No existing tool's arguments or responses change.
