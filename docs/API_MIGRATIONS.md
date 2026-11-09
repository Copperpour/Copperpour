# Copperpour MCP API migrations

Copperpour's tool names, arguments and responses are public API. This file
records intentional changes to them, newest release first, with the supported
replacement workflow.

Changes not yet released are in [`api-migrations/`](api-migrations/), one file
per change. `cargo xtask collect-api-migrations <version>` moves them here when
a release is cut.

Changes from Konnect, up to its last release, v0.13.0, are in
[Konnect's copy of this file](https://github.com/mixelpixx/Konnect/blob/v0.13.0/docs/API_MIGRATIONS.md).

## v0.16.1

### `copperpour init` registers `bin/copperpour` in the all-platforms package (patch release)

In the all-platforms PCM package, `bin/copperpour` is a launcher that runs
`bin-linux-x86_64/copperpour` or `bin-macos/copperpour`. `init`, `uninstall`,
`status` and the first-time setup now name the launcher, the documented path,
instead of the server it runs. Claude hooks naming that server are this
install's own: `init` replaces them, `uninstall` removes them, and `status`,
`get_installation_info` and `initialize` report them as `different` with reason
`alias_handler` instead of `other_executable`
([#400](https://github.com/Copperpour/Copperpour/issues/400)).

## v0.16.0

### `add_junction` and `batch_add_junction` skip points that already have a dot (minor release)

Both tools used to write a junction at every requested point, so a point that
already had one, or that a `batch_add_junction` call listed twice, got a second
dot stacked on the first. KiCad keeps such dots through a resave and its ERC
does not report them. Now a point with a dot is left alone
([#281](https://github.com/Copperpour/Copperpour/issues/281)).

`batch_add_junction`'s `added` is now the number of dots written, not the
number of positions given; the new `already_present` counts the rest.
`add_junction` keeps `added_junction` and adds `already_present: true` when the
point had a dot. A call that adds nothing no longer rewrites the file. Callers
that read `added` as the request size should use `added + already_present`.

### `apply_copper` (minor release)

A new tool, in a new `pcb_copper` toolset (`load_toolset('pcb_copper')`),
creates a batch of straight tracks and through vias on the board KiCad holds
open
([#95](https://github.com/Copperpour/Copperpour/issues/95)):
`apply_copper(board, tracks?, vias?, dry_run?, expected_plan_revision?)`. Each
track is `{start, end, width, layer, net}` and each via
`{position, drill, size, net}`, points as `{x, y}` in mm. It needs KiCad
running with the board open, and it never saves.

`dry_run` defaults to true: it checks every item and returns the normalized
plan, every length rounded to the nanometre as KiCad rounds it, each layer by
its canonical name and each net with its `net_code`, with `track_count`,
`via_count` and a `plan_revision`. The revision covers the plan, the board
KiCad named, its net table and its enabled layers, so a net renamed, added or
removed since the dry run makes an apply `stale_target`. An apply takes
`dry_run: false` and that revision as `expected_plan_revision`; without it the
call is `invalid_argument` and nothing is sent.

Before anything is written it refuses with `invalid_argument`, naming the
field, as `tracks[2].net`, and listing every fault in the message: a net the
board does not have, or an empty net (copper is never put on net 0); a layer
that is not copper, or that the board does not have; a width, drill or size
that is not above zero once rounded; a drill not smaller than its via's size;
a track whose ends round to the same point; a coordinate beyond the
±2147.483637 mm KiCad keeps; and a plan with no tracks and no vias.

An apply is `BeginCommit`, `CreateItems` and `EndCommit`: one commit, one undo
step. It is not an atomic transaction. KiCad answers `CreateItems` per item,
and its answer names the requested net even for an item it then stores on the
net of the copper the item touches (an end on a pad, a via on a track). So
after the commit every created item is read back by UUID, and the response
(`tracks` and `vias` with each item's `uuid`, geometry, layer and `net`, the
counts, `source: "ipc_readback"`) comes from that read-back:

- If KiCad stored any item on another net than requested, the whole batch is
  deleted again in a second commit, and the call fails with `plan_blocked`;
  `reasons` names each such item, its UUID and the net KiCad chose. KiCad's
  undo history then holds both commits.
- If KiCad refused some of the items, or reported one created without its
  UUID, the commit is dropped, and the call fails with `plan_blocked`;
  `reasons` names each such item with KiCad's status.
- In both cases the created UUIDs are then looked up again. When that lookup
  does not show them gone, the call fails with `mutation_outcome_uncertain`
  instead, its `reason` listing the UUIDs to inspect; do not repeat the call
  until they are inspected.
- A read-back that does not account for every created item is
  `mutation_outcome_uncertain` too, and nothing is removed.
- A `CreateItems` answer with a different number of results than items
  cannot be matched to the request: the commit is dropped and the call fails.

Not in this tool: skipping copper already on the board, arcs, blind, buried
and micro vias, zones. No existing tool changes or moves: `pcb_copper` is new,
and deleting and re-netting copper by UUID list are to follow there.

### `batch_add_junction` refuses a position missing `x` or `y` (patch release)

A `positions` item without `x` or `y` used to become a dot at 0 on that axis,
reported as added. Such a call is now refused as `invalid_argument` naming the
item and coordinate, such as `positions[0].y`, and nothing is written
([#282](https://github.com/Copperpour/Copperpour/issues/282)), as
`batch_add_wire` refuses a wire missing an endpoint. A call whose positions all
carry both coordinates behaves as before.

### Board writers write a layer's display name by its canonical name, and refuse `In01.Cu` (patch release)

`add_board_text`, `import_svg_logo`, `add_zone` and `add_copper_pour` accept
KiCad's display names for a layer (`F.Silkscreen`, `User.Drawings`,
`User.Comments`), but their board-file fallback wrote the name as given, and
KiCad 10 refuses to load a board whose item names a layer that way
([#99](https://github.com/Copperpour/Copperpour/issues/99)). They now write
the canonical name (`F.SilkS`, `Dwgs.User`, `Cmts.User`), and the response's
`layer` reports that name on both the IPC and the file path. A caller that
compared `layer` with the display name it sent should compare with the
canonical name.

The same four tools now refuse a copper or user layer number KiCad never
writes, such as `In01.Cu`, `In+1.Cu` or `User.01`, as `invalid_argument`
naming `layer`, before KiCad or the file is touched. They used to read it as
`In1.Cu` or `User.1`, and the file fallback wrote it verbatim. Write `In1.Cu`
or `User.1`.

### The bus tools and `set_layer_constraints` refuse a file changed after their read (patch release)

`add_bus`, `batch_add_bus`, `add_bus_entry` and `connect_pins_to_bus` now
replace the `.kicad_sch` only while it still holds the text they read, and
`set_layer_constraints` does the same for the board's `.kicad_dru`. Until now
each replaced its file unconditionally, so a file that KiCad, another server or
a concurrent HTTP call saved in between was put back to the older read, and that
change was lost while both calls reported success
([#200](https://github.com/Copperpour/Copperpour/issues/200)).

A file changed before the write now returns `conflict`, with the file in
`paths`, and is left as the other writer left it. Retry the call to apply the
change to the file as it is now. When there was no `.kicad_dru` at the read,
`set_layer_constraints` creates one without replacing anything, so a rules file
that appeared in between is also refused as `conflict`. A file that no longer
holds the change when read back after the write returns
`mutation_outcome_uncertain`; read the file before retrying. Arguments and
successful responses do not change.

`batch_add_bus` and `connect_pins_to_bus` no longer rewrite the sheet when
every item was rejected; they used to add a blank line. A file removed or
renamed after the read now returns `conflict` from these tools and from the
other revision-checked writers (the board, project-file and symbol-library
writers of [#153](https://github.com/Copperpour/Copperpour/issues/153)),
instead of an unstructured `handler_error`.

### `check_freerouting` and routing pick the Java they run, and report it (patch release)

Freerouting used to start with a bare `java`, the first one on the `PATH`. It
now runs `$JAVA_HOME/bin/java` when `JAVA_HOME` is set, then the `java` on the
`PATH`, and on macOS Homebrew's `openjdk` before the `/usr/bin/java` stub. On
Unix a candidate without an execute bit is skipped for the next one.

`check_freerouting` adds `java_path`, `java_release` and
`required_java_release` (the release the JAR's main class was built for). When
the Java found is older than the JAR needs, it answers `available: false` with
a note, instead of `available: true` and an `UnsupportedClassVersionError` at
routing time. `available` is now `true` only when the JAR actually ran
(`version_checked`); before, a JAR and any `java` were enough. When the JAR
does not run, `note` says so and points at `version_output`. Callers that
read `available` alone get the stricter answer and need no change.

### `check_schematic_overlaps` reports label and field text as `text_overlap` (minor release)

`check_schematic_overlaps` compared symbol drawings and pins, and labels only
where their anchors coincided, so two labels whose text ran into each other,
or a power symbol's Value lying over a label, reported clean
([#111](https://github.com/Copperpour/Copperpour/issues/111)). It now also
measures the text of every net, global and hierarchical label and of every
visible field of a placed symbol, and reports each that runs into another, or
into another symbol's body, as an entry of `"type": "text_overlap"`. `a` and
`b` each name their item: `{"kind": "label", "label_type", "net", "uuid",
"text"}`, `{"kind": "field", "reference", "unit", "symbol_uuid", "field",
"text"}` or `{"kind": "symbol_body", "reference", "unit", "symbol_uuid"}`.
Beside them are `overlap_x_mm`, `overlap_y_mm`, `bounds_a` and `bounds_b`, as
`component_overlap` has them. Text is compared at its ink, and a label's
outline or flag at its drawn line, as a body is, so outlines that only meet,
such as global labels stacked on the grid, are not reported; `bounds_*` and the
overlap depths keep the outline's stroke.
A symbol's own fields are not compared with its own body, and two labels of
different nets at one anchor stay a single `label_overlap`.

The text is placed and measured as eeschema draws it in KiCad's stroke font;
text in another font is measured as if it were. Free text, sheet fields and
sheet pins are not measured. `component_overlap` and `label_overlap` entries are
unchanged, but `overlap_count` now counts the text entries too, so a sheet that
reported 0 may report more. A caller that treats `overlap_count` as symbol
collisions only should count the entries whose `type` is `component_overlap`.

### Component edits accept a sheet placed more than once (patch release)

`edit_schematic_component`, `move_schematic_component`,
`rotate_schematic_component`, `mirror_schematic_component`,
`add_component_annotation`, `group_components` and `replace_component` refused
every symbol on a sheet placed more than once with distinct references as
`stale_target`, before writing
([#234](https://github.com/Copperpour/Copperpour/issues/234)). eeschema
writes a symbol's `Reference` property from one instance only, so the other
instances' designators differ from it by design. These tools now accept such a
symbol when one of its instance records names the property and every record
keeps its prefix, the rule placement already applies; with one record that is
still equality. Their readback holds the set of designators a unit's records
carry to what the file held before the edit. On such a sheet a unit's
`instance_references` in the response lists that set, sorted.

`new_reference` on such a sheet is refused as `ambiguous_target`, listing the
designators, because a rename would reach only one instance; set designators
per instance with `annotate_schematic`. A property that no record names, or a
record of another prefix, is still refused as `stale_target`. Arguments do not
change, and a caller that re-annotated or edited the file to get past the
refusal no longer needs to.

#### Known limitations

Without reading the hierarchy, a record for a path that is no longer a sheet
passes when another record names the property; before, it was refused when its
designator differed. A symbol whose unit differs between instances is still
refused as `ambiguous_target`.

### `connect_passthrough` dots its stub (patch release)

A stub drawn from a point where wires already met, or from mid-span on a wire,
used to get no junction dot
([#137](https://github.com/Copperpour/Copperpour/issues/137)). It now gets the
dots a new wire calls for, as a wire drawn with `add_wire` does. The response
is unchanged, and a stub that meets no other wire, and no pin past its root, is
written as before.

### `create_symbol` refuses a name the library already defines (patch release)

`create_symbol` used to append a symbol whose name the library already held
and report success. The library then had two top-level symbols of that name,
and KiCad's next save kept only one of them, without saying which. Now such a
call is `invalid_argument` on `name` and the library is left as it was
([#123](https://github.com/Copperpour/Copperpour/issues/123)). A name held
only by a unit sub-symbol (`NAME_0_1`) is still free. To replace a symbol,
call `delete_symbol` first. It removes one copy per call, so a library an
earlier release left holding two copies needs it twice. A library the parser
cannot read is now refused too, instead of having the symbol appended to it.

The write is now revision-checked: a library saved by another program between
`create_symbol`'s read and its write, or created there after the call found
none, is refused as `conflict` and left as saved. Read the library again and
retry.

### `delete_custom_rule` (minor release)

A new `verification` tool deletes one named rule from the board's sibling
`.kicad_dru` custom rules file: `delete_custom_rule(board, name)`, the
counterpart of `set_custom_rule`, which adds or replaces a rule but deletes
none ([#317](https://github.com/Copperpour/Copperpour/issues/317)). The rest
of the file is left as written, comment lines included. A rule that ends its
line goes with the blank space before it, so a rule
`set_custom_rule` appended goes as it came; one that shares its line with the
next goes with the spaces after it. A cut that would change how KiCad reads
the text beside the rule, such as text that would then open its line with `#`,
which KiCad reads as a comment, returns `plan_blocked` and writes nothing.

The file left is checked as `set_custom_rule` checks the file it writes: as
KiCad 10's rule parser and expression compiler read it, then, on a board with
an item, loaded by `kicad-cli pcb drc` on a copy of the board with a probe
rule. A fault in a rule left, or kicad-cli not loading the file, returns
`plan_blocked` and writes nothing; a rule whose fault lies inside it can be
deleted. On a board with no item yet the first check alone decides. A name no
rule has returns `invalid_argument`, naming the rules read and any fault that
may hide one; two rules of the name return `ambiguous_target`, their lines as
`candidates`; a missing file returns `file_not_found`; a file saved after the
read returns `conflict`.

The response gives `rules_file`, `name`, `previous`, the deleted rule's text,
`kicad_checked` and `unchecked_properties`, as `set_custom_rule` gives them.

No existing tool's arguments or responses change.

### `delete_netclass` (minor release)

A new `pcb_routing` tool deletes from the board's sibling `.kicad_pro`:
`delete_netclass(board, name | pattern, netclass?)`. With `name`, the class
goes and every `netclass_patterns` entry that named it names `Default` instead,
as KiCad's Board Setup deletes a class; `Default` itself is refused as
`invalid_argument`. With `pattern`, the entry with exactly that text goes;
`netclass` names the class whose entry goes where several classes hold the same
text; without it that is `ambiguous_target`, the classes as `candidates`. A
class delete moves no pattern into a second copy of one `Default` already
holds, as Board Setup saves it. It writes as `create_netclass` and
`assign_net_to_class` do: refused while KiCad holds the board or the project,
and only while the file holds what was read
([#109](https://github.com/Copperpour/Copperpour/issues/109)).

A class delete returns `patterns_moved_to_default` and
`label_assigned_nets`, the nets that the project's `netclass_assignments`
gives the class. Schematic netclass labels and that cache are not edited:
`run_erc` reports a label still naming a deleted class as
`undefined_netclass`.

### `set_design_rules` refuses the five older minima outside the range KiCad loads (patch release)

`set_design_rules` wrote `min_clearance`, `min_trace_width`, `min_via_drill`,
`min_via_size` and `min_hole_to_hole` without a range check, and KiCad 10
replaces a value outside the range it loads with its default when it opens the
project, so the call reported success and DRC ran with something else
([#152](https://github.com/Copperpour/Copperpour/issues/152)). They now take
the same check as the other five minima: `min_hole_to_hole` 0 to 10 mm, the
other four 0 to 25 mm. A value outside its range is refused as
`invalid_argument`, naming the field, and nothing is written; the schema
advertises each range as `minimum` and `maximum`. A caller that sent such a
value should send one inside the range.

### `run_drc` takes `skip_creepage` (minor release)

`run_drc` accepts an optional boolean `skip_creepage` (default `false`). When
`true`, kicad-cli checks the board against a temporary copy of the project's
`.kicad_dru` without its `creepage` constraints; every other rule, the
schematic parity check and the libraries are the project's, and the project
directory is never written. The response adds `creepage_skipped`, `true` only
when the constraints were actually left out. Without symbolic links (some
Windows setups), the full DRC runs and `creepage_skipped` is `false`. Calls
that leave `skip_creepage` out are unchanged.

`apply_specctra_ses` does not take `skip_creepage`: its candidate DRC does not
read the source project's custom rules at all (#312), so there is no creepage
constraint to leave out there yet.

### `edit_schematic_component` sets Exclude from BOM and Exclude from board (minor release)

`edit_schematic_component` takes two optional booleans, `in_bom` and `on_board`,
KiCad's native `(in_bom yes|no)` and `(on_board yes|no)` flags: `in_bom: false`
is Exclude from bill of materials and `on_board: false` Exclude from board
([#34](https://github.com/Copperpour/Copperpour/issues/34)). Each is written on
every placed unit, as `dnp` is, and a call that leaves one out keeps the file's
value. `unit` beside either alone is refused, as it is beside `dnp`.

The replies built from the component readback gain `in_bom` and `on_board`, at
the top level and for each unit, read from the saved file:
`edit_schematic_component`, `move_schematic_component`,
`rotate_schematic_component`, `mirror_schematic_component`,
`add_component_annotation`, `replace_component`, and the placement replies of
`add_schematic_component`, `batch_place_components`, `add_power_symbol` and
`batch_add_power_symbol`. Callers needn't change anything.

### `edit_sheet` shows and hides a sheet's name and file fields (minor release)

`edit_sheet` takes two optional booleans, `hide_name` and `hide_file`, for the
`Sheetname` and `Sheetfile` text of a sheet, the two Show checkboxes of KiCad's
Sheet Properties dialog
([#343](https://github.com/Copperpour/Copperpour/issues/343)). `true` hides
the field, `false` shows it, and a field left out stays as it is; either one
alone is enough for a call. The flag is written as KiCad 10 writes it, as
`(hide yes)` after the field's `(at …)`.

A restated visibility is an honest no-op, as for the other fields:
`requested_fields` names `hide_name` or `hide_file` and `changed_fields` leaves
it out. A value that is not a boolean is `invalid_argument`. The sheet object
in the replies of `add_hierarchical_sheet`, `edit_sheet`, `duplicate_sheet` and
`get_sheet_hierarchy` gains `hide_name` and `hide_file`, `null` for a sheet
without that field. A hidden sheet is still found by its `sheet_name`. No
existing call changes.

### `edit_symbol_pin` (minor release)

A new `library` tool sets the electrical type, graphic style, name, number,
position, angle or length of one pin of an existing `.kicad_sym` symbol:
`edit_symbol_pin(library_path, symbol_name, pin_number, unit?, body_style?,
type?, style?, name?, new_number?, x?, y?, angle?, length?)`. `type`, `style`,
`x`, `y`, `angle` and `length` are the keys `create_symbol`'s pins use, `x` and
`y` the pin's connection point in mm. Only the changed tokens are rewritten;
the pin's text effects and the rest of the library stay as written
([#113](https://github.com/Copperpour/Copperpour/issues/113)).

A pin is one drawing in one unit and body style, as KiCad writes it under
`NAME_<unit>_<body style>`; 0 is common to all, so a pin in unit 0 is in every
unit. A number found in more than one place returns `ambiguous_target`, with
the places as `candidates`, until `unit` or `body_style` chooses one. A De
Morgan symbol draws each pin in both body styles with different graphic styles,
so a call never changes both. Two pins with one number in one place cannot be
chosen and are refused. A derived symbol (`extends`) is refused with its
parent's name, since its pins are the parent's.

`new_number` is refused when another pin of the symbol stands for that number,
in any unit, unless the two are in different body styles: the pins KiCad's
symbol checker reports as duplicates, a stacked `[1-3]` number standing for
each number in it. A renumbered pin stays where it is in the file; KiCad's next
save puts it in number order. `angle` is 0, 90, 180 or 270, the directions
KiCad reads, and `length` is not negative. Coordinates and lengths are kept to
KiCad's 0.0001 mm, so a value KiCad would read as the current one is no change.
A pin that writes no `(at …)` or `(length …)`, which KiCad defaults, refuses a
change to it. KiCad keeps no space in a pin name or number and reads one as
`_`, so a space is written as `_`, and `pin_number` is read the same way. In a
library older than version 20250318 KiCad reads a pin named or numbered `~` as
empty: the tool reads it so too, and refuses `~` as a new name or number.

The response gives the changed pin's `unit` and `body_style`, and `changes`:
each changed part with its `previous` and `current` value, as
`set_symbol_properties` reports fields, `x`, `y`, `angle` and `length` as
numbers. A call that changes nothing writes nothing. The write and its readback
are `set_symbol_properties`'s, now one helper for both: a library saved after
the read returns `conflict` with nothing written.

Until now one pin could be changed only by `delete_symbol` and `create_symbol`
with every pin given again. No existing tool's arguments or responses change;
`set_symbol_properties`' refusal of a duplicated symbol name now says
"Copperpour cannot tell which one to change" instead of naming the tool.
`library` rises from 18 to 19 tools.

### `enrich_datasheets` resolves from the local catalog first and reports detailed provenance (minor release)

`enrich_datasheets` now queries the local JLCPCB SQLite catalog first (from
`jlcpcb_db_path` in server config, `db_path` in arguments, or the default user
data directory), and falls back to the live LCSC HTTP API only when a part's
local record is missing or its `Datasheet` field is empty. When all parts hit the
local catalog, no network requests are made.

The tool response includes additional structured metric fields:
- `local_hits`: number of unique LCSC IDs resolved from the local catalog;
- `api_hits`: number of unique LCSC IDs resolved via the LCSC HTTP API;
- `misses`: number of unique LCSC IDs not found in either source;
- `skipped_existing_values`: number of placed symbol instances whose non-empty
  datasheet was preserved because `overwrite_existing` was `false`;
- `attempted_sources`: list of sources queried, such as `["local_catalog"]`,
  `["local_catalog", "lcsc_api"]`, or `["lcsc_api"]`. `local_catalog` is listed
  only when a catalog was opened and read;
- `local_catalog_skipped`: present when no catalog was read because none is
  configured or present at the configured or default path;
- `local_catalog_error`: present when the configured or default catalog exists
  but could not be read; the parts were looked up through the LCSC API instead.
- `local_catalog_unreadable_ids`: present when the catalog was read but some
  parts' `Datasheet` can't be read as text (a blob, a number, or text that
  isn't valid UTF-8), which a catalog built by `download_jlcpcb_database` never
  has; those parts were looked up through the LCSC API instead. A NULL `Datasheet` counts as empty and is not listed.

A `db_path` given in the arguments is not replaced by the network: when it is
missing or cannot be read as a catalog, the call is refused as `file_not_found`
before any lookup and the schematic is left unchanged.

Previous fields (`datasheets_enriched`, `lcsc_ids_found`, `updated`, and
`schematic`) remain present and preserve their behavior.

### `estimate_cost` reports the board's size from its whole Edge.Cuts outline (patch release)

`board.width_mm` and `board.height_mm` were 0 for every board KiCad 10 saved,
because the size was read by scanning the file text for single-line `gr_line`
items, and KiCad writes one field per line. Arc, rectangle, circle and polygon
outlines were never read. The size is now the bounding box of every Edge.Cuts
outline graphic on the board, arcs by their real extent, read the way
placement reads the outline. It is rounded to KiCad's 1 nm grid.
`get_board_extents` is unchanged: it measures every item, not only the outline.

A board without an outline still reports 0 x 0, and so does one with an
outline graphic that cannot be read. Both now add a warning to `warnings`
saying the size was not read. Callers that took 0 to mean "unknown" can keep
doing so, or check `warnings`. No price changes: the PCB price uses only the
layer count.

### `estimate_cost` prices JLCPCB parts from the local catalog (minor release)

With `fab_house: "jlcpcb"`, parts whose footprint has an `LCSC` field are now
priced from the local JLCPCB catalog instead of at a flat $0.05. Each part is
priced at the catalog's price break for the order's quantity of it: its count
per board times `quantity`. Below the first break, the first break's price
applies. A part sold in steps (`10-90:…,100-590:…` sells in tens) rounds an
order between two breaks up to the next one. A catalog built before price breaks were kept, or a part whose breaks
can't be read, gives the first break's price. The feed rounds prices to 3
decimals and drops breaks below $0.01 except the first, so a cheap part has one
price. A price under $0.0005 rounds to `0.000`, and a catalog with price breaks
prices such a part at $0. In a catalog from before #63, 0 meant unknown, so
those parts still get the average. Parts without an `LCSC` field,
parts the catalog lacks, and parts it has no price for still use the $0.05
average. For each unique Extended part, the estimate adds JLCPCB's Economic
PCBA fee of $3.07 once per order.

Footprints the BOM leaves out (`exclude_from_bom` or `dnp`) are no longer
bought or placed, for JLCPCB and PCBWay: `components_estimate` and
`smt_assembly` count only the parts the BOM lists.

The response adds:
- `cost_estimate.extended_parts_fee`, which `total_estimate` and `per_board`
  include. It is `$0.00` for other fab houses.
- `cost_estimate.approximate`, true when the fab house's PCB and assembly
  prices have no cited source, as all of them have none today. `notes` then
  says so.
- `components`, which lists the parts `priced_from_catalog` and those
  `estimated` at the average, each with a `reason`. A part priced from the
  catalog gives its `order_quantity`, its `unit_price` at that quantity, and
  the `price_break` that gave it, as the feed writes it (`10-99:0.002`), or
  null when the catalog has no breaks it can read for the part. When the
  order is below the part's first break, `below_minimum_order` gives that
  break's lower end: JLCPCB sells at least that many, but the estimate prices
  only the parts the boards use. A part priced at $0 has a `note` saying its
  price rounds to $0.000. It also gives the `catalog` that
  was read or why none was, with `price_breaks` saying whether it keeps them
  and, when it doesn't, a `reason`. Then `average_unit_price`,
  `extended_unique_parts`, and the references `excluded_from_bom`. Other fab
  houses get `priced_from_catalog: []` and `excluded_from_bom`.

`components_estimate`, `smt_assembly` and the totals change for JLCPCB boards,
and for PCBWay boards with footprints the BOM leaves out. The note that named a
nonexistent `generate_bom` tool is gone. `generic` estimates are unchanged.

### `evidence_source` values, the viewer's app id and the bridge's server name use `copperpour` names (minor release)

`get_editor_state` capabilities now report `evidence_source` as
`copperpour_bundled_kicad_protocol` instead of `konnect_bundled_kicad_protocol`,
and `copperpour_saved_structure_and_typed_live_context` instead of
`konnect_saved_structure_and_typed_live_context`. The old values are no longer
returned, so a caller that matches them should accept both while it talks to
older servers.

The schematic viewer's Tauri identifier is
`com.github.copperpour.copperpour.viewer` instead of
`com.github.mixelpixx.konnect.viewer`. On Linux and Windows it names the
viewer's WebView data directory under the local data directory (`~/.local/share`,
`%LOCALAPPDATA%`). The viewer stores no settings, so nothing is lost: it starts
with a new directory, and the old one can be deleted.

The plugin's native bridge sends `Server: CopperpourNativeBridge/1` instead of
`KonnectNativeBridge/1`
([#250](https://github.com/Copperpour/Copperpour/issues/250)).

### `generate_jlcpcb_order_link` (minor release)

A new `manufacturing` tool uploads a board to JLCPCB and returns the link to
its quote page ([#208](https://github.com/Copperpour/Copperpour/issues/208)):
`generate_jlcpcb_order_link(board, schematic?, include_assembly?, skip_drc?,
quantity?, thickness?, color?, surface_finish?, copper_weight?,
jlcpcb_cpl_corrections_path?)`. It writes the JLCPCB package as
`export_manufacturing_package` does and runs DRC on the board it was exported
from. Then it posts the Gerbers and drill files, and with assembly the BOM and
CPL, to EasyEDA's order endpoint. This sends the design to a third party; it
places no order. The tool comes with the `jlcpcb` build feature, which release
builds have.

`schematic` is required when `include_assembly` is true, the default. The BOM
sends the schematic's `LCSC`, `MPN` and `Manufacturer` fields. An incomplete
package, a DRC error, or a board without an Edge.Cuts outline refuses the
upload before anything is sent. `skip_drc` skips DRC.

#### Response

`upload_outcome` is `not_sent`, `sent` or `uncertain`, and `uploaded` is
`false`, `true` or `null` with it:

- `sent` with `order_url`: JLCPCB's link to the uploaded files. It asks for a
  JLCPCB login, then opens the quote page with the files loaded. `upload_id`
  is EasyEDA's id for the upload.
- `not_sent`: a refusal before the request, or a failure to build the request
  or to connect. Nothing left the machine.
- `sent` with `order_url: null`: the endpoint received the files, but its
  answer was not a success (redirects are not followed), could not be read,
  or had no link. The first two give `http_status`. A sent result does not
  prove the upload was accepted or that a quote exists.
- `uncertain`: another transport failure, which may come after the files left
  the machine. Check the earlier outcome before uploading again; don't retry
  automatically.

A failure once the request is under way keeps `order_id` and the reason, and
a failed request adds `recovery`, which says whether retrying is safe.

### Over HTTP, progress arrives on the call's own SSE response (minor release)

Over the Streamable HTTP transport, a `tools/call` that carries
`_meta.progressToken` from a client whose `Accept` header lists
`text/event-stream` is now answered with an SSE stream of its own: the call's
`notifications/progress`, then the JSON-RPC response as the last event, then
the end of the stream. Only that client sees them.

Progress no longer goes to the `GET /mcp` streams, which every client shares:
a client could see progress for a token it never sent. Those streams keep
carrying the notifications not tied to a request, such as
`notifications/tools/list_changed`.

Every other request, including a `tools/call` without a token or from a client
that accepts only `application/json`, still gets a plain JSON response.
Callers that follow the MCP Streamable HTTP specification need no change; stdio
is unchanged.

### `copperpour init` replaces the hooks of the `konnect` alias (patch release)

The `konnect` alias beside `copperpour` runs that binary, so `init` now treats
the Claude hooks an older install wrote for it as its own and replaces them
with ones naming `copperpour`. Before, both were kept and each hook ran twice
for a client that still registers the server as `konnect`. `uninstall`
removes them as well. `status` and `get_installation_info` report a hook that
only the alias holds as `different` with the new reason `alias_handler`,
instead of `other_executable`. Hooks naming a `konnect` or `copperpour` in
another directory are left alone
([#244](https://github.com/Copperpour/Copperpour/issues/244)).

### The IPC client, temp directories and bridge thread use `copperpour` names (minor release)

Copperpour names itself `copperpour-<pid>` instead of `konnect-<pid>` in every
KiCad IPC request header, and the plugin's native bridge thread is
`copperpour-native-bridge`. The schematic viewer renders into
`<temp>/copperpour-viewer-<pid>` instead of `<temp>/konnect-viewer-<pid>`.
`get_schematic_view` writes its SVG under `<temp>/copperpour-schematic-views/`
instead of `<temp>/konnect-schematic-views/`, so the path it returns changes
too; nothing reads the old directory any more, and it can be deleted. Update
any sandbox rule, log filter or temp-dir cleanup that matches the old names
([#250](https://github.com/Copperpour/Copperpour/issues/250)).

### Coordinates and sizes sent to KiCad round to the nearest nanometre (patch release)

A tool writing to a board KiCad holds open converted millimetres to KiCad's
nanometres by truncating toward zero, so a value binary cannot hold exactly,
such as `4.1` or `128.2 - 5.9`, reached KiCad 1 nm toward zero, and KiCad kept
it ([#84](https://github.com/Copperpour/Copperpour/issues/84)). Read-back
showed `122.299999`, the position file and Gerbers carried the same
nanometre, and an `R_0805_2012Metric` pad of 1.025 mm arrived as 1.024999,
which DRC reports as `lib_footprint_mismatch`
([#238](https://github.com/Copperpour/Copperpour/issues/238)). The conversion
now rounds to the nearest nanometre, a half-way value away from zero. Anchors,
pads, tracks, vias, zones, graphics and text from `place_component`,
`move_component`, `set_component_placements` and every other live board write
move by at most 1 nm and now land on the value asked for; values that were
exact are unchanged. File writes, schematic grid snapping and JSON responses
are not affected. A caller that compensated for the lost nanometre should
stop.

### JLCPCB catalog tools report unknown prices and stock as `null` (minor release)

A price or stock that the JLCPCB feed doesn't give, or gives as something other
than a number, used to be stored and reported as `0`. A caller couldn't tell
"unknown" from free or out of stock. Unknown values now survive the import:

- `download_jlcpcb_database` stores an empty or non-numeric price or stock as
  NULL. It also keeps the feed's price breaks in a new `Price_Breaks` column,
  and marks the catalog as schema version 2.
- In `search_jlcpcb_parts`, `get_jlcpcb_part` and `suggest_jlcpcb_alternatives`
  results, `price` and `stock` are now a number or `null`. So are `mpn`,
  `package`, `manufacturer`, `library_type` and `description`, which are `null`
  when the catalog holds no value. A text price that holds a number is
  reported as that number.
- With `in_stock: true` (the default), `search_jlcpcb_parts` still leaves out
  parts whose stock is unknown, and reports how many in the new
  `unknown_stock_excluded` field.
- `suggest_jlcpcb_alternatives` excludes a part with unknown stock and counts it
  in the new `exclusions.unknown_stock_count`. It used to count such a part in
  `below_min_stock_count`. A part with an unknown price ranks after the parts
  with a known price, as before.
- A catalog row with no LCSC number is now an error instead of a missing result.
- `get_jlcpcb_database_stats` adds `schema_version`. For a version 2 catalog it
  also adds `unknown_price_count` and `unknown_stock_count`.

A catalog built before this change stores unknown values as 0, which can't be
told apart from a real 0. Its zeros are reported as they are. The catalog tools
add a `catalog_note` asking for a rebuild with
`download_jlcpcb_database` and `force: true`, and they don't report
`unknown_stock_excluded`.

Callers that read `price` or `stock` as a number must accept `null`.

### JLCPCB CPL rotations: rules backed by observations, unverified families warned (minor release)

`export_manufacturing_package` with `fab_house: "jlcpcb"` changes the CPL
rotation of one footprint, and reports more:

- The built-in policy becomes `copperpour-jlcpcb-cpl-v2`. Its
  `USB_C_Receptacle_HRO_TYPE-C-31-M-12` rule (+180) is withdrawn: it turned a
  correctly rotated connector upside down. The footprint is now unverified: it
  keeps KiCad's rotation and is listed in `warnings`. The SOIC-8 rule is
  unchanged.
- `warnings` now lists unverified matches and the polarized or multi-pin
  footprints no rule covers (all but resistors, capacitors, inductors, fuses,
  fiducials, test points and mounting holes; resistor arrays are reviewed); the
  package is not marked incomplete for them, but the summary no longer says
  "No warnings".

The `jlcpcb_cpl_corrections` policy file gains optional `footprint_exact`,
`status: "unverified"`, `evidence` (with `lcsc` and `side`),
`rotation_insensitive_prefixes`, `rotation_sensitive_prefixes` and `note`. A
rule whose `rotation_degrees` disagrees with its own `evidence` is refused, and
so is a bottom-side observation. Existing v1 files stay valid.

### `kicad_cli_timeout_seconds` sets the long kicad-cli timeout; a timed-out kicad-cli is killed (minor release)

The timeout of long kicad-cli operations (DRC, ERC, exports, upgrades) was a
fixed 600 s. The new config key `kicad_cli_timeout_seconds` sets it (default
600, so nothing changes without it; 0 is read as 1). A call that times out now
kills kicad-cli instead of leaving it running, then as a zombie.

### Catalog tools report a missing or unreadable JLCPCB catalog alike (minor release)

The tools that read the local JLCPCB catalog now find it the same way (the
tool's `db_path` or `output_path` argument where it has one, then the
`jlcpcb_db_path` setting, then the default path) and
report a missing or unreadable one in the same words
([#209](https://github.com/Copperpour/Copperpour/issues/209)):

- no path: `no catalog path: jlcpcb_db_path is not set and the home directory is unknown`;
- no file: `no catalog at <path>; run download_jlcpcb_database to build it`;
- unreadable: `cannot read the catalog at <path>: <error>`.

Where a tool reports the catalog as an object, it is `{ read, path, reason }`:
`path` when there is one, `reason` when the catalog wasn't read.

- `add_schematic_component` and each `placed` entry of
  `batch_place_components` add `lcsc_catalog` when the symbol has an LCSC
  number: the catalog report, plus `part_found` when the catalog was read.
  Before, a missing or unreadable catalog left `MPN`, `Manufacturer`,
  `Datasheet` and `Description` unfilled without saying so. Placement still
  succeeds without the catalog. Their `lcsc_id` description now names the
  `jlcpcb_db_path` setting instead of the old `~/.konnect/jlcpcb.db` path.
- `import_jlcpcb_part` adds `lcsc_catalog` when EasyEDA has no datasheet and
  the catalog's is used instead.
- `get_datasheet_url` adds `local_catalog_error` when the catalog can't be
  read, and its "not found" `note` gives the reason the catalog wasn't read.
  Before, an unreadable catalog was passed over as if it had missed. A part
  whose `Datasheet` is NULL is now a miss, where the lookup used to fail on it,
  and the catalog is opened read-only. The LCSC API's URL is read as
  `enrich_datasheets` reads it: trimmed, with an empty URL or `~` taken as no
  datasheet, where `get_datasheet_url` used to return them as given.
- `enrich_datasheets` keeps its fields. `local_catalog_error`, the skipped
  reason and the refusals of an explicit `db_path` use the wording above.
- `estimate_cost`'s `components.catalog.reason` uses the wording above.
- `search_jlcpcb_parts`, `get_jlcpcb_part`, `list_jlcpcb_categories` and
  `suggest_jlcpcb_alternatives` refuse a missing catalog with the wording above,
  in place of `JLCPCB database not found. Run download_jlcpcb_database first.`,
  and with no catalog path in place of `Cannot locate the JLCPCB database …`.
  An unreadable catalog is still a `handler_error` for them and for
  `get_jlcpcb_database_stats`, now with the wording above. The stats still
  report `exists: false` for a missing catalog. All five open the catalog
  read-only; only `list_jlcpcb_categories` did before.

### `update_footprints_from_library` no longer replans `graphics` after an apply for a polygon that repeats its first point (patch release)

892 of KiCad 10.0.8's stock footprints have a polygon whose last point
repeats its first. KiCad keeps that point when it loads a board from its
file, but drops it when refresh sends the footprint over the API. So after
an apply, every later dry run planned `graphics` for that footprint again
([#320](https://github.com/Copperpour/Copperpour/issues/320)). Refresh now
ignores a closed outline's repeated last point when it compares, and the dry
run after an apply plans no change. What refresh sends KiCad is unchanged.

### `update_footprints_from_library` stops planning pad changes for corner rounding (patch release)

A dry run planned `pads` for any footprint with a circular or rectangular
pad, though KiCad's DRC reports no `lib_footprint_mismatch` for it
([#254](https://github.com/Copperpour/Copperpour/issues/254)). KiCad
reports its default corner rounding, 0.25, on every pad, where the library
build leaves 0. The comparison now ignores corner rounding on shapes other
than rounded and chamfered rectangles, the only ones that use it, so such
footprints plan no change: a board whose footprints match their libraries
dry-runs to `noop` and an apply leaves them alone. Real differences are
still planned: a footprint whose board copy lacks the library's
attributes, properties or text settings still reports `attributes`,
`metadata` or `graphics`.

Graphics still compare exactly, which is stricter than DRC: KiCad's
library-parity check accepts graphic points within 10 nm. A board copy
whose points drifted by a nanometre or a few still plans a `graphics`
change that DRC does not flag, and the update corrects the drift. A caller
that discounted planned `pads` changes as noise no longer needs to.

### `update_footprints_from_library` refreshes footprints with pad fabrication properties or a reference point on KiCad 10.0.7+ (minor release)

Typed library refresh refused a library footprint whose pads have a
fabrication property, `(property pad_prop_…)`, or that has a reference
point, `(point …)`, as `unsupported_library_footprint`
([#283](https://github.com/Copperpour/Copperpour/issues/283)). 1,364 of
KiCad 10.0.8's stock footprints have pad properties (heatsink, BGA,
mechanical, fiducial) and 617 have a reference point, among them the DIP
packages.

On KiCad 10.0.7 and later, refresh now reads both:

- Each pad's property is sent as the pad's fabrication property, as KiCad
  loads it from the file. A pad whose property differs from its library
  pad plans `pads`.
- Each point is sent as a child of the footprint, placed, turned and
  mirrored to the back as KiCad places it. A point that differs from the
  library's, or that one side has and the other doesn't, plans `graphics`.

KiCad before 10.0.7 can't take either over its API, so there refresh still
refuses these footprints as `unsupported_library_footprint`, with a message
that names KiCad 10.0.7. Footprints with neither clause refresh as before on
every version.

Many of these footprints also set a pad's `zone_connect`, which refresh
reads as well since
[#300](https://github.com/Copperpour/Copperpour/issues/300). A footprint with
a clause refresh still refuses is still refused, and the message names that
clause.

### `update_footprints_from_library` refreshes footprints with pad zone connections or thermal relief settings (minor release)

Typed library refresh refused a library footprint whose pads set a zone
connection, `(zone_connect N)`, or thermal relief settings,
`(thermal_bridge_angle …)`, `(thermal_bridge_width …)` or `(thermal_gap …)`,
as `unsupported_library_footprint`
([#300](https://github.com/Copperpour/Copperpour/issues/300)). 1,141 of KiCad
10.0.8's stock footprints set a pad zone connection and 137 a pad spoke
angle, among them the exposed-pad packages and the plated mounting holes.

Refresh now reads them on every KiCad 10 version:

- Each pad is sent with its zone connection and thermal settings, as KiCad
  loads them from the file. A pad without a connection inherits one, and a
  pad without a spoke angle gets KiCad's default for its shape: 45° for a
  round pad, 90° for the others.
- A pad whose connection or thermal settings differ from its library pad
  plans `pads`. That includes a placed pad that inherits its connection but
  whose spoke angle, spoke width or gap was changed on the board. Refresh
  used to leave such a pad's settings out of the comparison; it now restores
  the library's.

A footprint that also has a pad fabrication property or a reference point
still needs KiCad 10.0.7 for those, as before.

A footprint-level `(zone_connect …)` is still refused, and so is a footprint
with any other clause refresh doesn't read, with the clause named.

### `list_jlcpcb_categories` lists the values `search_jlcpcb_parts` accepts as `category` (minor release)

New tool in the `integration` toolset. Without arguments it lists the
top-level category groups of the local JLCPCB database, each with
`part_count`, `in_stock_count` and `subcategory_count`. With `contains`, it
lists every full category name (`Group / Subcategory`) containing that text,
case-insensitive, with `part_count` and `in_stock_count`.

`search_jlcpcb_parts`' `category` filter now matches `%` and `_` literally,
as the query words already did; it was a substring match before and remains
one. `get_jlcpcb_database_stats` adds `category_count`.

### Live PCB writes with no PCB editor open say so (patch release)

With KiCad running and no PCB editor open, live PCB writes such as
`set_board_title_block`, `add_board_text`, `set_component_placements` and
`update_footprints_from_library` still refuse and leave the board file alone.
The refusal no longer says KiCad "may hold this board open". It now says that
KiCad reports no PCB editor at the endpoint, and asks the caller to open the
board in the PCB Editor and retry. The result is still an error of the same
shape (`update_footprints_from_library` keeps its `conflict` diagnostic).
Callers matching the old sentence should match the new one.

### a move or a wire delete judges a junction dot by its exits (patch release)

The tools that re-judge junction dots where pins moved or went away
(`move_schematic_component`, `rotate_schematic_component`,
`mirror_schematic_component`, `move_region`, `replace_component`,
`delete_schematic_component`, `bulk_move_schematic_components`,
`batch_place_components`, `batch_delete` and
`batch_delete_schematic_components`) and the wire deletes
(`delete_schematic_wire`, `batch_delete_schematic_wire`) used to keep every dot
with two or more wires under it, and the first added one only where a pin
landed mid-span on exactly one wire. A pin moved onto a wire corner got no dot,
and a T whose branch was deleted kept its dot at the corner left.

Now each dot at a point they touch is judged by the count of exits the wire
tools write dots by (see *wire tools dot a T built from wire ends*)
([#136](https://github.com/Copperpour/Copperpour/issues/136)). A dot is kept at
three or more exits and goes below, and a pin that makes three gets one. So a
pin landing on a corner gets a dot, and a dot at a plain corner, at two wire
ends meeting in line, or at one wire ending on a pin goes. Where a wire passes
mid-span and another wire is at the point, as at a crossing, a collinear
overlap or a T whose wire was not split, only a dot (or a label over them)
joins them, so none is added there and none removed. A pin under a no-connect
gets no dot, and bus points are left alone. When a sheet's pins cannot be
resolved, a dot with a wire under it is kept. Schemas and response shapes are
unchanged. A caller that added a dot after such a move, or removed one after
such a delete, can drop that step.

### `connect_to_net` stubs refuse to join another net (patch release)

`connect_to_net`, and `batch_connect_to_net` with `stub_length` above 0, drew
the stub with no check of what it met. A stub ending on another pin, or
running over one, dotted it, so that pin joined the requested net and the call
still reported success
([#117](https://github.com/Copperpour/Copperpour/issues/117)).

A stub is now checked before it is written. It may join only what a label
named `net` on the pin would: the pin's own net, and every item already on
`net`. In `batch_connect_to_net` that includes every pin the call names.

- With `direction: "auto"`, a stub that would join another net turns across
  the pin instead, to either side, and never runs back over the symbol. A pin
  given by coordinates whose outward direction is unknown tries all four. The
  response's `direction` names the one drawn. A stub turned up or down keeps
  the outward label rotation, so its text still runs away from the symbol.
- When the named direction, or every `auto` one, would join another net, or a
  placed symbol's pins cannot be resolved so the check cannot see them,
  `connect_to_net` refuses with `plan_blocked` and writes nothing. `reasons`
  holds one entry per direction tried.
- `batch_connect_to_net` skips that pin, adds
  `"<reference>.<pin_number>: <reasons joined by '; '>"` to `errors`, and still
  writes the other pins.
- `batch_connect_to_net` now sets `isError` when it connected no pin and
  `errors` is not empty, as `batch_connect_pins` does. That also covers a call
  whose every pin failed to resolve, which used to report success.

A zero-length stub, a label on the pin, is unchanged.

### The operating-rules skill is now `copperpour` (minor release)

The bundled skill that holds Copperpour's operating rules installs as
`copperpour` instead of `konnect`: `~/.claude/skills/copperpour/` (Codex:
`~/.agents/skills/copperpour/`), frontmatter `name: copperpour`, and
`copperpour/SKILL.md` in `status` and `get_installation_info`. The bundled
agents preload it under the new name. `init` removes the `SKILL.md` and
`references/reliability-contract.md` it wrote under `konnect/`, and the
directory once it is empty; files you added there are kept, and a `konnect`
or `konnect/references` that is a link is not followed. `uninstall` does the
same. `copperpour skill konnect` still prints the rules. Update your own
skills or agents that preload `konnect`
([#239](https://github.com/Copperpour/Copperpour/issues/239)).

### Placements refuse to land a pad on another net's copper (minor release)

`set_component_placements`, `move_component` and `rotate_component` now check
a live board before they move anything
([#96](https://github.com/Copperpour/Copperpour/issues/96)). A pad dropped on
another net's via makes KiCad re-net the via to the pad's net, silently, and
DRC cannot see the change afterwards; a pad dropped on a track shorts it. For
each moved footprint the tools ask KiCad for its pads' copper outlines
(`GetPadShapeAsPolygon`) on every copper layer the pad has copper on
(`CheckPadstackPresenceOnLayers`), move them to the planned pose, and test them
against every via, track and arc of another net on that layer. Only overlap
counts: touching is a conflict, a near miss is not. Clearance, zones, and the
pads of other footprints, including two footprints of one batch, are not
checked. Copper with no net is never the same net as anything.

A move that would put a pad on such copper is now refused as `plan_blocked`
before any change: no commit is opened and nothing moves, and
`set_component_placements` refuses the whole batch. `error.reasons` names each
conflict in words, and `copper_check.conflicts` lists each as `reference`,
`pad`, `pad_net`, `layer`, `item_kind` (`via`, `track` or `arc`), `item_uuid`
and `item_net`. A check that cannot complete, for example because KiCad gives
no outline for a pad, refuses the call too, and nothing moves. A caller that
puts a part on such copper on purpose, to remove the copper next, passes
`allow_copper_conflicts: true`.

All three tools take two new optional booleans, both default `false`:

- `allow_copper_conflicts: true` applies the move anyway and still returns the
  conflicts, with a `warning`.
- `dry_run: true` moves nothing and returns the check, `dry_run: true`,
  `source`, and `count` and `placements`, each footprint's `reference` with its
  `current` and `target` pose (`x`, `y`, `rotation`).

Each success response of the three tools, and the `plan_blocked` refusal, now
carries `copper_check`. On a live board it holds `status` (`clear` or
`conflicts`), `conflict_count`, `conflicts`, and `coverage`: `pad_count`, the
moved pads tested; `layers`, the copper layers they were tested on; and
`via_count`, `track_count` and `arc_count`, the copper read from KiCad. The
other response fields are unchanged.

Holes are not checked. Only a pad's copper is tested and an NPTH pad has none,
so a footprint whose only pads are NPTH, such as `MountingHole_3.2mm_M3`,
reports `status: "clear"` with `pad_count: 0` and `layers: []`: a mounting
hole moved onto a track reads as clear. Read `clear` with a `pad_count` of 0
as nothing tested.

The closed-file path cannot run the check, because it needs KiCad's own pad
shapes. With no live KiCad the tools still edit the saved board as before, and
report `copper_check` as `{"status": "not_run", "reason": "no_live_board",
"detail": …}`: read that as no check, never as a pass. A `dry_run` there reads
the poses from the file and writes nothing. Open the board in KiCad to have
placements checked.

### Placement refusals Copperpour makes itself say so, with an error kind (patch release)

On a live board, `set_component_placements`, `move_component`,
`rotate_component` and `align_components` refuse some requests themselves,
on what they read from KiCad, before KiCad keeps any change
([#240](https://github.com/Copperpour/Copperpour/issues/240)):

- a rotation that is not a multiple of 90° of a footprint holding a
  rounded rectangle or a textbox;
- a footprint holding an item Copperpour cannot carry along, such as a
  barcode (`kiapi.board.types.Barcode`) or a table
  (`kiapi.board.types.Table`);
- a reference no footprint on the board carries, or, in a
  `set_component_placements` batch, one two footprints carry;
- a copper check that could not complete.

These refusals were plain text that blamed KiCad: "KiCAD rejected the …
over IPC: … KiCAD is reachable and may hold this board open …", and for
`align_components` "IPC align error: KiCad batch failed; changes dropped: …".
They now read `The {what} was not written: {reason}. The board file was not
modified.`, where `{what}` is `move`, `rotation`, `component placement
batch` or `alignment`, and carry an `error`:

| Refusal | `error` |
|---|---|
| A rotation that is not a multiple of 90° | `invalid_argument`, `field` `rotation` or `placements[i].rotation` |
| A reference missing, or held twice | `invalid_argument`, `field` `reference`, `placements[i].reference` or `references[i]` |
| An item Copperpour cannot carry | `unsupported_capability`, `capability` the tool's name, `kicad_version` null |
| A copper check that could not complete | `plan_blocked`, `operation` the tool's name, `reasons` holding the refusal |

An `invalid_argument`'s `reason` is the refusal's text. Nothing was changed
on the board: the refusal comes before any change is sent, or inside a
commit that is then dropped. A refusal KiCad itself returns keeps the
"KiCAD rejected …" wording and has no kind. A caller that matched the old
sentences should match `error.kind` instead.

### The project-file writers refuse a `.kicad_pro` changed after their read (patch release)

`set_design_rules`, `set_predefined_sizes`, `create_netclass` and
`assign_net_to_class` now replace the board's `.kicad_pro` only while it still
holds the text they read. Until now each replaced it unconditionally, so a
project file that KiCad, another server or a concurrent HTTP call rewrote in
between was put back to the older read, and that change was lost while both
calls reported success
([#153](https://github.com/Copperpour/Copperpour/issues/153)).

A file changed before the write now returns `conflict`, with the project file
in `paths`, and is left as the other writer left it. Retry the call to apply
the change to the file as it is now. A file that no longer holds the change
when read back after the write returns `mutation_outcome_uncertain`; read the
project file before retrying. Arguments and successful responses do not change.

The four writers also ask for KiCad's project lock again at the write, so a
KiCad program that opened the project after the first check is refused with
`unsafe_file_fallback`, as before the read. Board file fallbacks and the
symbol-library writers now share the same revision-checked write: their
`conflict` message reads "The board '…' changed after Copperpour read it, …
Reload it and retry.", and a symbol library's `mutation_outcome_uncertain`
`reason` names "the symbol library" instead of "the library". Kinds and fields
are unchanged.

### Footprints with a reference point can be moved and rotated on KiCad 10.0.7+ (patch release)

KiCad 10.0.7 started sending a footprint's reference point, its `(point …)`,
as a child of the footprint (`kiapi.board.types.ReferencePoint`). 617 of
KiCad 10.0.8's stock footprints have one, among them the DIP packages. On
KiCad 10.0.7 and later, `move_component`, `rotate_component`,
`set_component_placements` and `align_components` refused every move of such
a footprint, as an item Copperpour could not carry along
(`unsupported_capability`, [#270](https://github.com/Copperpour/Copperpour/issues/270)).

They now move and turn the point with the footprint, about its anchor, as
KiCad does. Nothing else changes. Footprints holding other items Copperpour
cannot carry, such as a barcode or a table, are still refused the same way.

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

### `renumber_sheet_pages` numbers each placement of a reused sheet file (minor release)

`renumber_sheet_pages` failed with `duplicate transaction target` on a
hierarchy that places a sheet file holding sheets more than once and changes
the pages of those sheets, since it prepared a write of that file at each
placement ([#351](https://github.com/Copperpour/Copperpour/issues/351)). It
now writes each file once, with a page record for every
placement of each sheet in it, as eeschema keeps them; the walk order and the
numbering are unchanged. Each entry of `pages` carries `path`, the sheet
instance's own hierarchy path (its record's path plus its uuid), since one
`sheet_name` and `file` now appear once per placement.

### `repair_instance_metadata` (minor release)

A new `sch_hierarchy` tool repairs the placed-symbol instance records that make
every write to a sheet fail as `stale_target`
([#128](https://github.com/Copperpour/Copperpour/issues/128)):
`repair_instance_metadata(schematic, dry_run?, expected_plan_revision?)`;
`sch_hierarchy`'s description now names it. It drops a path of the owning
project that its sheet tree does not give the file, a repeat of a path a symbol
already records, and every record under a project name that no candidate
`.kicad_pro` defines; the candidates are the project files in the sheet's
directory and those above it, as for ownership. It adds each path the owning
project gives the file and a symbol does not record, with the reference and
unit of a dropped record at that path, as a renamed project's records have,
else with the symbol's Reference and unit. A record of another project that a
candidate defines, or one with no project or path, is kept, and the repair is
refused. A project in another directory that places the sheet through `../` is
not a candidate, so its records are dropped; the dry run lists them first.

`dry_run` defaults to true and returns `status` (`ready`, `partial`, `noop` or
`blocked`), `project`, `instance_paths`, `plan_revision`, the `drop`, `add`
and `keep` records (each `project`, `path`, `symbols`, and for `drop` and `keep`
a `reason`), `stale_after`, `applied: false` and `source: "file"`.
`stale_after` says why the sheet would still be refused once repaired, or is
null; with it, a plan that changes the sheet is `partial`, one that does not is
`noop`. Apply requires `expected_plan_revision` and returns the same keys with
`status: "applied"` and `applied: true`, or `noop` when there is nothing to
write. It returns `plan_blocked`, naming each kept record, for a plan the dry
run calls `blocked`; `stale_target` when the sheet or its project changed since
that dry run, or when the text it prepared would change more than instance
records; and `conflict` while KiCad holds the sheet or its project's root
schematic open, or when the sheet changed after it was read. A file that is not
the text written is `mutation_outcome_uncertain`. A missing sheet is
`file_not_found`; a sheet with no `(uuid …)`, or of a project that lists another
top-level sheet or gives its root another uuid, `invalid_argument`; one whose
project cannot be told, `conflict`. No existing tool's arguments or responses
change.

### `replace_board_outline` (minor release)

A new `pcb_board` tool replaces the board outline with closed contours of
lines and arcs ([#47](https://github.com/Copperpour/Copperpour/issues/47)):
`replace_board_outline(board, contours, dry_run?, expected_plan_revision?)`.
Each contour is an ordered list of `{type: "line", start, end}` and
`{type: "arc", start, mid, end}`, points as `{x, y}` in mm, the line and arc
`set_footprint_graphics` takes. Each primitive ends where the next starts, and
the last where the first starts. A contour inside an outline is a cut-out, and
one inside a cut-out is another outline, as KiCad reads them.

`dry_run` defaults to true: it checks the contours, lists under `replaced` the
board-owned Edge.Cuts shapes it would delete, and returns `plan_revision`. An
apply takes `dry_run: false` and that revision as `expected_plan_revision`. If
the board's Edge.Cuts shapes, the contours or the source have changed since,
the apply is refused as `stale_target`, as `delete_zone` refuses a stale
revision. Edge.Cuts shapes that footprints own are left alone and counted in
`footprint_edge_cuts_count`; the checks below do not include them. Text and
dimensions on Edge.Cuts are not shapes and stay.

Before writing, it refuses with `invalid_argument`, naming the primitive as
`contours[i][j]`: a joint whose two points differ at all; a line of 0.001 mm or
less, or an arc of 0.001 mm or less measured start to mid to end, as KiCad
measures them; an arc whose three points lie on a line, or one so nearly
straight that its centre lies beyond the ±2147.483637 mm KiCad keeps, as does
any coordinate beyond it; and primitives that cross or touch anywhere but at
their shared joint. Arcs are checked as chords within 0.001 mm of them.

On a board KiCad holds, the delete and the draw are one commit and one undo
step; otherwise the saved file is edited. Either way the outline is read back,
and a read-back that differs returns `mutation_outcome_uncertain`. KiCad may
store an arc reversed, with its own midpoint as `mid`, so the read-back
compares arcs by their end points and circle. The response gives each
contour's `role`, the line and arc counts, the box as `get_board_extents` names
it (`x_min` to `height`), `replaced`, `drawn` and `source`.

No existing tool changes. `pcb_board` rises from 17 to 18 tools.

### `replace_component` embeds a symbol once per sheet (patch release)

`replace_component` looked for the new symbol's embedded copy by the text
`(symbol "<lib_id>"`, but the copy it and `update_symbols_from_library` write
has the name on the line after `(symbol`. So the second part replaced by one
symbol embedded another copy in `lib_symbols`
([#315](https://github.com/Copperpour/Copperpour/issues/315)). It now finds
the copy among `lib_symbols`' children by name, as
`update_symbols_from_library` does, wherever the name sits. A sheet that
already holds two copies keeps them until KiCad saves it, which keeps one.
Arguments and response are unchanged.

### Footprints with square-cornered rectangles rotate off-axis over IPC (patch release)

`rotate_component` and `set_component_placements` refused any non-90° rotation
of a footprint holding a rectangle graphic, because KiCad sends every
rectangle with a `corner_radius`, 0 for square corners
([#224](https://github.com/Copperpour/Copperpour/issues/224)). Such a
rectangle now rotates into a polygon, as KiCad's own rotation does, so stock
footprints such as `C_0805_2012Metric` rotate to 45°. Only a rectangle with a
positive corner radius is still refused, and the refusal now names the
footprint and the graphic's layer, for example
`cannot rotate footprint 'Y1' by a non-90° angle: its rounded-rectangle graphic
on F.Fab has no off-axis form`. The textbox and unsupported-item refusals of
the same tools, and of `move_component` and `align_components`, name the
footprint too. A caller that matched the old message text should match the
new one, or rotate that footprint by a multiple of 90°.

### `route_specctra_dsn` keeps Freerouting's JVM crash reports out of the working directory (patch release)

The Freerouting JVM inherits Copperpour's working directory, usually the KiCad
project, so a HotSpot crash wrote `hs_err_pid<pid>.log` there, and could dump
core there. The JVM now starts with `-XX:ErrorFile` pointing at the
directory that already holds Freerouting's logs (`$XDG_STATE_HOME/freerouting`,
else `~/.local/state/freerouting`, created if missing), or at the temporary
directory when that one is relative or cannot be created, and with
`-XX:-CreateCoredumpOnCrash`. Look for crash reports there; nothing changes in
the tool's arguments or responses.

### `route_specctra_dsn` reads Freerouting's log incrementally (minor release)

Progress and `unrouted_connections` come from Freerouting's own log. Each poll
used to read only the last 256 KB of it, while one optimizer pass can write
over a megabyte of DEBUG lines between two lines that say where the job
stands. On a long optimizer run, progress froze on an old pass, and
`unrouted_connections` came back `null` on a fully routed board, so the call
could not tell a complete board from an incomplete one.

Each poll now reads what the log gained since the previous one, keeps a line
cut by a read until it is complete, and remembers the latest activity and
unrouted count. The first read still starts 256 KB from the end; a log that
becomes shorter than what was read is read again from its tail. Callers need
no change: on long runs, progress and `unrouted_connections` now follow the
job to its end.

### `route_specctra_dsn` survives unanswered job polls (minor release)

A JVM short of CPU, or in a long garbage collection, can stop answering for
more than 30 seconds in the middle of a long route. The first job poll left
unanswered used to fail the call with `Freerouting MCP request 'tools/call'
timed out`, and the routing done so far was lost. Up to 10 polls in a row may
now go unanswered: each one is reported as progress, `Freerouting not
answering, 3/10 polls missed in a row (12m05s)`, and the job is polled again.
The next answer resets the count; the 11th unanswered poll in a row fails the
call as before. The overall timeout still bounds the whole run.

The late answer to a poll that was given up is skipped instead of failing the
call as an unexpected response id. Callers need no change.

### `route_specctra_dsn` refuses negative `router_settings` numbers, before Java starts (minor release)

Freerouting takes a negative cost, count or distance without a warning and
routes with it, and none of its settings has a meaning below zero. A negative
number in `router_settings` was sent as given. The call now fails with
`invalid_argument` before the JAR is looked up, so no Java process starts and
no SES is written. `field` is the path of the first negative value, such as
`router_settings.scoring.via_costs`, and the message names every one, such as
`router_settings.scoring.via_costs is -5`.

Only the values that would be sent are checked: a `router_settings` value that
a named argument replaces (`max_passes` over `autorouter.max_passes`, for
example) is not sent and is not refused. Remove the sign, or the setting, and
call again.

### `route_specctra_dsn` accepts `optimize_seconds` (minor release)

New optional argument `optimize_seconds` (0 to 86400). Once routing is complete
and Freerouting's optimizer has started (`optimizer_enabled`), the call lets it
run that many seconds, then keeps the best board found so far, stops the job and
writes that board to `ses_output_path`. The result then has `final_state`
`OPTIMIZATION_STOPPED`. The board is the best one so far: Freerouting
republishes it at the start and end of every optimizer pass, and works
candidates on copies. It is complete only if the router finished: the result's
`last_activity` gives its unrouted count.

`overall_timeout_seconds` (900 by default) still bounds the whole call, and a
call that reaches it keeps no board. Raise it above the routing time plus
`optimize_seconds`.

The optimizer is seen in Freerouting's log, read as for progress, or in the
job's `stage`. When neither shows it, the optimizer runs to its end as before.

Until now the only bounds were `overall_timeout_seconds`, which kills the job
and keeps nothing, and `job_timeout_seconds`, whose `TIMED_OUT` job has no
output in Freerouting. Its description now says so. Calls without
`optimize_seconds` behave as before.

### `route_specctra_dsn` sizes its default overall timeout for `optimize_seconds`, and refuses one that leaves no room for it (minor release)

The optimizer window opens only once routing is done, but the overall timeout
defaulted to a flat 900 s: with `optimize_seconds` above that, the call was
killed while the optimizer was still inside its window, and no board was kept.
Without `overall_timeout_seconds`, the default is now 900 plus
`optimize_seconds`, at most 86400. A call without `optimize_seconds` keeps
900.

A call that gives `optimize_seconds` with an `overall_timeout_seconds` that
does not exceed it now fails with `invalid_argument` and `field`
`overall_timeout_seconds` before the JAR is looked up, so no Java process
starts and no SES is written. Raise `overall_timeout_seconds` above the
routing time plus `optimize_seconds`, or drop it to take the default.

The largest `optimize_seconds` is now 86399, in the schema and in the
tool's own check: `overall_timeout_seconds` is at most 86400 and must exceed
it, so a window of 86400 could never run. A call with `optimize_seconds`
86400 now fails at the schema with `invalid_argument` and `field`
`optimize_seconds`: lower it to 86399 at most.

The `overall_timeout_seconds` schema no longer carries `"default": 900`; its
description states the default.

### `route_specctra_dsn` takes `plane_nets` and `plane_as_obstacle` (minor release)

Two new optional arguments, passed to Freerouting's job settings under its own
names:

- `plane_nets` (array of net names): nets Freerouting treats as power-plane
  nets, with plane routing mode and cheaper vias to the plane. A `(plane ...)`
  declared in the DSN already marks its net the same way, so this matters for a
  DSN whose planes are not declared.
- `plane_as_obstacle` (boolean): whether copper pours block foreign traces
  (Freerouting's default is false).

Calls without these arguments behave as before: nothing is sent for them.

### `route_specctra_dsn` reports progress and returns `last_activity` (minor release)

A `tools/call` that carries `_meta.progressToken` (a string or a number) now
receives MCP `notifications/progress` while `route_specctra_dsn` runs: the
routing pass with its unrouted connections and violations, then each optimizer
pass with its score, read from the job's own lines in Freerouting's log. An
unchanged job still reports once a minute. All of a call's notifications are
written before its response. Calls without a token get no notifications.

The result gains `last_activity`, the last state read from the log (for example
`"pass 17: 0 unrouted, 218 violations"`), or `null` when the log could not be
read. Callers need no change.

The log is `$FREEROUTING_LOG` when set, else
`$XDG_STATE_HOME/freerouting/logs/freerouting.log` (by default under
`~/.local/state`), where Freerouting writes it on Linux. Elsewhere, set
`FREEROUTING_LOG` to the file Freerouting writes; without it, progress carries
only the job state and pass number, and `last_activity` is `null`.

### `route_specctra_dsn` sends the KiCad project's edge and hole rules, and takes `project_path` (minor release)

Freerouting does not find the board edge clearance or the hole rules in a
Specctra DSN: KiCad leaves the first out of it, and the hole rules have no
Specctra form. Through its job API Freerouting then routed with none at all,
so traces could run up to the board edge and drilled holes.

`route_specctra_dsn` now reads the KiCad project and sends three of its design
rules, in micrometres:

| `.kicad_pro` rule (mm)        | Freerouting setting           |
|-------------------------------|-------------------------------|
| `min_copper_edge_clearance`   | `copper_to_edge_clearance_um` |
| `min_hole_to_hole`            | `hole_to_hole_um`             |
| `min_hole_clearance`          | `hole_clearance_um`           |

- The project is the new optional `project_path` argument, else the only
  `.kicad_pro` next to the DSN, else the only one in its parent folder. With
  several in a folder none is guessed and no project rule is sent.
- A rule at 0 or absent is not sent: Freerouting keeps its own default.
- `router_settings` and the named arguments win over a project rule. What the
  caller asked for is still refused when Freerouting does not apply it.
- A project rule that Freerouting does not apply (Freerouting 2.5.0 has no
  `hole_to_hole_um`, and a JAR without `update_job_settings` takes none) does
  not refuse the run: it is listed in the answer.
- A `project_path` that is not a file is refused with `file_not_found`; one that
  is not a `.kicad_pro`, or a named or found project that is not readable
  JSON, with `invalid_argument` on `project_path`. Nothing is routed.

The answer has a new `project_rules` object: `project_path` (the project read,
or null), `sent` (the settings taken from it), `not_applied` (as
`"key: reason"`), and `note` (why no project was used, or null).

A DSN routed next to a `.kicad_pro` can come out different from before, with
more clearance at the board edge and around holes. To use other values, set
these settings in `router_settings`: they win over the project's.

### `route_specctra_dsn` takes `router_settings` and reports `settings_not_applied` (minor release)

New optional argument `router_settings` (object): any of Freerouting's router
settings, in the JSON shape its `get_effective_settings` returns (for example
`{"scoring": {"via_costs": 80}, "hole_clearance_um": 200}`), passed through as
given. The named arguments (`max_passes`, `optimizer_max_passes`,
`plane_nets`...) are deep-merged on top and win. Its improvement threshold,
`router_settings.optimizer.improvement_threshold`, is refused above 0 and
below 0.1 like `optimizer_improvement_threshold`, when no
`optimizer_improvement_threshold` replaces it.

Freerouting accepts keys it does not know and drops them without a word. After
the job settings are sent, they are read back from `get_effective_settings`,
and the result has three new keys:

- `settings_checked` (boolean): `true` when the settings sent were read back
  and compared. `false` when nothing was sent, or when the Freerouting server
  has no `get_effective_settings` or its answer could not be read: then
  `settings_not_applied` is empty because nothing was compared.
- `settings_not_applied` (array of strings): each setting sent that Freerouting
  does not have, or holds with another value, as `"path: not a Freerouting
  setting"` or `"path: sent X, effective Y"`. Arrays are compared element by
  element, with the index in the path (`layers[0].bend_cost`): what an element
  leaves out, and elements that were not sent, are Freerouting's defaults and
  not reported, while a sent element missing from the effective array is
  reported as `"path[i]: sent X, not in the effective settings"`. Empty when
  everything took, or when `settings_checked` is `false`.
- `settings_warnings` (array): Freerouting's own `validation_warnings` for the
  effective settings.

The routing still runs when a setting did not take. Calls without
`router_settings` behave as before, apart from the three new keys in the result.

### `route_specctra_dsn` settings now take effect; optimizer passes and threshold (minor release)

`max_passes` and `job_timeout_seconds` used to reach Freerouting as `maxPasses`
and `jobTimeoutString`. Freerouting 2.5 accepts both keys and ignores them: its
`get_effective_settings` still showed unlimited routing passes and a 12-hour job
limit. They are now sent as `autorouter.max_passes` and `job_timeout`, the names
Freerouting reads, so both now apply. A caller that passed them gets the
behavior it asked for: routing stops after `max_passes`, and a job that reaches
`job_timeout_seconds` ends `TIMED_OUT`, without output.

Two new optional arguments:

- `optimizer_max_passes` (1 to 100): at most this many optimizer passes
  (Freerouting's `optimizer.max_passes`, default 100).
- `optimizer_improvement_threshold` (0 to 100, percent): the optimizer stops
  after a pass that improves its score by less (Freerouting's
  `optimizer.improvement_threshold`, default 2.5). Values above 0 and below
  0.1 are refused: Freerouting reads them as fractions and multiplies them by
  100, so 0.05 would run as 5%. 0 is sent as given.

Values out of range are refused before Java starts. Calls without these
arguments behave as before.

### `route_specctra_dsn` and `check_freerouting` errors give the cause first, then the end of Freerouting's stderr (patch release)

When Freerouting had written to stderr, an error carried all of it in front of
the cause: about ten thousand characters of banner and warnings, with the
cause at the very end. The error now starts with the cause, followed by the
last 12 lines of stderr (at most 2000 characters, cut from the front with
`...`). When the run wrote a `.freerouting.log` next to the SES, the error
names it: it still holds all of stderr. Callers that parse the error text for
the cause no longer have to look past the banner.

### `route_specctra_dsn` fails at once on `TIMED_OUT`, `TERMINATED` and `INVALID` (minor release)

A Freerouting job that ends `TIMED_OUT` (its `job_timeout_seconds` reached),
`TERMINATED` or `INVALID` now fails the call at the next poll, with the state in
the error. These states were not seen as terminal: the call polled on until
`overall_timeout_seconds` and then failed with a timeout. Freerouting refuses
the output of all three, so no board was ever returned.

The error says to bound the optimizer with `optimize_seconds` rather than
`job_timeout_seconds`. Calls whose job completes, fails or is cancelled behave
as before.

### `route_specctra_dsn` refuses settings Freerouting did not apply, before routing (minor release)

The job settings are read back from `get_effective_settings` before the job is
started. A setting Freerouting does not have or holds with another value (an
unknown key, a typo, a value it replaced) was listed in `settings_not_applied`
once the whole run was over, and the board was routed without it. The call now
fails at that point, before anything is routed, with an `invalid_argument`
error on `field: "router_settings"`. Its `reason` names each such setting as
`"path: not a Freerouting setting"` or `"path: sent X, effective Y"`, joined by
`"; "`. No SES is written.

`settings_not_applied` stays in the result, for callers that read it, and is
now always empty. Fix the settings named in the error and call again. A
Freerouting server without `get_effective_settings` is not checked, as before:
the routing runs and `settings_checked` is `false`.

### `route_specctra_dsn` answers `success: false` when connections stay unrouted (minor release)

Freerouting ends a job `COMPLETED` when `max_passes` runs out or no pass makes
progress, whatever is left unrouted, and `route_specctra_dsn` answered
`success: true` for it. It now reads the job's last unrouted count from
Freerouting's log and, when it is not zero, answers `success: false` with a
`message` naming the count. The SES is still written, for inspection.

Two fields are added: `unrouted_connections` (the count, or `null` when the log
could not be read) and `message` (`null` when the log counts 0 unrouted). An
unknown count leaves `success: true`, as before, with a `message` saying the
count could not be read from Freerouting's log and naming the log, so the SES
was not checked for completeness. Callers that took `success: true` as "the
board is routed" now get the stricter answer; callers that want a partial SES
anyway can still read it from `artifacts.ses_output_path`.

### Routing tools refuse a `layer` that is not copper (patch release)

`route_trace`, `route_pad_to_pad`, `modify_trace` and `route_differential_pair`
now refuse a `layer` other than `F.Cu`, `B.Cu` or `In1.Cu`..`In30.Cu`, spelled
as KiCad writes it, as `invalid_argument` on `layer`, before KiCad is asked
([#205](https://github.com/Copperpour/Copperpour/issues/205)). They used to
send an unknown name such as `F.Silk` or `In0.Cu` to KiCad as `BL_UNDEFINED`,
and put a track on a non-copper layer such as `F.SilkS` and report success.
`modify_trace` now refuses before it deletes the old track. Pass a copper layer
name.

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

### Schematic writes follow a project's `top_level_sheets` (patch release)

A project's top-level sheets are the files its `.kicad_pro` lists in
`schematic.top_level_sheets`, read as eeschema 10 reads them; a project that
lists none still has `<project>.kicad_sch` alone. A write to another listed
sheet, or to any sheet under one, used to be refused as `conflict`; it now
works ([#130](https://github.com/Copperpour/Copperpour/issues/130)). Each
top-level sheet's instance paths start at the uuid its entry gives unless that
is nil, so a project that gives its sheet another uuid is no longer refused as
`stale_target` ([#125](https://github.com/Copperpour/Copperpour/issues/125)).
A file placed under two top-level sheets keeps the paths each gives it. A file
that a project's list leaves out is no root of it, as eeschema does not load
it, so a write that took `<project>.kicad_sch` for the root although the list
leaves it out is now refused as `conflict`.

On a project that lists more than one top-level sheet,
`rename_schematic_net_label` and `annotate_schematic` with `schematic_scope`
`file` are refused as `invalid_argument`, as `schematic_scope` `hierarchy`
already was: each reads one top-level sheet's hierarchy as the whole design.
Hierarchy-scope annotation still refuses a project that gives its sheet another
uuid, and reads the list as above, so a project file it cannot parse no longer
stops it. `run_erc` on a sheet under a top-level sheet other than
`<project>.kicad_sch` is refused as `unsupported_capability`. The tools that
check the editor lock on a project's root as well as on the files they write
(`edit_schematic_text`, `delete_schematic_text`, `set_schematic_title_block`,
`rename_schematic_net_label`, and `annotate_schematic` over a hierarchy) check
the one beside `<project>.kicad_sch`, the file eeschema opens and locks.

### `search_jlcpcb_parts` ranks results instead of ordering them by LCSC number (minor release)

Results now rank in-stock parts first, then as `suggest_jlcpcb_alternatives`
ranks them: Basic, then Preferred, then Extended parts, then known price before
unknown, price ascending, stock descending, and the LCSC number last, so a page
is still the same on every run. The response states this order in a new
`ranking` field. The in-stock tier matters only with `in_stock: false`; the
default already leaves out parts with no stock.

`limit` applies after ranking, so a limited search can now return different
parts than before: the best-ranked ones rather than the lowest LCSC numbers.
Callers that relied on LCSC order should sort the results themselves.

### `set_board_page` (minor release)

A new `pcb_board` tool sets the paper size and orientation of a board KiCad
does not hold open, the board counterpart of `set_schematic_page`
([#257](https://github.com/Copperpour/Copperpour/issues/257)):
`set_board_page(board, size, portrait?)`. `size` is one of the sizes
`set_schematic_page` takes (A0–A5, A–E, USLetter, USLegal, USLedger); a `User`
size is not offered. `portrait` defaults to `false`, landscape. Nothing on the
board moves.

The saved file's `(paper …)` token is replaced whole, whatever its form, or,
on a board that has none and so is A4 to KiCad, inserted directly after
`(general …)`, written as KiCad 10 writes it: `(paper "A3")`,
`(paper "A3" portrait)`. A KiCad 5 board's `(page …)` node is replaced the same
way. The write takes the board's lock and is read back. A board whose file
already holds that token is not written, and returns `changed: false`.

KiCad 10 has no command that sets a live board's page settings, so a board
KiCad holds open is refused with `unsupported_capability`, its `capability`
`set_board_page`: change the page in the PCB Editor (File > Page Settings), or
close the board and retry. The live-board gate the other board writers use
decides this, so its own refusals stand too: KiCad running with no PCB editor,
and a board this session saw KiCad hold, which stays refused once KiCad no
longer answers for it.

The response gives `paper` and `paper_size_mm` as `get_board_info` reports
them, read back from the written file: `paper` names the size, and
`paper_size_mm` is `null`, as it is for every named size. `portrait` gives the
orientation read back, which `get_board_info` does not report. With them come
`changed`, `file`, `source: "file"`, `saved: true`, and the `fallback_reason`
and `warning` of `set_board_title_block`'s file writes. A file that does not
read back as written returns `mutation_outcome_uncertain`.

No existing tool's arguments or responses change.

### `set_board_stackup` (minor release)

A new `pcb_board` tool replaces the physical stackup of a board KiCad does not
hold open ([#78](https://github.com/Copperpour/Copperpour/issues/78)):
`set_board_stackup(board, copper_layers, dielectrics, solder_mask?, silkscreen?,
finish?, impedance_controlled?, board_thickness_mm?)`. `copper_layers` runs top
to bottom, one per copper layer of the board, each `{thickness_mm}` or
`{weight_oz}`, an ounce read as KiCad reads `oz` typed into a field, 1.37 mil.
`dielectrics` runs top to bottom between them, each `{type: "core" | "prepreg",
thickness_mm, material?, epsilon_r?, loss_tangent?}`. `solder_mask` (`color`,
`thickness_mm`) and `silkscreen` (`color`) apply to both sides; a color is one
of KiCad's names (Green, Red, Blue, Purple, Black, White, Yellow) or
`#RRGGBB`/`#RRGGBBAA`, written as KiCad writes one, `#RRGGBBAA`. `finish` is
one of Board Setup's finish names.

What a call leaves out is kept from the board's stackup: a dielectric's
material, εr, loss tangent, color and thickness lock, taken from the dielectric
in the same place, as KiCad numbers them; the mask's and silkscreen's colors
and the mask's thickness, material, εr and loss tangent; the finish, impedance
control and the edge settings. Where the board has no stackup, KiCad's
defaults: FR4, εr 4.5 and loss tangent 0.02 for a dielectric, a 0.01 mm mask
with no color, finish "None".

The stack is written as KiCad writes one, in its order and layout, and the
board thickness under `(general …)` is always the stack's sum, which KiCad
reports and exports. `board_thickness_mm` only checks it: a sum more than
0.001 mm away is refused with `invalid_argument`, naming the sum. Refused the
same way: a count of copper layers or dielectrics that does not fit the board,
a copper layer with both or neither of `thickness_mm` and `weight_oz`, a
thickness that rounds to under 1 nm or that KiCad's reader would clamp, a
`solder_mask` or `silkscreen` on a board without those layers, and a value of
the wrong type or outside the schema. The copper-layer count is never changed.
Refused as `plan_blocked`: a board file older than format 20200922, whose
layer table may name copper otherwise, until KiCad 10 has saved it; copper
that is not F.Cu, In1.Cu … B.Cu; a dielectric that holds sub-layers, left to
Board Setup.

Before writing, kicad-cli loads and saves a copy with the new stack (`pcb
upgrade --force`); unless KiCad's save gives back every layer, value and the
thickness, or when there is no kicad-cli, the tool returns `plan_blocked`
naming each difference, and writes nothing. A board KiCad holds open is
refused, before the copy and again before the write; a file saved after the
read returns `conflict`; the saved stack is read back. The response gives
`changed`, `copper_layer_count`, `board_thickness_mm`, `finish`,
`impedance_controlled`, `edge_connector`, `has_edge_plating`, `count` and
`layers`, each layer as `get_board_stackup` gives one, less `enabled`,
`user_name` and a copper layer's `material`, and with the `color` the file
names; `source: "file"`, and `warning` when the file changed.

`get_board_stackup`'s `hint` and description now name `set_board_stackup` for
a closed board. No existing tool's arguments or responses change otherwise.

### `set_custom_rule` (minor release)

A new `verification` tool adds or replaces one named rule in the board's
sibling `.kicad_dru` custom rules file:
`set_custom_rule(board, name, constraints, condition?, layer?, severity?)`
([#108](https://github.com/Copperpour/Copperpour/issues/108)). Each constraint
is `{type, min?, opt?, max?}`, for the types whose values are min, opt and max;
values are written in mm, except `track_angle` and `via_count`, which KiCad
reads without units. `layer` is a layer name as the board names it, `outer` or
`inner`. A rule of the same name is replaced where it stands; a new one goes at
the end, where KiCad gives it precedence over every earlier rule that matches
the same items. The rest of the file is left as written. A missing file is
created with `(version 1)`.

`kicad-cli` drops a rules file it cannot parse and checks against KiCad's
implicit rules alone, without a word. Before writing, the tool checks the whole
resulting file as KiCad 10's rule parser and expression compiler read it, and
when any rule, old or new, has a fault it returns `plan_blocked` with each fault
in `reasons`, naming its rule and line, and writes nothing. That check does not
know property names in conditions (`A.Width`), which KiCad looks up at run
time, so the tool then runs `kicad-cli pcb drc` on a copy of the board with the
file and a probe rule that fails on every item. When kicad-cli does not report
the probe, or cannot run, the tool returns `plan_blocked` and writes nothing. A
board with no item yet gives the probe nothing to fail on, and no rule anything
to act on, so there the first check alone decides. The response gives
`rules_file`, `name`, whether the file was `created` and `changed`, `previous`
and `current`, the rule's text before and as read back, `kicad_checked`, false
when the probe did not run, and `unchecked_properties`, the property names the
file uses that no check verified. Two rules of the name return
`ambiguous_target`; a file saved after the read returns `conflict`.

No existing tool's arguments or responses change. `verification` rises from 10
to 11 tools.

### `set_erc_severity` (minor release)

A new `sch_export` tool sets the severity of ERC checks in a project file
([#231](https://github.com/Copperpour/Copperpour/issues/231)):
`set_erc_severity(project, severities)`. `project` is the `.kicad_pro`;
`severities` maps a check, by its key under `erc.rule_severities` (for
example `single_global_label`), to `error`, `warning` or `ignore`. A check is
one KiCad 10.0.6 has or one the file already names, as a later KiCad writes
the checks it adds. For `pin_to_pin`, `error` and `warning` both turn the check
on, and KiCad's pin map sets each conflict's severity. Only the named keys are
written, through the project file's revision check, and a project with no
`erc` section gets one holding only them. Any other check or severity, an
empty map, a path that is not a `.kicad_pro`, and a project KiCad holds (its
`~<project>.kicad_pro.lck`) are refused, and nothing is written.

The response gives `file`, `changed` (false when the file already held every
severity named, and then nothing is written) and `severities`, each check's
`previous` and `current`, `previous` null where the file did not name the
check. `run_erc` reports by the new severities from its next run. No existing
tool's arguments or responses change. `sch_export` rises from 10 to 11 tools.

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

### `edit_sheet` and `edit_sheet_pin` keep pins on their edges (patch release)

Resizing a sheet with `edit_sheet` now moves the pins on its right and bottom
edges and its `Sheetfile` caption with those edges, and pulls a pin past the end
of a shrunken edge back to the corner. Before, the pins were left behind and
KiCad relocated them on load
([#285](https://github.com/Copperpour/Copperpour/issues/285)).

`edit_sheet_pin` given `x` and `y` on another edge, without `side`, now turns the
pin to that edge, and `changed_fields` includes `side`. A position on the edge
the pin already names, including either of its corners, keeps the pin's side,
and `changed_fields` stays `["position"]`. A corner of two edges the pin is not
on takes the first of `right`, `left`, `top`, `bottom`. Callers needn't change
anything; a position on no edge behaves as before.

### Specctra jobs use `copperpour` names (minor release)

`export_specctra_dsn` writes its reverse manifest to `<output>.copperpour.json`
by default instead of `<output>.konnect.json`; `manifest_output_path` still
chooses another path. The Rust exporter names the DSN's padstacks
`copperpour_pad_NNNN` and `copperpour_via_NNNN`, its images
`copperpour_image_<ref>` and its net classes `copperpour_class_NNNN`, so
Freerouting's SES carries the same names. A KiCad-native DSN keeps KiCad's
names, as before. `plan_specctra_ses_import` and `apply_specctra_ses` check
the SES against the names in the manifest you pass, so a `.konnect.json`
manifest and its SES from an earlier release still validate. Pass the old
manifest path explicitly; nothing looks for `<output>.konnect.json`
([#250](https://github.com/Copperpour/Copperpour/issues/250)).

### A hook reads current only when `init` would leave it alone (patch release)

A hook read `current` whenever the command `init` writes was present. Now it
is `current` exactly when `copperpour init` would leave our handler for it
unchanged. Three new reasons for a `different` hook:

- `duplicate_handler`: another handler of ours beside the current one, such as
  the `konnect` alias's hook or the pre-hook-JSON `… skill <name>` form, or a
  second entry for it. `init` removes it.
- `stale_matcher`: our entry's tool matcher is from an older release. `init`
  rewrites it.
- `shared_entry`: ours is only inside an entry with your handlers, under your
  matcher. `init` moves it into an entry of its own.

Any of them makes the guidance `out_of_sync`. User handlers, hooks for a
Copperpour binary in another directory, and entries `init` drops without
touching ours still leave the hook `current`. `copperpour status` scans on
every run. `get_installation_info` and the startup notice reuse the check
recorded for the current install marker, so for guidance installed before
this release they keep the earlier result until the next `copperpour init`
([#245](https://github.com/Copperpour/Copperpour/issues/245)).

### Schema errors name the alternative the caller chose (patch release)

When an argument matches none of a `oneOf` or `anyOf`'s alternatives, the
`invalid_argument` refusal now reports the alternative the value names. An
alternative that rejects the value's JSON type, or whose `const` tag (such as
`type`) the value fails, is passed over.

- A `graphics` item of `set_footprint_graphics` or `create_symbol` of type
  `rect` without `fill` was refused as `graphics[0].type: "line" was
  expected`; it is now `graphics[0].fill: "fill" is a required property`.
- An unknown type such as `rectangle` is now `graphics[0].type: "rectangle" is
  not one of "line", "arc", "rect", "circle", "poly"`, where it used to name
  the first type or an unrelated field.
- A `drill` object of `create_footprint` or `edit_footprint_pad` was told it
  is not a number; it now hears about its own fields, such as
  `drill.shape: "round" is not one of "oval"`.
- `load_toolset` with `name: [5]` now reports `name[0]` instead of `name`.

The same calls are refused. `error.field`, `error.reason` and the `message`
built from them change
([#134](https://github.com/Copperpour/Copperpour/issues/134)).

### The live-KiCad test variables and the plugin class use Copperpour names (patch release)

Every `KONNECT_<X>` variable that points an ignored live-KiCad or e2e test at a
board, schematic, library, reference, `kicad-cli` or output directory is now
`COPPERPOUR_<X>`: `COPPERPOUR_LIVE_KICAD_BOARD`, `COPPERPOUR_E2E_KEEP_DIR`,
`COPPERPOUR_CAPTURE_IPC_FIXTURE` and so on. The `KONNECT_<X>` name is still
read when the new one is unset, for one release. An empty value now counts as
unset, so set `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` rather than to nothing. Only
developers running those tests see this; no tool's arguments or responses
change
([#250](https://github.com/Copperpour/Copperpour/issues/250)).

The plugin's action class is now `CopperpourPlugin` instead of `KonnectPlugin`.
KiCad stores the toolbar button's visibility under the plugin file and class
name, so a hidden Copperpour button reappears once after the upgrade; hide it
again under Preferences → PCB Editor → Action Plugins.

### Transaction journals and scratch files are now `.copperpour-*` (minor release)

Multi-file schematic changes write their journal as
`.copperpour-transaction-<id>.json` instead of `.konnect-transaction-<id>.json`,
and `copperpour transaction abandon` renames it to
`.copperpour-transaction-<id>.abandoned.json`. A `.konnect-transaction-*`
journal left by an earlier release is still listed by `transaction status`,
completed by `transaction recover` and by the check every multi-file change
runs first, and abandoned under its old prefix. An older, Konnect-named build
does not see the new journals, so recover any interrupted change before going
back to one. Atomic-write scratch files are `.copperpour-*`, export staging
directories `.copperpour-export-*` (holding `.copperpour-previous-*`), the
Freerouting SES scratch file `.<name>.copperpour-<uuid>.tmp.ses`, the ERC report
directory `copperpour-erc-*` and the JLCPCB catalog work directory, beside the
database, `copperpour-jlcpcb-*`. Add `.copperpour-transaction-*.json` to any
ignore rule that names the old pattern
([#250](https://github.com/Copperpour/Copperpour/issues/250)).

### `validate_wire_connections` and `find_orphan_items` report a wire end lying mid-span without a junction (patch release)

Both tools counted a wire end that lies mid-span on another wire as connected,
with or without a junction there. eeschema joins the two only through a
junction or a label at the point, so KiCad's netlist keeps them apart and its
ERC reports the end as `unconnected_wire_endpoint`. Both tools now report it
too: `validate_wire_connections` among `floating_endpoints`, and
`find_orphan_items` as a `dangling_wire_end`
([#347](https://github.com/Copperpour/Copperpour/issues/347)). A junction or a
label at the point still joins them. To join such an end, add a junction there
with `add_junction`.

### Wire tools dot only the T's their own wire forms (patch release)

`add_wire`, `batch_add_wire`, and the wire stubs of `connect_to_net` and
`batch_connect_to_net` used to add a junction at every junction-less T on the
sheet, wherever it was. KiCad treats a wire end that lands mid-span without a
junction as a separate net, so any call joined those nets without saying so.
These tools now add a junction only where the wire they draw forms a T or
meets three or more exits, as the routed tools have done since mixelpixx/Konnect#887
([#118](https://github.com/Copperpour/Copperpour/issues/118)).
To join a T that has no junction, call `add_junction` at that point.

## v0.15.0

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

### A bare `kicad_cli` or `kicad_binary` is looked up on `PATH` only (patch release)

A bare name, such as the default `kicad-cli`, no longer resolves to a file of
that name in the server's working directory, and empty or relative `PATH`
entries are skipped. A stray `kicad-cli` there used to stop discovery:
`get_installation_info` reported `cli_path: "kicad-cli"`, and on Windows the
installed KiCad was never found. The lookup now continues to `PATH` and the
known install locations, and `cli_path` names the binary found. To use a
binary in the working directory, configure it as `./kicad-cli`
([#182](https://github.com/Copperpour/Copperpour/issues/182)).

### The native bridge registers only where the server looks (patch release)

The KiCad plugin now picks its native-bridge directory by the server's rule.
It ignores an empty or relative `XDG_DATA_HOME` (and, on Windows,
`LOCALAPPDATA`) instead of writing below KiCad's working directory. If no
absolute directory remains, the settings dialog shows the bridge as
unavailable and it stays off. On
Windows the server reads the LocalAppData known folder, so a `LOCALAPPDATA`
changed to name another folder still leaves the two apart.

`COPPERPOUR_BRIDGE_DIR` must be an absolute path on both sides. A relative
value, or one starting with `~`, now leaves the bridge off, and
`export_specctra_dsn` reports it in `native_bridge_diagnostics`. Before this,
the plugin and the server each resolved it against their own working
directory ([#181](https://github.com/Copperpour/Copperpour/issues/181)).

### `connect_to_net` with `stub_length: 0` writes no wire, and refuses a negative length (patch release)

`stub_length: 0` used to write a wire whose two ends are the same point. It
now writes only the label, on the pin or point itself, as
`batch_connect_to_net` does, and the response's `wire` is `null` instead of an
object with equal ends. A negative `stub_length` used to flip the stub to the
opposite side; it is now refused with `invalid_argument` naming
`stub_length`, before anything is written. Pass `direction` to choose the
side ([#143](https://github.com/Copperpour/Copperpour/issues/143)).

### New KiCad files name Copperpour as their generator (patch release)

Schematics, boards, footprints and symbol libraries that Copperpour creates
(`create_project`, `create_schematic`, `create_footprint`, `create_symbol` and
the other tools that start a new file) now carry `(generator "copperpour")`
instead of `(generator "konnect")`. KiCad replaces the value with its own the
next time it saves the file, and nothing reads it back. The schematic viewer's
window is titled "Schematic Viewer — Copperpour" and uses the Copperpour icon.

### `edit_schematic_component` removes custom fields (minor release)

`edit_schematic_component` accepts an optional `remove_fields` array of
property names and deletes each one from every placed unit of the component,
in the same atomic write as the call's other edits, `dnp` included. Omitting it
reproduces the previous behavior exactly. An explicit `null` is refused as an
invalid argument on `remove_fields`, as the advertised schema refuses it.

KiCad's mandatory fields (Reference, Value, Footprint, Datasheet and
Description) are refused in any case, as eeschema reads them in any case. So is
a name also given in `fields` or `field_placements`. Both refusals come before
anything is written. A name is matched whole, so removing `MP` leaves `MPN`.
A stray property named `DNP` may be removed here; `fields` still refuses to
write one, and the native attribute is set with `dnp`.

When `remove_fields` names at least one field, the response adds
`removed_fields`, the names deleted from at least one unit, and `absent_fields`,
the names no unit carried. A call in which every name is absent and nothing
else changes writes nothing. The readback checks that each named field is gone
from every unit, before the write and after it.

To carry a field over to a new name, set the new name in `fields` and remove
the old one in the same call. The new field is added hidden at each unit's
anchor with default text effects, as `fields` adds any field; give
`field_placements` in the same call to place or show it. A field KiCad marks
private, written `(property private "NAME" …)`, refuses the whole call as an
invalid argument on `remove_fields`, naming the field and the component, before
anything is written: none of the call's other edits is applied.

### Footprint graphics keep their library line style, and an unchanged refresh plans `noop` (patch release)

`update_footprints_from_library`, `place_component` and the footprints
`update_pcb_from_schematic` adds over IPC used to send every footprint shape
with no line style. KiCad saved each one as `(type default)`, including the
`(type solid)` strokes of every stock footprint. Footprint user text was sent
without keep-upright, so KiCad saved it `(unlocked yes)`. So were the custom
properties the refresh writes, such as `KiLib_Generator`. Each shape now keeps
the library's `(stroke (type …))`. A stroke without a type, or a KiCad 6 bare
`(width …)`, is sent as solid, as KiCad 10.0.6 loads it. User text keeps
upright unless the library marks it `(unlocked yes)` or, in a KiCad 7 or older
file, `(at … unlocked)`. The refresh still refuses unlocked user text and
properties, so its text always keeps upright. `place_component`'s Reference
and Value fields are unchanged
([#157](https://github.com/Copperpour/Copperpour/issues/157)).

Footprint text and refreshed properties are now sent at their angle as placed,
which KiCad turns upright itself, where they used to be turned by 180° past
90°. That flip showed `(unlocked yes)` text upside down from the library. A
rectangle turned by a multiple of 90° keeps its corners in rotated order, so
KiCad saves the library's own `start` and `end`.

`update_footprints_from_library` also stopped reporting changes that are only
KiCad's representation: the parent each child names, an SMD pad's zero-size
drill, chamfer and anchor settings a pad's shape does not use, and a pad angle
of 270° against −90°. A footprint that matches its library now plans `noop`,
and so does a dry run right after an apply. Before, every stock footprint
planned `pads`, `graphics` and `metadata` on every run. A footprint an earlier
Copperpour refreshed plans `graphics` and `metadata` once more, because its text
was saved `(unlocked yes)` and turned; applying that plan settles it.

KiCad 10.0.6 reports a `(type default)` stroke as solid over IPC, so the
planner cannot see strokes that an earlier refresh turned into `default`, and
does not offer to restore them. Use KiCad's own Update Footprints from Library
for those. Arguments and response shapes are unchanged.

### `update_footprints_from_library` can reset Reference and Value text (minor release)

`update_footprints_from_library` accepts two optional booleans, both
`false` by default, named after KiCad 10's own Update Footprints options:

- `reset_text_sizes_and_styles` gives each refreshed footprint's Reference and
  Value the library's text attributes: size, thickness, angle, justification,
  mirroring and style.
- `reset_text_positions` moves them to the library's position.

Either keeps the fields' text, layer and visibility. A library Reference or
Value with no position, layer or effects is refused for that footprint when a
reset is asked for. With neither flag, Reference and Value keep their placed
effects as before, and the plan revision is unchanged.

A plan that resets a field reports `text_effects` in `changed_domains`. Each
change then lists `text_effects`: the field, and its `before` and `after`
position, angle, size, thickness (`null` for KiCad's default of one eighth of
the height), style, mirroring, keep-upright and alignment. After the commit
the board is read back, and a field that does not hold its reset returns
`mutation_outcome_uncertain`.

### `edit_footprint_pad` and `create_footprint` set a pad's fabrication property (minor release)

`edit_footprint_pad` and each `create_footprint` pad accept an optional
`fabrication_property`, one of KiCad's pad properties: `none`, `bga`,
`fiducial_glob`, `fiducial_loc`, `testpoint`, `heatsink`, `castellated`,
`mechanical` or `pressfit`. It is written as KiCad 10 writes it, and `none`
removes the property. Without the field, `edit_footprint_pad` leaves the pad
as it is and `create_footprint` writes no property. An unknown value is
refused as `invalid_argument`, naming the field, and nothing is written.

When the field is used, the response reports `fabrication_property` as read
back from the written pads. A property that doesn't suit the pad type, such
as `castellated` on an SMD pad, is still written, as KiCad allows, and the
response carries KiCad's padstack warning in `warnings`.

### The plugin's cache directory is never relative (patch release)

The KiCad plugin keeps `server.pid`, and on Windows the staged server binary,
in `%LOCALAPPDATA%\copperpour\cache` on Windows and `~/.cache/copperpour/cache`
elsewhere. It no longer reads `LOCALAPPDATA` outside Windows, or falls back to
`%USERPROFILE%\.cache` on Windows. A server recorded under the old directory
is not reaped on the next start; stop it once by hand.

When that directory would be missing or relative to KiCad's working directory
(an unset, empty or relative `LOCALAPPDATA` on Windows, a relative `HOME`
elsewhere), the plugin writes no PID file and runs the server from the plugin
directory unstaged. Stop Server then reaches only the server started by the
same plugin instance: not one orphaned by a crashed session, and not one
started before a plugin reload
([#190](https://github.com/Copperpour/Copperpour/issues/190)).

### A relative `HOME` no longer reaches the working directory (patch release)

The remaining per-user lookups now treat a relative `HOME` (and, on Windows, a
relative `USERPROFILE`, `LOCALAPPDATA` or `ProgramFiles`) as no directory at
all, instead of resolving it against the server's working directory:

- Schematic and board edits refuse, as they already did with no local-data
  directory: the cooperative document lock lives below the home directory.
  Set `COPPERPOUR_STATE_DIR` to an absolute path to edit anyway.
- `check_freerouting` and `route_specctra_dsn` skip the KiCad plugin folders
  below a relative `HOME` or `USERPROFILE`, so `searched_locations` no longer
  lists them. This supersedes the earlier note that a relative value is
  searched below the working directory; it still holds for
  `KICAD*_3RD_PARTY` and `PATH`.
- KiCad install discovery (`kicad-cli`, the bundled libraries) and the native
  Specctra bridge skip the per-user locations.

On Unix, an unset `HOME` falls back to the account's home directory. Set
`HOME` to an absolute path to restore the old locations
([#179](https://github.com/Copperpour/Copperpour/issues/179)).

### Renamed to Copperpour (minor release)

The server now reports `serverInfo.name` `"copperpour"` instead of `"konnect"`,
and the binary is `copperpour` (`copperpour.exe`). A `konnect` binary still
ships in source builds and the standalone archives and runs `copperpour`; it
will be removed in a later release. The KiCad package has a new identifier,
`com.github.copperpour.copperpour`, so it installs beside Konnect's rather than
over it, and `get_installation_info` recognises only the new identifier as a
PCM install. A version line is read as `copperpour <version>` or
`konnect <version>`.

`COPPERPOUR_STATE_DIR`, `COPPERPOUR_BRIDGE_DIR` and `COPPERPOUR_BUILD_COMMIT`
replace the `KONNECT_*` names, which are still read when the new ones are
unset. `copperpour.toml` is searched before `konnect.toml`. User and project
directories named `konnect` or `.konnect` are moved to `copperpour` or
`.copperpour` on first use; when the new one already exists, nothing moves.

`copperpour init` writes hook matchers of the form
`mcp__(copperpour|konnect)__(...)`, and the bundled agents allow both
`mcp__copperpour__*` and `mcp__konnect__*`, so either MCP server name works. No
tool's arguments or responses change.

### Messages and tool descriptions say Copperpour (patch release)

Error messages, warnings, `source_evidence.detail` text, update instructions
from `get_installation_info`, and tool and argument descriptions now name the
server Copperpour instead of Konnect. A Specctra DSN from
`export_specctra_dsn` names its `host_cad` as `Copperpour`. Kinds, reasons,
JSON keys and tool names are unchanged; match on those, not on message text.
An SES that Freerouting returns with `host_cad Konnect` still imports.

### `set_design_rules` sets via annular width and hole clearance minima (minor release)

`set_design_rules` accepts two more optional board-wide minima, in mm:
`min_via_annular_width` (0 to 25) and `min_hole_clearance` (0 to 100), the
ranges KiCad 10.0.6 loads. `min_via_annular_width` applies to plated
through-hole pads as well as vias. `get_design_rules` and the
`set_design_rules` response report both under `rules`, which now holds ten
minima. As with `min_copper_edge_clearance`, `min_text_height` and
`min_text_thickness`, a value outside the range is refused as
`invalid_argument`, naming the field, and nothing is written.

### `set_design_rules` sets copper-to-edge clearance and silkscreen text minima (minor release)

`set_design_rules` accepts three optional board-wide minima, in mm:
`min_copper_edge_clearance` (-0.01 to 25), `min_text_height` (0 to 100) and
`min_text_thickness` (0 to 25). `get_design_rules` reports them under
`rules`, beside the existing five. The ranges are the ones KiCad 10.0.6
loads. It replaces a value outside them with its default when it opens the
project, so Copperpour refuses such a value as `invalid_argument`, naming the
field, and writes nothing.

The `set_design_rules` response also carries `rules`, the eight minima as
re-read from the written project. A rule that already holds the requested
value, compared in nm as KiCad stores it, is no longer rewritten or listed in
`changed`, and a call that changes nothing leaves the project file untouched.
If the written file cannot be re-read, or does not hold a requested value,
the call returns `mutation_outcome_uncertain`.

### `set_symbol_properties` sets Value, and renames and removes custom fields (minor release)

`set_symbol_properties` accepts three new optional arguments: `value`, the
symbol's own Value field; `rename_fields`, custom fields as current name: new
name; and `remove_fields`, an array of custom field names. Omitting them leaves
the other arguments as they were, with two differences. A field written as
`(property private "NAME" …)` is now read by its name; before, its name was
taken to be `private`. And `fields` naming Value now points to the `value`
argument, so `set_symbol_properties` no longer leaves Value alone: it changes
Value only through `value`, and still does not change Reference or Datasheet.

A rename replaces only the field's name, so its value, position, visibility and
text effects stay as they were and it keeps its place among the fields, as
KiCad's own resave keeps it. Names match whole and case-sensitively, as KiCad
matches custom field names, so `MANUFACTURER` can be renamed `Manufacturer`.
Mandatory and `ki_*` names, in any case, cannot be renamed, taken by a rename or
removed. A current name the symbol does not carry, or a new name it already
carries, is refused, and a rename to the same name changes nothing. A name may
appear in only one of `fields`, `rename_fields` and `remove_fields`, and a chain
or swap of names takes one call per step. An explicit `null` for `rename_fields`
or `remove_fields` is refused as `invalid_argument` on that argument, as their
schema types say, rather than read as omitted. Every refusal comes before
anything is written. A `value` for a symbol without a Value field is refused
rather than written hidden. On a derived symbol each argument edits that
symbol's own fields, and an empty Value shows its parent's.

In `changes` a rename appears twice, the old name with `current` null and the
new name with `previous` null, and a removal appears with `current` null. When
`remove_fields` names at least one field, the response adds `removed_fields`,
the names deleted, and `absent_fields`, the names the symbol did not carry. A
call in which every name is absent and nothing else changes writes nothing. The
readback checks, before the write and after it, that each removed or
renamed-away name is gone and that each renamed field is its old block with
only the name changed.

### Per-user paths are never relative to the working directory (patch release)

Per-user paths used to resolve against the server's working directory when
`HOME` (`APPDATA` on Windows) was unset, empty or relative. Now an unset `HOME`
on Unix falls back to the account's home directory, and an unset `APPDATA`
to the roaming profile folder. When there is still no absolute home directory
(a relative `HOME`, say), per-user paths resolve to nothing:

- `load_user_config`, `save_user_config`, `get_effective_config`,
  `list_design_rules` and `add_design_rule` with `scope: "user"` refuse.
- `list_symbol_libraries` and `list_footprint_libraries` with the global table
  in scope, and `register_symbol_library` and `register_footprint_library` with
  `scope: "global"`, refuse.
- The JLCPCB tools refuse unless the server config sets `jlcpcb_db_path`
  (or `download_jlcpcb_database` is given `output_path`); component placement
  and part lookups that use the database only to enrich a result skip it.
- `copperpour install`, `uninstall` and `status`, and the startup guidance
  check, fail or are skipped as they already were for an unset `HOME`.
- User templates, the EasyEDA cache, the global library tables used for
  symbol and footprint lookup, the per-user `config.toml`, the call log and
  the run registry are skipped.

Set `HOME` (or `APPDATA`) to an absolute path to restore the old locations
([#171](https://github.com/Copperpour/Copperpour/issues/171)).

### `update_pcb_from_schematic` puts its verdict first and bounds its change list (minor release)

The sync's response listed every planned change with every pad-to-net pair,
and its keys were sorted, so `status` and `plan_revision` came after the whole
list. For 340 footprints the dry run was 166 KB, more than a client would take,
and the plan could not be applied from the chat session
([#69](https://github.com/Copperpour/Copperpour/issues/69)).

The top-level keys now come in this order: `status`, `plan_revision`,
`coverage`, `changes_count`, `changes_shown`, `changes_truncated`, `staging`,
`undo`, `diagnostics`, `unassigned_footprints`, then `changes`. `changes` is
still an array, now of the first `limit` planned changes, 50 unless the call
names another. `changes_count` is how many the plan has, and
`changes_shown` and `changes_truncated` say how many are listed, as `run_drc`
reports `shown` and `truncated` beside its violations. Only the response is
bounded: `plan_revision` covers every change, and an apply sends every change,
whatever `limit` the dry run used. `diagnostics` and `unassigned_footprints`
are listed whole. A caller that needs every change passes a larger `limit` or
an `output`.

`output` names a file to receive the whole response, every change listed, as
`run_drc` writes its report. It is written with every response that carries a
plan: ready, noop, conflict, a stale revision and applied. A refusal before a
plan exists, or an IPC failure, writes no file and leaves an earlier one in
place. A path that is the board or a file of the saved hierarchy, a directory,
or under something that is not a directory is refused before the board is
read. A write that fails after that check fails the call, as `run_drc` does:
with nothing applied the call can be repeated, and after an apply the error
carries the applied response with an `output_not_written` diagnostic, so the
commit is not repeated. `run_drc` and `get_drc_violations` share the same check
and writer, unchanged.

### `open_schematic_viewer` looks for the viewer next to the server, then on `PATH` (patch release)

The viewer was looked for next to the server binary, then as
`schematic-viewer` or `schematic-viewer.exe` in the server's working
directory. A file found there was returned as a bare name, which the spawn
looked up on `PATH`, so `viewer` named something other than the binary that
ran, or the launch failed. The second step is now a `PATH` lookup over
absolute entries, for the platform's file name only, and `viewer` is the full
path launched. Keep the viewer next to the server binary, as the release
packages install it, or put its directory on `PATH`. The not-found error now
names both places
([#184](https://github.com/Copperpour/Copperpour/issues/184)).

## v0.14.0

### `mirror_schematic_component` (minor release)

A new `sch_components` tool sets or clears the mirror of a placed component:
`mirror_schematic_component(schematic, reference, mirror, unit?)`. `mirror`
takes `"x"`, `"y"` or `"none"`, the vocabulary `add_schematic_component`
already uses, and is absolute rather than a toggle. Without `unit`, every
placed unit takes it. With `unit`, only that unit changes, because KiCad stores
the mirror per placed unit and one gate of a package is often reflected while
its siblings and power unit are not. Field text reflects with the body.
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
responses change. `sch_components` rises from 18 to 19 tools.

### `batch_add_power_symbol`, and no second symbol on a pin already on its rail (minor release)

New tool `batch_add_power_symbol` in `sch_batch`. It takes
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
property fills the BOM's DNP column.

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
Measured on KiCad 10.0.5.

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
unchanged.

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
the request or response changes.

### computed coordinates are written as KiCad writes them (patch release)

Coordinates that come out of arithmetic rather than the grid snap are now
rounded to the six decimals KiCad writes before they reach a file:

- `move_labels_by_offset` used to write a label at 167.64 moved by −5.08 as
  `162.55999999999997`. It now writes `162.56`.
- `fix_connectivity` used to move a wire end onto a pin at
  `97.78999999999999`. It now writes `97.79`, and `fixes[].to` reports the
  rounded point, both in a dry run and when applied.
- `copy_routing_pattern` rounds the translated `(start …)`, `(end …)` and
  `(at …)` it writes. It finds no tracks on a board KiCad saved, so
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
unrounded, and a caller's own coordinate is still echoed as given.

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
warnings, so that output now stops when the server exits.

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
reported only for its pins can show a depth of 0 on one axis.

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
reload it before retrying. Arguments and the success response are unchanged.

### `add_power_symbol` accepts a pin (minor release)

`add_power_symbol` takes `reference` + `pin_number` as an alternative to
`x` + `y`, like `connect_to_net`. The symbol is placed on the pin's
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
unchanged.

### KiCad 10's schematic editor is reported as unsupported, not stale (minor release)

KiCad 10.x reports an open schematic by its file name alone, with no project
and no sheet path; KiCad 11 adds the schematic editor to its IPC API and
reports both. The editor-navigation tools used to call that a malformed
identity and refuse with `stale_target`, which read as a Copperpour-side race.

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
or the caller in place of KiCad's.

### `create_netclass` and `get_netclasses` carry the differential-pair settings (minor release)

`create_netclass` could not set a netclass's differential-pair width, gap or
via gap, and `get_netclasses` did not report them. `create_netclass`
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
with an error instead of answering with that symbol's pins missing.

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
title blocks. Existing tools are unchanged.

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
live board); a readback that differs is `mutation_outcome_uncertain`. Existing tools
are unchanged.

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
unchanged.

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
Connectivity, schemas and response shapes are unchanged.

### sheet tools keep symbol instance paths to live sheets (patch release)

`add_hierarchical_sheet` used to add a path to each symbol of a reused child
file and never remove one. After `delete_sheet` and a re-add, the child kept a
path naming the deleted sheet, and every later Copperpour write to it was refused
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
  before anything is written.
- Only a project root receives the `sheet_instances` footer. A nested parent
  no longer gets the root's page-1 entry written into it.

### routed connections refuse to join another net (patch release)

`connect_pins`, `batch_connect_pins` and `add_schematic_connection` route an
L-bend, horizontal leg first. A leg that passed over another pin's tip got a
junction there, and a corner that landed on a sheet pin connected to it. Each
route also dotted every junction-less T anywhere on the sheet, which KiCad
reads as two nets. So pins, labels and sheet pins on other nets (or on none)
silently joined, and the call still reported success.

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
additive; pin positions are unchanged.

### `add_schematic_text` writes coordinates as KiCad writes them (patch release)

`add_schematic_text` wrote the caller's `x`, `y` and `rotation` into the file
as given, so a value carrying float noise, such as `54.60999999999999` from an
older response, was written that way. Each is now rounded to the six
decimals KiCad writes, moving the text by less than 1e-6 mm. The response still
echoes the caller's values. When the saved file does not hold the text at the
rounded position, the call returns `mutation_outcome_uncertain`. A value so
large that rounding makes it infinite, such as `1.8e303`, is refused as
`invalid_argument` before anything is written. Arguments are unchanged.

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
  `mutation_outcome_uncertain`.

### `set_footprint_metadata` reports what it replaced (patch release)

`attributes` replaces the whole `(attr …)` block, so a caller who added
`exclude_from_bom` without repeating `smd` or `through_hole` silently left the
footprint with no type. KiCad then reads it as unspecified.

The response now adds `changes`, which maps each changed field to its
`previous` and `current` value: a string, or `null` when absent, for
`description`, and arrays for `tags` and `attributes`. It also adds `warnings`,
an array of strings that is empty when there is nothing to say. A warning is
given when an `attributes` replacement drops the footprint type, or drops an
attribute the `attributes` argument does not accept, such as
`allow_soldermask_bridges`
([#119](https://github.com/Copperpour/Copperpour/issues/119)). `current` reflects what
was written.
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
unchanged.

### a copper zone blocks pad-net changes only on its own net (patch release)

`update_pcb_from_schematic` refuses to move a pad between nets while copper
on the old or new net is routed (`routed_pad_net_change`). On a board with any
zone, it used to count every board net as routed, so no pad on such a board
could change net. A board-level copper zone now counts as copper on its own net
only, as KiCad reports it in `Zone.copper_settings.net`, and a rule area counts
as none. A plan that conflicted only because of a pour on another net is now
`ready`. A zone whose net KiCad does not report still blocks every net.
Response shapes are unchanged.

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
Successful responses and the IPC path are unchanged.

### board text, logo and zone writers refuse an unknown layer (patch release)

`add_board_text`, `import_svg_logo`, `add_zone` and `add_copper_pour` now
refuse a `layer` KiCad has no board layer for, such as the typo `F.Silk`, as
`invalid_argument` with `field: "layer"`. Before, a live KiCad was sent the
item on `BL_UNDEFINED`, which KiCad 10 does not validate, and the file
fallback wrote the name into the board, which KiCad 10.0.6 then refuses to
load. The refusal comes before KiCad is asked anything or the board is read,
so neither changes. Every name these tools accepted and KiCad also accepts
still works, and the `F.SilkS` default is unchanged. Response shapes are
unchanged.

### `add_board_text` writes coordinates as KiCad writes them (patch release)

When no live KiCad holds the board, `add_board_text` wrote the caller's `x`,
`y` and `rotation` into the file as given, so a value carrying float noise,
such as `54.60999999999999` from an older response, was written that
way. Each is now rounded to the six decimals KiCad writes, moving the text by
less than 1e-6 mm. The response still echoes the caller's values. When the
saved file does not hold the text at the rounded position, the call returns
`mutation_outcome_uncertain`. A value so large that rounding makes it
infinite, such as `1.8e303`, is refused as `invalid_argument` before the file
is written. The live-editor (IPC) path is unchanged. Arguments are unchanged.
The prose `message` of every `mutation_outcome_uncertain` result now
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
padstack back to normal in KiCad. Response shapes are unchanged.

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
fields and the manifest are unchanged.

### `export_specctra_dsn` exports rounded-rectangle pads (minor release)

`export_specctra_dsn` refused any board with a `roundrect` pad, which covers
most stock SMD footprints. It now exports them. Specctra has no rounded
rectangle, so the DSN carries each one as a polygon, as KiCad's own exporter
does. The polygon's edges are tangent to the corner arcs, so it encloses the
rounded rectangle and stands at most about 1 µm outside it. As for every pad
shape, the pad's size is first rounded to the micrometre.

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
  this check, for reasons unrelated to pad shape
  ([#98](https://github.com/Copperpour/Copperpour/issues/98)).

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
that is not a string is refused as `invalid_argument` instead of being dropped.

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
when undoing the renames fails. Arguments and the success response are unchanged.

### Windows errors name resolved paths without `\\?\` (patch release)

On Windows, errors printed a board or lock path that Copperpour had resolved in
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
unchanged, and other platforms are unaffected.

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
both flags off was dropped without being listed. KiCad's own Update PCB
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
board's own text settings, which Copperpour does not read. The styles above are
KiCad's defaults. With Board Setup's "Apply board defaults to footprint fields"
on, which is off by default, KiCad restyles every field of a footprint it adds
from those settings, and the sync does not.

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
`via` items from the parsed board.

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
not affect these tools. Response shapes are unchanged.

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

A class that Copperpour does measure, if KiCad refuses it the same way, is now
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
default, and the via used to keep its base diameter.

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
layer and visibility are not part of this change.

### grid-snapped coordinates are written as KiCad writes them (patch release)

A point snapped to the 1.27 mm grid is now rounded to the six decimals KiCad
writes. `bulk_move_schematic_components` used to write a symbol at
132 × 1.27 as `(at … 167.64000000000001 …)` and report the same value as
`placements[].new_y`. It now writes and reports `167.64`, and `add_wire`'s
`added_wire` echo is rounded the same way. The values differ by less than
1e-12 mm; nothing else in the request or response changes.

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
the server exits.

### `get_board_extents` reads the live board and measures every item (minor release)

`get_board_extents` used to ask KiCad for the box of an empty item list.
KiCad 10.0.5 answers that with no boxes on every board, so the IPC branch never
answered. The tool then discarded the failure and answered from the saved file,
using only top-level `gr_line` endpoints and footprint anchor points.

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
the path, where it used to answer "install Freerouting or pass jar_path".

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
number. A stored price of 0 is treated as unknown
([#63](https://github.com/Copperpour/Copperpour/issues/63)): it sorts after every
known price, and `max_price_usd` excludes it. The response adds
`package_match` (the rule and the LCSC names searched), `value_match`,
`ranking` (the criteria and the effective settings), `count`, `matched_count`
and `exclusions`, which counts the parts the stock floor and the price limit
removed.
