### `rename_schematic_net_label` (minor release)

A new `sch_hierarchy` tool renames placed net labels in place:
`rename_schematic_net_label(schematic, new_name, uuid? | net + x + y | net +
scope, allow_merge?, allow_split?)`. `uuid` chooses one label, as
`list_schematic_labels` reports it; `net` with `x` and `y` chooses the one label
of the name at that position; `net` with `scope` chooses every label of the
name, `sheet` in this file, `hierarchy` in this file and every sheet file below
it, as `set_schematic_title_block` scopes. Only each label's name string is
rewritten, so its uuid, position, rotation, effects and fields stay as written;
the files are written in one transaction, refused while KiCad holds a target or
the project's root schematic open
([#146](https://github.com/Copperpour/Copperpour/issues/146)).

`new_name` is written as eeschema's label dialog writes it: `/` as `{slash}`,
line breaks dropped, an empty name refused. `net` is matched in that form, so a
label written with a raw `/` is found too; two labels of the name at one
position return `ambiguous_target` with their uuids. Bus names, names holding a
text variable, names nesting braces more than 16 deep, and a name that a bus on
a renamed label's sheet carries as a member, bus aliases included, are refused.

KiCad joins nets by name: on one sheet, wires that share a name; across
sheets, nets whose chosen driver is global (a global label, a power symbol not
`(power local)`, or a hidden `power_in` pin of another symbol), by the global
names they carry and by a global name such a net did not choose;
and a sheet pin with the hierarchical labels of its name. The check follows
those rules over the whole hierarchy. A rename that would join two nets
returns `plan_blocked` naming them, unless `allow_merge`; one that would break a
net apart, unless `allow_split`. Under `hierarchy`, sheet pins of the name in
the files reached are renamed with their labels. A hierarchical label whose
sheet pin is not in the files reached is renamed alone, which breaks that
connection: a split.

The response gives `old_name`, `new_name`, `count`, `labels` (`uuid`, `type`
as the file's tag, `file`, `x`, `y`) and `sheet_pins`, each read back from the
written file; `merged` and `split` as allowed; `two_names` for each renamed
label's wire left with another name (KiCad's `multiple_net_names`);
`sheet_pins_left` for the sheet pins that kept the old name; `hierarchy_root`,
the root the check read from; and `source: "file"`. Until now a rename was
`delete_schematic_net_label` and `add_schematic_net_label`, which loses the
uuid, the effects and a global label's fields, and needs the position,
rotation and shape read back first. No existing tool's arguments or responses
change. `sch_hierarchy` rises from 15 to 16 tools; `sch_wiring`, which holds
the other label tools, is at its 20-tool cap.
