# Copperpour MCP API migrations

Copperpour's tool names, arguments and responses are public API. This file
records intentional changes to them, newest release first, with the supported
replacement workflow.

Changes not yet released are in [`api-migrations/`](api-migrations/), one file
per change. `cargo xtask collect-api-migrations <version>` moves them here when
a release is cut.

Changes from Konnect, up to its last release, v0.13.0, are in
[Konnect's copy of this file](https://github.com/mixelpixx/Konnect/blob/v0.13.0/docs/API_MIGRATIONS.md).

## v0.14.0

### `mirror_schematic_component` (minor release)

A new `sch_components` tool sets or clears the mirror of a placed component:
`mirror_schematic_component(schematic, reference, mirror, unit?)`. `mirror`
takes `"x"`, `"y"` or `"none"`, the vocabulary `add_schematic_component`
already uses, and is absolute rather than a toggle. Without `unit`, every
placed unit takes it. With `unit`, only that unit changes, because KiCad stores
the mirror per placed unit and one gate of a package is often reflected while
its siblings and power unit are not. Field text reflects with the body (#613).
Junctions and no-connect flags are handled as `rotate_schematic_component`
handles them.

Several rotation and mirror pairs draw the same body, and eeschema writes only
one of them: 180° with `x` becomes 0° with `y`, and 90° with `y` becomes 270°
with `x`. The tool stores that pair, so a reflection can change a unit's stored
rotation without turning it, and KiCad's next save changes nothing. A later
`rotate_schematic_component` works from the stored angle, so take it from the
response rather than from the angle the unit was given. The response
lists each selected unit as written, in `mirrored_units` (`unit`, `uuid`,
`rotation`, `mirror_x`, `mirror_y`, `changed`), with `changed_unit_count`. As in
every component tool, the top-level fields describe the lowest-numbered unit. A
call that changes nothing leaves the file untouched.

Until now a placed symbol could not be reflected at all: the only way was to
delete it and place it again with `mirror`. No existing tool's arguments or
responses change. `sch_components` rises from 18 to 19 tools (#450).

### `batch_add_power_symbol`, and no second symbol on a pin already on its rail (minor release)

New tool `batch_add_power_symbol` in `sch_batch` (#856). It takes
`power_net` and `pins: [{reference, pin_number}]`, plus optional `max_gap`
(default 10.16 mm) and `stub_length` (default 2.54 mm, a multiple of 1.27).
Pins that face the same way, end on one row and are within `max_gap` of a
neighbour are joined by stubs and a bar under one power symbol. Every other pin
gets a symbol on its endpoint. A group whose wires would touch another item is
not joined, and its entry carries `not_joined_reason`. The response lists
`groups` (`pins`, `joined`, `symbol`, `wire_uuids`), `already_connected`,
`placed_count`, `wires_added_count` and `junctions_added`.

`add_power_symbol` with `reference` + `pin_number` no longer adds a symbol to a
pin that is already on `power_net`, whether a symbol sits on the pin, on a pin
stacked with it, or is reached through a wire. It writes nothing and returns
`{already_connected: true, power_net, existing_reference, x, y}` instead of the
placement response. A caller that wanted the duplicate can still place it with
`x` + `y`, which is unchanged.

No tool, argument, or response field was renamed or removed.

### schematic component tools set and report KiCad's native DNP attribute (minor release)

KiCad marks a part do-not-populate with the symbol attribute `(dnp yes|no)`.
No schematic tool could set it, so a caller fell back to a custom property
named `DNP`. KiCad does not treat that property as DNP: its plot draws the part
as fitted and `kicad-cli sch export bom --exclude-dnp` keeps it, while the
property fills the BOM's DNP column (#415).

- **New argument:** an optional boolean `dnp` on `edit_schematic_component`,
  and on each `batch_edit_schematic_components` entry. `true` writes
  `(dnp yes)` and `false` writes `(dnp no)` on every placed unit of the
  reference, in the same single write as the rest of the edit. Omitting it
  changes nothing. A symbol from before KiCad 7, which has no token, gets one
  after `(on_board …)`/`(in_pos_files …)`, where KiCad writes it. KiCad 10
  per-variant overrides inside `(instances …)` are not touched. `unit` does
  not select a unit for `dnp`: it scopes `field_placements` only, so
  `edit_schematic_component` refuses `unit` beside `dnp` when no
  `field_placements` are given. A unit carrying two `(dnp …)` tokens refuses
  the whole `edit_schematic_component` call; in the batch it is an entry
  error, like a malformed field.
- **Now refused:** a property named `DNP` in any case, through
  `edit_schematic_component`'s and `batch_edit_schematic_components`' `fields`
  and through `add_component_annotation`'s `key`, whatever the value's type.
  The whole request is refused with `invalid_argument` naming the `dnp`
  argument, before anything is written: no other field or batch entry in it
  is applied. A `DNP` property a file already carries is neither refused on
  other edits nor removed.
- **New response fields:** `dnp` on `get_schematic_component` (the lowest
  placed unit's state, plus `dnp` on each `units` entry) and on each `list_schematic_components`
  row, read from the file. Every response built from the committed component
  readback (`edit_schematic_component`, `move_schematic_component`,
  `rotate_schematic_component`, `replace_component`, `add_schematic_component`
  and the other placement tools) carries the same fields. That readback now
  also checks that the written file holds the expected DNP state: the edit's
  requested one, and otherwise the one the file had before. A
  `batch_edit_schematic_components` entry that set `dnp` reports the state
  read back from the written file.

### project-file writers also refuse while KiCad holds the project (patch release)

`set_design_rules`, `set_predefined_sizes`, `create_netclass` and
`assign_net_to_class` already refused while KiCad held the board. Eeschema
rewrites the project file from its own copy when it saves too, and it holds no
board. So with only the schematic editor open, these tools wrote the
`.kicad_pro`, reported success, and the next schematic save reverted the change.
Measured on KiCad 10.0.5 (#804).

All four now also refuse while KiCad's project lock, `~<project>.kicad_pro.lck`,
sits beside the project file. pcbnew, Eeschema and the project manager each
create that lock while they hold the project. The lock does not say which
program holds it, so the project manager's lock refuses too. The refusal is
`unsafe_file_fallback`, with reason `kicad_project_lock_present`, or
`kicad_project_lock_unreadable` when the lock cannot be inspected. It names the
lock and leaves the project file unchanged.

Close the project in KiCad, or make the change there, and retry. A lock left by
a crash, with no KiCad program running, refuses until it is deleted. Response
shapes are unchanged.

### `list_schematic_labels` reports each label's `uuid` (patch release)

Each entry in `list_schematic_labels`' `labels` array gains a `uuid` field
holding the UUID KiCad wrote for that label, for net, global and hierarchical
labels alike. Pass these to `batch_delete` to remove many labels in one call,
as `list_schematic_wires`' UUIDs already allow for wires. The existing fields,
including the `NetLabel`/`GlobalLabel`/`HierarchicalLabel` `type` values, are
unchanged (#746).

### computed schematic coordinates are reported as KiCad places them (patch release)

Pin positions, stub ends and symbol bounds are now rounded to the six decimals
KiCad writes before they reach a response. `batch_get_schematic_pin_locations`
used to report a pin at 101.6 + 15.24 as `116.83999999999999`; it now reports
`116.84`. The same holds for every response that carries a transformed pin
position, for `connect_to_net`'s and `batch_connect_to_net`'s stub ends, for
`create_symbol`'s `units[].pins[]`, and for `check_schematic_overlaps`'
`bounds_*` and `overlap_*_mm` and `get_schematic_layout`'s `width` and
`height`. A value changes only past its sixth decimal, by at most 0.0000005
mm; for the float noise this removes, by less than 1e-9 mm. Nothing else in
the request or response changes (#747).

### computed coordinates are written as KiCad writes them (patch release)

Coordinates that come out of arithmetic rather than the grid snap are now
rounded to the six decimals KiCad writes before they reach a file (#766):

- `move_labels_by_offset` used to write a label at 167.64 moved by −5.08 as
  `162.55999999999997`. It now writes `162.56`.
- `fix_connectivity` used to move a wire end onto a pin at
  `97.78999999999999`. It now writes `97.79`, and `fixes[].to` reports the
  rounded point, both in a dry run and when applied.
- `copy_routing_pattern` rounds the translated `(start …)`, `(end …)` and
  `(at …)` it writes. It finds no tracks on a board KiCad saved (#802), so
  this applies only to boards it can read. Its `dx` and `dy` response fields
  are unchanged.
- Every schematic item written as raw text is rounded the same way: the wires,
  buses, junctions and bus entries of `connect_pins`, `batch_connect_pins`,
  `add_schematic_connection`, `connect_passthrough`, `split_wire_at_point`,
  `add_bus`, `batch_add_bus`, `add_bus_entry` and `connect_pins_to_bus`; the
  labels `batch_connect_to_net`, `connect_passthrough` and
  `connect_pins_to_bus` place at the end of a stub; and the sheets and field
  positions `add_hierarchical_sheet` and `duplicate_sheet` write. A wire
  `connect_pins` draws to a pin endpoint is an example.

A sum like those above moves by less than 1e-12 mm. A coordinate a caller
passes with more than six decimals now moves by at most 0.0000005 mm, finer
than the 0.0001 mm KiCad itself stores in a schematic. Responses are
unchanged except `fixes[].to`: a computed point a response echoes is still
unrounded (#747), and a caller's own coordinate is still echoed as given.

### `launch_kicad_ui` reports a KiCad that exits during startup (patch release)

`launch_kicad_ui` used to return `launched: true` as soon as the KiCad process
spawned, even when KiCad exited straight away, as it does on Linux with no
`DISPLAY` or `WAYLAND_DISPLAY`. With `wait_ready: true` the call then waited
out `timeout_seconds` and returned `ipc_ready: false`, which read as "retry".

Now an exit before KiCad is up is an error whose message gives the exit status
and the last 20 lines of KiCad's stderr, each cut to 500 characters, the same
way `open_schematic_viewer` reports one. With `wait_ready: true` the wait ends
as soon as KiCad exits, even while a ping is waiting for an answer, rather than
at the timeout. With `wait_ready: false` KiCad is watched for one second,
so a successful call now takes about one second longer. Successful responses
are unchanged.

KiCad no longer inherits the server's stdin and stdout, which carry the stdio
transport's JSON-RPC stream. Both are `/dev/null` (`NUL` on Windows). KiCad's
stderr is read by the server and written to the server log as `kicad`
warnings, so that output now stops when the server exits (#764).

### `check_schematic_overlaps` passes symbols that meet at a pin tip (patch release)

`check_schematic_overlaps` used to compare one envelope per symbol, covering
both its drawings and its pins. A power symbol on a pin tip therefore shared
area with the symbol it connects to, and was reported as a
`component_overlap`. The tool now compares the drawings and the pins
separately. A pair is reported when the drawings share area, when a pin runs
into the other symbol's drawing, or when two pins meet anywhere but tip to tip,
as KiCad connects pins only there. Pins meeting tip to tip and drawings that only
touch are not reported. Response fields are unchanged: `bounds_a`, `bounds_b`,
`overlap_x_mm` and `overlap_y_mm` still describe the envelopes, so a pair
reported only for its pins can show a depth of 0 on one axis (#745).

### `move_labels_by_offset` moves the label it names, with its fields (patch release)

`move_labels_by_offset` used to edit the first `(at ` in each label's text. For
a label whose name contains `(at `, that match was inside the name: the tool
rewrote the name, left the label where it was, and still counted it in
`moved_labels`. It also moved only the anchor, so a global label's
`Intersheetrefs` field stayed at its old position.

It now edits the label's own `(at …)` and the `(at …)` of each of its fields,
as KiCad's own move does, and leaves the name alone. Moved coordinates carry no
float noise: `114.3 - 2.54` is written as `111.76`. After the write the tool
reads the file back. If any label of that name is not at its moved anchor, the
call fails with `mutation_outcome_uncertain`; the file has been written, so
reload it before retrying. Arguments and the success response are unchanged
(#803).

### `add_power_symbol` accepts a pin (minor release)

`add_power_symbol` takes `reference` + `pin_number` as an alternative to
`x` + `y`, like `connect_to_net` (#727). The symbol is placed on the pin's
endpoint, unsnapped, so an off-grid pin is still reached. When `rotation` is
omitted it is derived from the pin, so the symbol faces away from the
component body; an explicit `rotation` still wins.

`x` and `y` are no longer listed as required in the schema, and the advertised
`rotation` default of `0` is gone because it now applies only to coordinates.
A coordinate call behaves exactly as before. A call must give one selector:
mixing the two, or giving half of one, is `invalid_argument` naming the field.
A reference or pin that is not on the sheet is `stale_target`, and a pin that
more than one placed symbol carries is `ambiguous_target` with the candidates.
None of these refusals writes.

No tool, argument, or response field was renamed or removed.

### `update_pcb_from_schematic` refuses an update that would drop a footprint's groups, points or variants (patch release)

KiCad's API updates a footprint by replacing it with one built from the
footprint message sent, and that message carries none of the groups, points or
variants the footprint owns, so the replacement lacked them. A planned update
of such a footprint now makes the dry run and the apply a `conflict`, before
any commit, with one `footprint_children_not_carried` diagnostic per footprint
naming what it owns. Update those footprints with KiCad's Update PCB from
Schematic, which edits them in place. No item read lists these, so the sync
reads the board as KiCad writes it, and only when the plan updates a footprint;
a footprint missing from that text, or text that does not parse, refuses the
update the same way. A footprint's membership of a board-level group is not
affected: KiCad restores it on replacement. An update still drops what a
footprint owns beyond these three that the message does not carry, such as its
unit metadata, which KiCad's own Update PCB restores. The response shape is
unchanged (#836).

### KiCad 10's schematic editor is reported as unsupported, not stale (minor release)

KiCad 10.x reports an open schematic by its file name alone, with no project
and no sheet path; KiCad 11 adds the schematic editor to its IPC API and
reports both. The editor-navigation tools used to call that a malformed
identity and refuse with `stale_target`, which read as a Konnect-side race.

- `get_editor_state` now succeeds on KiCad 10 with a schematic editor open.
  The schematic editor is reported with `addressable: false`, no documents,
  its document, selection and cross-probe capabilities `unsupported`, and an
  `unavailable_reason` naming the KiCad version. The PCB editor is observed as
  before. The call used to fail as a whole.
- `get_editor_selection`, `mutate_editor_selection`,
  `resolve_navigation_target` and `resolve_cross_probe_target` refuse a
  schematic request on KiCad 10 with `unsupported_capability`,
  `capability: "schematic_editor_document_identity"`, and `kicad_version`
  where KiCad reports it. They used to return `stale_target`.
- A malformed identity on a supported path is still `stale_target`, now with
  the identity's own reason in every one of these tools.

No argument changed, and no identity is taken from the saved file, the board
or the caller in place of KiCad's (#771).

### `create_netclass` and `get_netclasses` carry the differential-pair settings (minor release)

`create_netclass` could not set a netclass's differential-pair width, gap or
via gap, and `get_netclasses` did not report them (#777). `create_netclass`
now takes `diff_pair_width`, `diff_pair_gap` and `diff_pair_via_gap`,
optional, in mm and greater than zero. An update changes only the ones named.
A new named class without them leaves them out of the project file, so its
width and gap keep resolving from the Default; the four older settings keep
their creation defaults. A Default is still written complete, and an
incomplete one is still repaired without touching values already set.

`create_netclass`'s reply and each class `get_netclasses` reports gain the
three fields. The reply reports the class as stored, `null` where it sets
none. `get_netclasses` reports them resolved, naming in `inherits` those taken
from the Default. KiCad 10.0.6 never fills a named class's `diff_pair_via_gap`
from the Default (`addMissingDefaults` has it commented out), so a named class
without its own reports `null` and does not list it. KiCad 10.0.6's router and
DRC never use a named class's via gap, and its Board Setup, which has no via-gap
column, drops one when its changes are applied.

No argument or field was renamed or removed.

### `get_net_connections` lists the net's pins and all its labels (minor release)

`get_net_connections` was described as returning a net's pins and labels but
returned only the labels and a `connected_points` count. It now adds `pins`,
one `{reference, pin, name, x, y}` entry per component pin on the net. As in
`kicad-cli sch export netlist`, power symbols and `PWR_FLAG`s (`#` references)
name a net but are not listed as its pins.

`labels` is now chosen by net rather than by name, and each entry carries the
`net` it spells. A net with several names (a `+3V3` power symbol beside a `VCC`
label) used to list only the labels spelling the requested name, while
`connected_points` covered the whole net; asked by any of its names, it now
lists every label on it, and `label_count` counts them all. A net with one name
answers as before, apart from the added `net` field.

A schematic where a placed symbol's library entry cannot be found is refused
with an error instead of answering with that symbol's pins missing (#853).

### setting a schematic sheet's title block (minor release)

New `sch_hierarchy` tool `set_schematic_title_block` sets the same fields as
`set_board_title_block` (`title`, `date`, `revision`, `company`,
`comment_1`..`comment_9`) on a schematic sheet file, whose sheets resolve
`${REVISION}`, `${ISSUE_DATE}` and the rest from it. `scope` is `sheet` (the
default) or `hierarchy`, which also sets every sheet file below the given one,
each shared file once, and commits them in one journaled file transaction.
An omitted field keeps its value, an empty string clears it, and a call that
supplies none is refused as `invalid_argument`.

The tool refuses before writing while KiCad's lock is beside any target or
beside the owning project's root schematic (`conflict`), when a placed sheet
file is missing (`file_not_found`), when a sheet lies outside the project
directory or the hierarchy is deeper than 20 sheets
(`unsupported_capability`), and when a sheet changed after it was read
(`conflict`). A title block holding something the model cannot represent is
refused as `unsupported_capability` with `capability:
"unrepresentable_title_block"`, by this tool and by `set_board_title_block`.
It returns each sheet's block read back after the write and `changed`; a
readback that differs is `mutation_outcome_uncertain`, naming every file the
transaction replaced. The `sch_hierarchy` toolset description now mentions
title blocks. Existing tools are unchanged (#714).

### setting the board's title block (minor release)

New `pcb_board` tool `set_board_title_block` sets any of `title`, `date`,
`revision`, `company` and `comment_1` to `comment_9`, the fields that
`${TITLE}`, `${ISSUE_DATE}`, `${REVISION}`, `${COMPANY}` and
`${COMMENT1}`..`${COMMENT9}` resolve from on the board. An omitted field keeps
its value, an empty string clears it, and a call that supplies none is refused
as `invalid_argument`. Values are stored verbatim.

When KiCad holds the board, the write goes through `SetTitleBlockInfo`, which
the PCB editor handles from KiCad 10.0.1. The change is unsaved and has no undo
step. An older KiCad answers `unsupported_capability`, and the file is not
touched. Otherwise the saved file's `(title_block …)` node is rewritten as KiCad
writes it, and nothing else in the file changes. Both paths return the block
read back after the write, `changed`, `source` and `saved` (`false` for the
live board); a readback that differs is `mutation_outcome_uncertain`. Existing tools are unchanged (#714, board half).

### `move_region` can move a whole block rigidly (minor release)

`move_region` takes an optional `include` argument. `"symbols"`, the default,
is the behavior so far: symbol units move, snapped to the 1.27 mm grid, and
wires stay. With `"all"`, everything inside the box moves by exactly
`(dx, dy)`: symbols and power symbols, wires, buses, bus entries, junctions,
local, global and hierarchical labels, directive labels, texts, text boxes and
sheets. No-connects follow their pins, and a marker on no pin moves with the
box. Graphic shapes and images stay where they are.

The rigid move refuses before writing, with `plan_blocked` and one reason per
problem, when a wire, bus, bus entry, symbol or sheet crosses the box edge (a
wire passing through with both ends outside counts, and so does a symbol with
its origin and a pin on different sides), or when any pin's net would change,
for example because a moved pin lands on a wire outside the box. A successful
response adds `items_moved`, counting the non-symbol items moved by kind;
no-connects stay in `no_connects_moved`. A label now keeps its fields with it when it
moves, as a global label's `Intersheetrefs`. Calls without `include` are
unchanged (#896).

### wire tools dot a T built from wire ends (patch release)

`add_wire`, `batch_add_wire`, `connect_to_net`, `batch_connect_to_net`,
`connect_pins`, `batch_connect_pins` and `add_schematic_connection` used to add a
junction dot only where a wire end landed mid-span on another wire, or where a
wire passed over a pin. When three or more wire ends met at one point, they
wrote no dot. KiCad still connected the ends, but the sheet showed a T that
looked like a crossing.

Now every point a new wire touches is judged by how many distinct exits meet
there, counted as eeschema's `JUNCTION_HELPERS::AnalyzePoint` counts them. A
wire ending there is one exit, and a wire passing through is two. Each symbol
with a pin tip there is one more, and so are a sheet pin and the wire side of a
bus entry. Three or more exits get a dot. So two wire ends meeting at a pin
tip, a sheet pin or a bus entry also get one. A plain corner, or two wires that
overlap and leave in the same direction, gets none. That includes a collinear
overlap whose end lands mid-span, which used to get a dot. Pins that one symbol
stacks on a point, or that two body styles put there, count once.
Connectivity, schemas and response shapes are unchanged (#897).

### sheet tools keep symbol instance paths to live sheets (patch release)

`add_hierarchical_sheet` used to add a path to each symbol of a reused child
file and never remove one. After `delete_sheet` and a re-add, the child kept a
path naming the deleted sheet, and every later Konnect write to it was refused
with `stale_target`. Linking a file now keys its symbols to exactly the sheets
that place it in the project. Paths naming a sheet that no longer exists are
dropped, but only when every sheet of the hierarchy can be read. Paths under
another project name are kept.

- `delete_sheet` drops the deleted sheet's path from a child file that other
  sheets still place. It adds `unlinked_symbol_instances`, the number of
  symbols it changed. A file nothing else places is left untouched.
- `duplicate_sheet` keys the copy's symbols to the new sheet only, not also to
  the source's sheets.
- A sheet added inside a child sheet is keyed from the project root through
  that sheet, as KiCad resolves it, rather than from the child file's own UUID.
  `project_name` for `add_hierarchical_sheet` and `duplicate_sheet` now
  defaults to the owning project's name, not the parent file's stem. A
  different name is refused as `invalid_argument`, since symbols keyed to it
  fail every later write. A child sheet placed more than once cannot take a
  new sheet (`invalid_argument` on `schematic`). A sheet file in a project
  folder that no project places is refused as `conflict`, because a sheet
  added inside it would be keyed to no real hierarchy. Each refusal comes
  before anything is written (#478).
- Only a project root receives the `sheet_instances` footer. A nested parent
  no longer gets the root's page-1 entry written into it.

### routed connections refuse to join another net (patch release)

`connect_pins`, `batch_connect_pins` and `add_schematic_connection` route an
L-bend, horizontal leg first. A leg that passed over another pin's tip got a
junction there, and a corner that landed on a sheet pin connected to it. Each
route also dotted every junction-less T anywhere on the sheet, which KiCad
reads as two nets. So pins, labels and sheet pins on other nets (or on none)
silently joined, and the call still reported success (#887).

Each route now dots only the T's it forms itself, and is checked before it is
written. If the horizontal-first bend would join anything beyond the nets at
its two endpoints, the vertical-first bend is tried. Ending on a wire mid-span
still taps that wire's net. If both bends would join another net, or a placed
symbol's pins cannot be resolved so the check cannot see them,
`connect_pins` and `add_schematic_connection` refuse with `plan_blocked` and
write nothing. `reasons` holds one entry per bend tried.
`batch_connect_pins` skips that pair, adds
`"<ref1>.<pin1> -> <ref2>.<pin2>: <reasons joined by '; '>"` to `errors`,
still writes the other pairs, and counts only routed pairs in
`connected_count`.

Because the corner can now be on either side, `connect_pins` and
`add_schematic_connection` add `path`: the route's points as `[[x, y], …]`,
corner included. A route that merely crosses another wire without a junction
is not a connection and is unchanged.

### pin-location tools report each placement's mirror (minor release)

`get_schematic_pin_locations` and `batch_get_schematic_pin_locations` now
return `mirror_x` and `mirror_y` beside `rotation`, at the top level (the
lowest unit, like `x`, `y` and `rotation` there) and on each `units[]` entry.
They are the names `get_schematic_component` already uses. The fields are
additive; pin positions are unchanged (#855).

### `add_schematic_text` writes coordinates as KiCad writes them (patch release)

`add_schematic_text` wrote the caller's `x`, `y` and `rotation` into the file
as given, so a value carrying float noise, such as `54.60999999999999` from an
older response (#747), was written that way. Each is now rounded to the six
decimals KiCad writes, moving the text by less than 1e-6 mm. The response still
echoes the caller's values. When the saved file does not hold the text at the
rounded position, the call returns `mutation_outcome_uncertain`. A value so
large that rounding makes it infinite, such as `1.8e303`, is refused as
`invalid_argument` before anything is written. Arguments are unchanged (#828).

### library symbols take Footprint, Description, keywords, filters and custom fields (minor release)

`create_symbol` accepts five new optional arguments:

- `footprint`
- `description`
- `keywords`, a space-separated string
- `fp_filters`, an array of patterns
- `fields`, custom fields such as `MPN` or `LCSC` as name: value, the same
  shape `edit_schematic_component` takes

Before, `create_symbol` always wrote an empty Footprint and Description and
could write none of the others. Custom fields are written hidden, in name
order. An empty `keywords` or `fp_filters` writes no property, as KiCad does. A
custom field named after a mandatory field (in any case), or one with a `ki_`
prefix, is refused as `invalid_argument`. Footprint, Description, keywords and
filters have their own arguments, and `set_symbol_properties` does not change
Reference, Value or Datasheet. `create_symbol` now also escapes `"` and `\` in
field values. Before, a quote in `value` or `datasheet` produced a library that
did not parse.

The new tool `set_symbol_properties` (toolset `library`) changes the same
fields on an existing symbol, named by `library_path` and `symbol_name`.

- Only the fields passed change. An existing field keeps its position, size
  and visibility.
- A new field is written hidden, in KiCad's own field order.
- An empty `keywords` or `fp_filters` removes that property.
- On a KiCad 6/7 library, `description` replaces the `ki_description`
  property, which is where KiCad reads it from in that format.
- The response's `changes` gives `{"previous", "current"}` for each changed
  field, keyed by KiCad's property name, with `null` for an absent property.
- A call that changes nothing writes nothing.

Refusals:

- A file that changed after it was read is refused as `conflict`.
- A symbol name defined twice is refused as `ambiguous_target`.
- A file that is not a symbol library is a `handler_error`, like other paths
  that exist but cannot be used.
- When the file does not hold the edit on readback, the tool reports
  `mutation_outcome_uncertain` (#892).

### `set_footprint_metadata` reports what it replaced (patch release)

`attributes` replaces the whole `(attr …)` block, so a caller who added
`exclude_from_bom` without repeating `smd` or `through_hole` silently left the
footprint with no type. KiCad then reads it as unspecified (#895).

The response now adds `changes`, which maps each changed field to its
`previous` and `current` value: a string, or `null` when absent, for
`description`, and arrays for `tags` and `attributes`. It also adds `warnings`,
an array of strings that is empty when there is nothing to say. A warning is
given when an `attributes` replacement drops the footprint type, or drops an
attribute the `attributes` argument does not accept, such as
`allow_soldermask_bridges` (#902). `current` reflects what was written.
`success`, `footprint_path` and `changed_fields` are unchanged, and so is what
is written.

### footprint flips mirror inner copper layers (patch release)

A footprint that changes sides now moves its inner copper items the way KiCad's
`FlipLayer` does. On N copper layers, `In<k>.Cu` goes to `In<N-1-k>.Cu`, so
`In2.Cu` and `In5.Cu` swap on an 8-layer board. Both side changes are covered:
`flip_component` on a closed board, and `update_footprints_from_library` on a
back-side footprint. Both used to leave inner layers where they were. The
refresh now also mirrors text on inner copper, as KiCad does. Wildcard, user
and other non-copper layers are unchanged, and so are a pad's
`zone_layer_connections`, which KiCad's own flip also leaves alone.

When an inner layer the footprint uses is not on the board (`In5.Cu` on a
4-layer board), both refuse instead of following KiCad, which clamps it to
`In1.Cu`. `flip_component` returns `plan_blocked` with `operation:
"flip_component"` and leaves the file unchanged. The library refresh reports a
`footprint_update_conflict` diagnostic for that reference.

The file flip reads the stack from the board's `(layers …)` table, or assumes
KiCad's two-layer default when there is none. It returns `plan_blocked` for a
table whose copper layers are not `F.Cu`, `In1.Cu`..`In<N-2>.Cu` and `B.Cu`,
and refuses a board with two tables or a pad with a per-layer `(padstack …)`,
whose inner entries KiCad would also move. The refresh takes KiCad's
copper-layer count, and its `plan_revision` now includes that count, so a
revision from an earlier build needs a new dry run. The native IPC flip is
unchanged (#831).

### a copper zone blocks pad-net changes only on its own net (patch release)

`update_pcb_from_schematic` refuses to move a pad between nets while copper
on the old or new net is routed (`routed_pad_net_change`). On a board with any
zone, it used to count every board net as routed, so no pad on such a board
could change net. A board-level copper zone now counts as copper on its own net
only, as KiCad reports it in `Zone.copper_settings.net`, and a rule area counts
as none. A plan that conflicted only because of a pour on another net is now
`ready`. A zone whose net KiCad does not report still blocks every net.
Response shapes are unchanged (#779).

### board file fallbacks refuse a board that changed mid-edit (patch release)

`add_layer` always edits the saved board file. `set_board_size`,
`add_board_outline`, `delete_graphics`, `add_board_text`, `add_zone`,
`add_copper_pour` and `import_svg_logo` edit it when no live KiCad holds the
board. They used to read the file, splice in the edit and replace it
unconditionally, so a save that landed in between (KiCad's autosave, an editor,
another call) was overwritten with an edit of the older read, and the call
still reported success.

The write now goes through only while the file still holds the bytes the edit
was read from. Otherwise the call returns `conflict` with the board in `paths`
and leaves the newer file as it is; reload and retry. If the file no longer
holds the edit when read back after the write, the call returns
`mutation_outcome_uncertain`; reload and inspect the board before retrying.
Successful responses and the IPC path are unchanged (#845).

### board text, logo and zone writers refuse an unknown layer (patch release)

`add_board_text`, `import_svg_logo`, `add_zone` and `add_copper_pour` now
refuse a `layer` KiCad has no board layer for, such as the typo `F.Silk`, as
`invalid_argument` with `field: "layer"`. Before, a live KiCad was sent the
item on `BL_UNDEFINED`, which KiCad 10 does not validate (#237), and the file
fallback wrote the name into the board, which KiCad 10.0.6 then refuses to
load. The refusal comes before KiCad is asked anything or the board is read,
so neither changes. Every name these tools accepted and KiCad also accepts
still works, and the `F.SilkS` default is unchanged. Response shapes are
unchanged (#844).

### `add_board_text` writes coordinates as KiCad writes them (patch release)

When no live KiCad holds the board, `add_board_text` wrote the caller's `x`,
`y` and `rotation` into the file as given, so a value carrying float noise,
such as `54.60999999999999` from an older response (#747), was written that
way. Each is now rounded to the six decimals KiCad writes, moving the text by
less than 1e-6 mm. The response still echoes the caller's values. When the
saved file does not hold the text at the rounded position, the call returns
`mutation_outcome_uncertain`. A value so large that rounding makes it
infinite, such as `1.8e303`, is refused as `invalid_argument` before the file
is written. The live-editor (IPC) path is unchanged. Arguments are unchanged
(#838). The prose `message` of every `mutation_outcome_uncertain` result now
says "file mutation" instead of "schematic mutation"; its fields are unchanged.

### `export_specctra_dsn` refuses per-layer padstacks (patch release)

A KiCad 10 pad in `front_inner_back` or `custom` padstack mode can have a
different shape or size on `B.Cu` and the inner layers. The exporter read only
the front geometry and wrote it on every copper layer, so Freerouting routed
against copper the board does not have. KiCad 10.0.6's own DSN exporter does
the same, so the native bridge is no remedy.

`export_specctra_dsn` now refuses such a pad, or a locked via with such a
padstack, under every `native_bridge_mode`, with a `handler_error` naming the
item and its mode. Nothing is written. A normal padstack, which KiCad saves
with no `(padstack ...)` block, exports as before. To route the board, set the
padstack back to normal in KiCad. Response shapes are unchanged (#842).

### `export_specctra_dsn` turns rotated footprints' pins once (patch release)

A pad's angle in a `.kicad_pcb` already includes its footprint's rotation. The
Rust exporter wrote that angle as the pin's `(rotate ...)`, and the
footprint's `(place ...)` rotation turned the pin again, so Freerouting routed
around every rotated footprint's pads at the wrong orientation. A pin's
`(rotate ...)` is now the pad's angle within its footprint, normalized to
[0, 360), kept to six significant digits and omitted when 0, as KiCad's own
exporter writes it. A footprint at 0° whose pad angles have at most six
significant digits exports as before.

KiCad's native DSN of a board whose pins carry a `(rotate ...)` could not be
read, so `native_bridge_mode` `prefer` fell back to the Rust export and
`require` refused. Both now read it, and treat a native pin rotation that
differs from the Rust export's like any other mismatch. Arguments, response
fields and the manifest are unchanged (#840).

### `export_specctra_dsn` exports rounded-rectangle pads (minor release)

`export_specctra_dsn` refused any board with a `roundrect` pad, which covers
most stock SMD footprints. It now exports them. Specctra has no rounded
rectangle, so the DSN carries each one as a polygon, as KiCad's own exporter
does. The polygon's edges are tangent to the corner arcs, so it encloses the
rounded rectangle and stands at most about 1 µm outside it. As for every pad
shape, the pad's size is first rounded to the micrometre (#790).

What changes for a caller:

- The response's `capabilities.supported_profile` is now
  `two_layer_front_side_circle_rect_roundrect_no_existing_routing_or_zones`;
  it was `two_layer_front_side_circle_rect_no_existing_routing_or_zones`.
- In the reverse manifest, `supported_profile.pad_shapes` lists `roundrect`.
  A rounded-rectangle padstack has `"shape": "roundrect"` and a new
  `corner_radius_um`: KiCad's `min(width, height) * roundrect_rratio`, rounded
  down to the micrometre. Circle, rect and via padstacks keep their old
  fields. `plan_specctra_ses_import` and `apply_specctra_ses` accept both
  forms.
- A chamfered pad, which KiCad also writes as `roundrect`, is still refused,
  now with `pad <ref>-<n> has chamfered corners`.
- With `native_bridge_mode` `prefer` or `require`, KiCad's polygon for a
  rounded-rectangle pad is checked against the pad, not against the Rust
  polygon, because the two exporters approximate the corners differently. The
  polygon may sit inside the pad by KiCad's default 5 µm arc error and outside
  it by the corner growth KiCad's exporter adds. A project whose maximum arc
  error is set above 5 µm fails this check, so `require` refuses and `prefer`
  falls back to the Rust export. Circle and rect pads still have to match
  exactly. KiCad's native DSN of most stock footprints is still refused before
  this check, for reasons unrelated to pad shape (#841).

### `export_pdf` and `export_svg` default to the front view (patch release)

Both tools documented an omitted or empty `layers` as "all visible layers", but
passed no layer list to `kicad-cli`, which refuses that with "At least one layer
must be specified". `kicad-cli` has no visible-layers option, so the
documented default never worked.

An omitted or empty `layers` now plots the board's front view on one page:
whichever of F.Cu, F.Mask, F.SilkS, and Edge.Cuts the board declares, drawn
in that order so the silkscreen sits on top. Inner
and back copper are left out, because every copper layer on one page cannot
be read. Pass `layers` for any other view. Both responses now include the
`layers` that were plotted, as `export_gerber`'s already does. A `layers` item
that is not a string is refused as `invalid_argument` instead of being dropped
(#465).

### `rename_project` refuses a file that changed mid-rename (patch release)

`rename_project` used to read each file it rewrites and write it back
unconditionally. If KiCad or another writer saved one of them in between, the
rename replaced that save with the older copy plus the new name, and reported
success.

It now reads every file before renaming any, and rewrites them in one journaled
transaction that replaces a file only if it still holds what was read. If one
changed, or KiCad opened one under its new name, nothing is rewritten, the
renames are undone, and the call returns `conflict` naming that file under its
old name. Retry the rename. As with other writes, a KiCad lock on a board
returns `unsafe_file_fallback` rather than `conflict`.

Three refusals now come before anything moves:

- KiCad holds one of the files under its old name (a `~NAME.kicad_sch.lck` or
  `~NAME.kicad_pcb.lck` beside it). Its next save would recreate the old name,
  outside the renamed project. Close the project in KiCad first.
- A file cannot be read.
- A `.konnect-transaction-*.json` journal left by an earlier interrupted call
  cannot be recovered. Use `konnect transaction status` to inspect it.

When the outcome cannot be proven, the call returns
`mutation_outcome_uncertain`. This happens when the transaction fails after its
journal was written (the `path` is that journal, and the files stay renamed), or
when undoing the renames fails. Arguments and the success response are unchanged
(#851).

### Windows errors name resolved paths without `\\?\` (patch release)

On Windows, errors printed a board or lock path that Konnect had resolved in
its extended-length form, `\\?\C:\…`, beside the caller's own spelling of the
same directory. These now print the ordinary form: `C:\…` for a drive path and
`\\server\share\…` for a `\\?\UNC\…` network path. A path that the ordinary
form would name differently keeps its prefix: one of 260 characters or more, or
one with a reserved device name such as `NUL`, or a component that ends in a
dot or a space.

This covers every path in `wrong_document`, `ambiguous_target`, `stale_target`
and `ambiguous_open_board` (`requested`/`target`/`path`, `open_documents`,
`candidates`, `previously_bound`, and the reason text), and the sibling-lock
path in `unsafe_file_fallback` messages and in `source_evidence.detail`. A path
the caller typed prints as typed unless it began with `\\?\` itself. Board
matching still compares resolved paths. Field names and error kinds are
unchanged, and other platforms are unaffected (#673).

### a listener that closes mid-negotiation is `handshake_failed` on Unix (patch release)

A dial to a listener that accepts and then closes before NNG's negotiation
could report `reason: "transport_error"` with "Socket is not connected", as
seen on macOS (os error 57), depending on which call saw the close first. On
every Unix target that `ENOTCONN` now reports `handshake_failed`, as `Closed`
and `ConnectionReset` already did and as NNG itself classifies `WSAENOTCONN`
on Windows. No other raw OS error changes, and response shapes are unchanged.
### batch_delete says where reference designators go (patch release)

`batch_delete` deletes components by `references` and other items by `uuids`.
A reference passed in `uuids` is still not deleted, but its error now names the
right argument: `UUID 'J3' not found; 'J3' is a component reference, pass it in
references`. Other unknown UUIDs, and a reference also passed in
`references`, keep `UUID '<id>' not found`. The tool description and the
`uuids` property description now name both arguments. Argument types and
response shapes are unchanged.
### `update_pcb_from_schematic` keeps parts excluded from the BOM on the board (minor release)

The sync dropped every symbol marked *Exclude from bill of materials*, so test
points, wire pads and mounting holes never reached the board, and a part with
both flags off was dropped without being listed (#875). KiCad's own Update PCB
removes only a part marked *Exclude from board*; *Exclude from BOM* sets the
footprint's attribute.

The sync now does the same:

- A symbol with `(in_bom no)` and `(on_board yes)` is added or updated like any
  other, its footprint's `exclude_from_bill_of_materials` attribute set from the
  symbol, as `dnp` is. A footprint whose attribute differs from its symbol is
  planned as an update.
- Only `(on_board no)` keeps a part off the board, and such a part is always
  listed under `skipped_by_flag`, whatever its BOM flag.
- Each `add` and `update` change carries `exclude_from_bom` beside `dnp`.

A board synced before this change gains the footprints the old sync dropped on
its next sync. No argument was renamed or removed.

### omitted rectangular symbol pin coordinates are distributed (patch release)

`create_symbol` no longer places every coordinate-less rectangular-unit pin at
one point. On each angle-selected edge (left by default), omitted `y` values
for left/right pins or omitted `x` values for top/bottom pins are spaced at
2.54 mm pitch in input order. Supplied edge-axis coordinates are preserved and
automatic slots avoid them. Repeated physical pin numbers share an automatic
slot on the same edge, preserving intentional graphical stacks.

This shared rectangle behavior applies to `pins`, `units[].pins`, and a glyph
that falls back to a rectangle. Existing body sizing can still move the other
coordinate outward to meet the body; use the returned resolved pin coordinates
for wiring. Custom `graphics` still require explicit coordinates and glyphs
that fit retain their established layout. Tool schemas and response shapes are
unchanged; this does not infer electrical pin roles or redesign a supplied layout.

### `update_pcb_from_schematic` copies symbol fields onto footprints (minor release)

The sync carried no symbol field beyond Reference, Value and the footprint.
`LCSC`, `MPN`, Datasheet, Description and every custom field stayed off the
board, and a field changed in the schematic never reached it. DRC's schematic
parity reported each one, as `Missing symbol field` or as a field that differs.

The sync now copies the fields KiCad's Update PCB from Schematic copies, with
that dialog's defaults: "Update footprint fields from symbols" on, "Remove
footprint fields not found in symbols" off.
- **Which fields:** every symbol field but Reference, Value, Footprint and
  Component Class, an empty one included.
- **A field the footprint already has** keeps its layer, position, visibility
  and font, and takes the symbol's text. That includes a field the library gave
  it, so the symbol's value wins, as in KiCad.
- **A field the footprint lacks** is added hidden on F.Fab, or B.Fab for a
  footprint on the back, at the footprint's position and rotation. It uses
  KiCad's default fabrication text: 1.0 mm, with a 0.15 mm stroke. Datasheet and
  Description, which every KiCad footprint has, are made as KiCad's footprint
  constructor makes them when the library carried neither.
- **A footprint field the symbol lacks** is left in place.

This covers added footprints and existing ones. A footprint whose fields alone
differ from its symbol's is now planned as an update, whoever placed it. An
update sends the whole footprint back to KiCad, as every update already did, so
such a board shows an update for each footprint whose fields differ on its next
sync.

Each planned change gains `fields`, a list of `{name, value}`. An addition lists
every field it receives; an update lists only the fields it changes. The
post-apply readback compares each one's text, layer and visibility, as it does
the library's fields.

Not covered: the per-variant field values a KiCad 10 netlist can carry, and the
board's own text settings, which Konnect does not read. The styles above are
KiCad's defaults. With Board Setup's "Apply board defaults to footprint fields"
on, which is off by default, KiCad restyles every field of a footprint it adds
from those settings, and the sync does not (#788).

### `update_pcb_from_schematic` gives footprints their library's fields (patch release)

The previous release carried a footprint's library attributes, description,
tags and 3D models. Its fields still came from a fixed template:
- Value went on the silkscreen, hidden, and Reference and Value had no font
  thickness;
- the library's Datasheet, Description and custom properties, such as the
  `KiLib_Generator` on 12,018 stock footprints, were left off.

A footprint the sync adds now has the library's fields:
- **Reference and Value** with the library's layer, visibility, font,
  position and keep-upright setting, holding the schematic's text;
- **Datasheet and Description** where the library has them;
- **every custom property.**

The post-apply readback also checks each field that was sent: its text, layer,
visibility and whether it is kept upright. A difference is reported as
`board_readback_differs`, as other readback differences are.

A footprint whose properties cannot be read is still placed, with the default
fields as before. The plan then carries a non-blocking `library_fields_not_carried`
diagnostic naming the footprint, its parts and the reason. 15 of KiCad 10.0.5's
15,451 stock footprints repeat a property name and are placed this way.

The library property reader is shared with `update_footprints_from_library`.
Both now read `(unlocked yes)` on a property instead of refusing the footprint:
3,353 stock footprints carry it. The clause is KiCad's keep-upright setting,
not a lock: a field without it is kept upright, and one with it is not. Fields
used to be sent as not kept upright, so KiCad saved `(unlocked yes)` on every
field either tool wrote; they now keep the library's setting.
`update_footprints_from_library` still refuses footprints for any other child
it cannot represent, including a `(locked …)` clause on a property, which no
stock footprint carries.

### `copy_routing_pattern` copies routing from boards KiCad saved (patch release)

`copy_routing_pattern` matched `(segment` and `(via` as two-space-indented
text, which KiCad 10 never writes, so it copied nothing from a board KiCad had
saved and reported `copied: 0`. It now reads top-level `segment`, `arc` and
`via` items from the parsed board (#802).

- **Selection:** an item is copied when every point that places it (a
  segment's `start`/`end`, an arc's `start`/`mid`/`end`, a via's `at`) is
  inside the source region, edges included. Vias used to be skipped even on
  the old layout, because the filter read a `(start …)` they do not have.
- **Crossing items:** an item with only some points inside is not copied. It
  is listed in the new `excluded_crossing` array as `{kind, uuid}`.
- **Copies** keep every attribute (layer, net, width, size, drill, layers,
  `locked`, …), get a fresh `uuid` (or `tstamp`, on boards from KiCad 7 and
  earlier), and have each coordinate rounded to the six decimals KiCad writes.
- **Refusals, with the board untouched:**
  - KiCad holds the board, or a KiCad lock file sits beside it.
  - `invalid_argument`: `src_x2 < src_x1` or `src_y2 < src_y1`; a destination
    equal to the source anchor (field `dest_x`), which would stack every copy
    on its original; a `net_map` value that is not a string, or names a net
    no item on the board carries (field `net_map.<name>`); `net_map` on a
    board that references nets by number, as KiCad 9 boards do (field
    `net_map`).
  - A routing item anywhere on the board without readable points or an
    identity, since whether it is in the region cannot be known.
  - `conflict`: the board changed after it was read. This is new: the file
    used to be replaced without that check.
- **Response:** `copied` is counted in the board text the write verified on
  disk. New keys are `copied_by_kind` (`segment`, `arc`, `via`), `uuids` (the
  copies) and `excluded_crossing`. A successful write adds the `warning` the
  other file-only board edits carry. `dx`/`dy` are rounded the same way.

### project-file writers refuse while KiCad holds the board (patch release)

`set_design_rules`, `create_netclass` and `assign_net_to_class` edit the
board's sibling `.kicad_pro`. When KiCad saves a board it holds, it rewrites
that file from its own copy. So an edit made while KiCad held the board was
reported as a success and then reverted on KiCad's next save, with no warning.
On KiCad 10.0.5, all three values went back as soon as `run_drc` with
`sync_live_board` saved the board.

All three tools now run the check `set_predefined_sizes` already runs. While
KiCad holds the board, they refuse and leave the project file untouched. The
check comes before the file is read as well as before it is written, so a call
that would change nothing is refused too: while KiCad holds the board, the file
can be older than what KiCad holds. Close the board in KiCad, or make the
change there, and retry. A KiCad that holds a different board, or none, does
not affect these tools. Response shapes are unchanged (#791).

### `get_board_extents` names what KiCad will not list, and measures text boxes (minor release)

KiCad 10 will not list tables or generators through `GetItems`; it answers
`AS_BAD_REQUEST`. The live read used to skip both without a word, so an answer
read as complete bounds when it was not. It now asks for both on every read and
adds `unavailable_item_classes`, one entry per class it could not measure:

- `{"class": "tables", "reason": "refused", "kiapi_status": "AS_BAD_REQUEST",
  "message": "…"}` carries KiCad's own answer;
- `{"class": …, "reason": "undecodable", "listed_count": n}` is for a KiCad that
  does list a class the bundled protocol has no message for, so the items
  cannot be measured.

A class that Konnect does measure, if KiCad refuses it the same way, is now
named there too, and the rest of the read still answers. It used to fail the
whole read. Any other failure still fails the read. A saved-file answer
carries `unavailable_item_classes: []`, since it lists every item and names
what it cannot measure in `unmeasured_item_counts`.

The saved file now measures text boxes, as KiCad does: their corners, or the
four `pts` KiCad writes for a turned box, plus half the stroke width whether or
not the border is drawn. Text overflowing the box does not count. Board text
boxes move from `unmeasured_item_counts` to `measured_item_counts`
(`text_boxes`). Footprint text boxes count toward their footprint's box, except
on `Cmts.User`, `Dwgs.User`, `Eco1.User`, `Eco2.User` or a private layer, and
`footprint_text_boxes` is gone from `unmeasured_item_counts`. A saved-file
extent can therefore grow where a board has text boxes.

A footprint point or a via layer whose `size` is present but unreadable is now
skipped and counted in `skipped_item_count`. The point used to take the 1 mm
default, and the via used to keep its base diameter (#688).

### `update_pcb_from_schematic` places footprints with their library data (patch release)

Footprints that `update_pcb_from_schematic` added used to reach the board with
only their pads, drawings and Reference/Value fields. Everything else the library
footprint carries was left at its default:

- the mounting style (`smd`, `through_hole`) and exclusion flags from `(attr …)`;
- the description and keywords;
- every 3D model.

As a result, KiCad treated each synced part as unspecified. `export_3d` with its
default arguments left all of them out of the STEP, and KiCad's DRC flagged each
one as `lib_footprint_mismatch`.

A footprint the sync adds now carries all of these from its library. DNP still
comes from the schematic, overriding a library `dnp`, as in KiCad's own Update
PCB from Schematic.

The post-apply readback also checks each footprint's instance attributes and 3D
model files against what was sent. The attributes are the mounting style,
`board_only`, `exclude_from_pos_files`, `exclude_from_bom`,
`allow_missing_courtyard`, `allow_soldermask_bridges` and `dnp` (DNP as finally
sent, after the schematic's override). A difference is reported in `diagnostics`
as `board_readback_differs`, the same way a changed drawing count is reported.

`update_footprints_from_library` uses the same readers, so it now accepts a 3D
model hidden with KiCad 10's `(hide yes)`. 16 models in KiCad 10.0.5's own
library are written that way, and the refresh used to refuse those footprints.
Both tools now refuse a model whose `(opacity …)` is present but not a number,
where it used to be read as fully opaque. An absent opacity still defaults to
fully opaque.

Library properties (Datasheet, Description and custom fields) and each field's
layer and visibility are not part of this change (#789).

### grid-snapped coordinates are written as KiCad writes them (patch release)

A point snapped to the 1.27 mm grid is now rounded to the six decimals KiCad
writes. `bulk_move_schematic_components` used to write a symbol at
132 × 1.27 as `(at … 167.64000000000001 …)` and report the same value as
`placements[].new_y`. It now writes and reports `167.64`, and `add_wire`'s
`added_wire` echo is rounded the same way. The values differ by less than
1e-12 mm; nothing else in the request or response changes (#744).

### `open_schematic_viewer` reports a viewer that exits during startup (patch release)

`open_schematic_viewer` used to return `launched: true` as soon as the viewer
process spawned, even when it crashed straight away, for example on Linux with
no `DISPLAY` or `WAYLAND_DISPLAY`. It now watches the viewer for one second. If
the viewer exits in that time, the call is an error whose message gives the exit
status and the last 20 lines of the viewer's stderr, each cut to 500
characters. A viewer that is still running after that second gets the same
successful response as before. Every successful call now takes about one second
longer, and on the stdio transport no other request is answered during it.

The viewer no longer inherits the server's stdin and stdout, which carry the
stdio transport's JSON-RPC stream. Its stderr is read by the server and written
to the server log as `schematic_viewer` warnings, so that output now stops when
the server exits (#702).

### `get_board_extents` reads the live board and measures every item (minor release)

`get_board_extents` used to ask KiCad for the box of an empty item list.
KiCad 10.0.5 answers that with no boxes on every board, so the IPC branch never
answered. The tool then discarded the failure and answered from the saved file,
using only top-level `gr_line` endpoints and footprint anchor points (#688).

It now lists each class of board item and has KiCad measure every one in
`BBM_ITEM_ONLY` mode, which leaves footprint text out. It reads through the same
board-source contract as `get_layer_list`:

- **New argument:** an optional `board_source` (`auto`, `live`, `saved`). The
  default, `auto`, answers from KiCad when it holds the board.
- **When KiCad holds the board but cannot complete the read,** the call is
  refused rather than answered from a file that may be older than the editor.
- **The saved file is measured the way KiCad measures a live board:** pads,
  graphics with stroke width, tracks and arcs with width, vias and zone outlines.
  It agrees with KiCad item by item on its demo boards.
- **What the file cannot be measured for, it names:** board text, dimensions,
  text boxes, targets and the like are counted in `unmeasured_item_counts`.

Response changes:

- **New fields:**
  - `item_count`;
  - `measured_item_counts` and `unmeasured_item_counts`, objects keyed by item class;
  - `skipped_item_count` (unreadable items in the saved file);
  - `shared_kiid_count` (items KiCad listed under a KIID it had already listed);
  - `sources` (`{"bounds": "ipc"}` or `{"bounds": "saved_board"}`);
  - `source_evidence`.
- **`source`** still reads `ipc` or `file`.
- **A board with no items** now returns `null` for `x_min`, `y_min`, `x_max`,
  `y_max`, `width` and `height`, with `item_count: 0`. It used to return a zero
  box with `source: "empty"`.
- **Values change:** a saved-file answer now includes pads, tracks, vias, zones
  and stroke widths, so it is usually larger than before; a live answer is
  KiCad's own measurement.

`update_pcb_from_schematic` measures the live board the same way, and stages new
footprints 5 mm to the right of it. It used to stage them beside the page origin
(0, 0), because it read KiCad's empty answer as an empty board. Staged positions,
and therefore `plan_revision`, differ from plans computed before this change.
Apply already requires a fresh dry run.

`update_pcb_from_schematic` stages the footprints it adds beside the same live
measurement, and now says what that measurement covered. Its dry-run and apply
responses gain `staging`:

- `basis` is one of:
  - `complete_geometry`: every class was listed and measured;
  - `partial_geometry`: staged beside what KiCad measured, without the classes
    listed below (on KiCad 10.0.5, every board's tables and generators);
  - `empty_board`: every class was listed and none held an item;
  - `no_measured_geometry`: nothing was measured, but some classes could not
    be listed, so the board is not known to be empty. Staging starts at the
    origin.
- `unavailable_item_classes` uses the same shape as `get_board_extents`.

`staging` is `null` when the sync was refused before it read the board. It
never refuses a sync: staging is a starting position.

### `check_freerouting` reports where it searched (minor release)

The `jar_path` argument of `check_freerouting` and `route_specctra_dsn` was
described as "uses config default", but no Freerouting setting exists. Both
descriptions now list where discovery looks, in order: the directories named
by `KICAD10_3RD_PARTY`, `KICAD9_3RD_PARTY` and `KICAD8_3RD_PARTY`; the KiCad
10.0, 9.0 and 8.0 plugin folders `$HOME/Documents/KiCad/<version>/3rdparty/plugins`,
`$HOME/.local/share/kicad/<version>/3rdparty/plugins` and, on Windows,
`$USERPROFILE/Documents/KiCad/<version>/3rdparty/plugins`; `freerouting.jar` in
the server's working directory; `/usr/local/lib/freerouting` and
`/opt/freerouting`; then every directory on `PATH`. The search and both
descriptions are built from the same table of places, and the search itself is
unchanged.

When nothing is found, `check_freerouting` adds `searched_locations`: one
entry per location visited, with `kind`, `location_path`, `max_depth` and
`exists`, read from the server's own environment. `location_path` is always
absolute: a relative value from the environment is reported against the
server's working directory, where the search reads it. A location reached twice, such as the
`Documents` plugin folders when `HOME` and `USERPROFILE` are the same
directory, is searched once. When `jar_path` is passed and is not a file, it
reports `checked_path` and says so instead of giving download advice.
`route_specctra_dsn` now refuses such a `jar_path` with `file_not_found` naming
the path, where it used to answer "install Freerouting or pass jar_path"
(#787).

### `suggest_jlcpcb_alternatives` matches KiCad footprints and ranks by library type (minor release)

`suggest_jlcpcb_alternatives` used the last `_` segment of the footprint ID as
the package, so every standard KiCad footprint (`C_0402_1005Metric` becomes
`1005Metric`) matched nothing and returned `[]`. It now maps the footprint to
the package names LCSC uses: two-terminal chip parts to their imperial size
(`0402`), named discrete packages through a measured alias table (`SOT-23`,
`SOT-223`, `SOD-123`, `SMA` and others), and SOIC, SOP, TSSOP, MSOP, SSOP,
quad flat and leadless ICs by pin count and body size. Packages are compared
exactly, so `0402` no longer also matches `0402x4` arrays. A value without a
library prefix is still taken as LCSC's own package name, so `footprint: "0402"`
keeps working. A library footprint with no known LCSC package is refused with
`invalid_argument` on `footprint`, where it used to return an empty list.

The value now matches as a whole value in the description or the
manufacturer part number: no digit or `.` may come directly before or after
it. `10k` used to match `110kΩ` and `510kΩ`, and `20pF` matched `220pF`. Unit
spellings are not converted; `100nF` does not find `0.1uF`. An empty value is
refused.

Results used to be ordered by price alone. Parts with fewer than `min_stock_count`
units in stock (new, default 100) are now excluded. The rest are ranked Basic,
then Preferred, then Extended when `prefer_basic` (new, default true) is on,
then known price before unknown, price ascending, stock descending and LCSC
number. A stored price of 0 is treated as unknown (#582): it sorts after every
known price, and `max_price_usd` excludes it. The response adds
`package_match` (the rule and the LCSC names searched), `value_match`,
`ranking` (the criteria and the effective settings), `count`, `matched_count`
and `exclusions`, which counts the parts the stock floor and the price limit
removed (#785).
