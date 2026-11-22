# Copperpour MCP API migrations

Copperpour's tool names, arguments and responses are public API. This file
records intentional changes to them, newest release first, with the supported
replacement workflow.

Changes not yet released are in [`api-migrations/`](api-migrations/), one file
per change. `cargo xtask collect-api-migrations <version>` moves them here when
a release is cut.

Changes from Konnect, up to its last release, v0.13.0, are in
[Konnect's copy of this file](https://github.com/mixelpixx/Konnect/blob/v0.13.0/docs/API_MIGRATIONS.md).

## v0.17.0

### New `search_3d_models` and `check_3d_models` tools (minor release)

No existing tool names, arguments, or responses change. Two read-only tools
complete discovery and pre-export checks around 3D models:

- `search_3d_models` (`library` toolset) searches KiCad's bundled `3dmodels`
  directories and/or `<project_dir>/3dmodels` for `.step`, `.stp`, `.stpz`, `.stp.gz`, `.step.gz`,
  `.igs`, `.iges`, `.wrl`, `.wrz`, `.x3d`, `.idf`, and `.emn` files, returning `${KICAD*_3DMODEL_DIR}/...` or
  `${KIPRJMOD}/3dmodels/...` paths usable with `set_footprint_models` and
  `set_placed_footprint_models`.
- `check_3d_models` (`pcb_export` toolset) audits a `.kicad_pcb` file on disk
  for placed footprints with missing `(model ...)` entries or unresolvable model
  files before running `export_3d`, skipping `dnp` and `board_only` footprints
  unless `include_dnp` or `include_board_only` is set.

The audit reads the saved board, including hidden models; unsaved editor changes
are not included. Embedded `kicad-embed://` models resolve against footprint or
board `embedded_files` entries. Search follows KiCad's OCE, VRML and IDF plugin
formats; OBJ files are not loadable by KiCad and are excluded. Search returns an
absolute path whenever a variable path cannot resolve back to the discovered file.

### `add_board_text` adds `thickness`, `mirror`, `bold`, and `italic`, and aligns file/IPC default thickness (minor release)

`add_board_text` now accepts optional `thickness` (mm, positive, default `round6(size * 0.15)`), `mirror` (boolean, default `false`), `bold` (boolean, default `false`), and `italic` (boolean, default `false`), and returns all four resolved fields in both `source: "ipc"` and `source: "file"` responses.

When `thickness` is omitted, the file fallback now writes `round6(size * 0.15)` instead of hardcoded `0.15`, matching the IPC path at all font sizes. Non-positive or non-numeric `size` and `thickness` values and non-boolean style flags are refused with `invalid_argument` before touching IPC or disk. Pass an explicit `thickness: 0.15` if you relied on the old fixed 0.15 mm stroke at non-unit sizes on the file path.

### `add_rule_area` adds live KiCad rule areas in the `pcb_rule_areas` toolset (minor release)

The new `pcb_rule_areas` toolset provides `add_rule_area` for creating netless KiCad rule areas (keep-out zones) on one or more enabled copper layers of a live board open in KiCad (`ZT_RULE_AREA`, `RuleAreaSettings`).

- Accepts `layers` (specific enabled copper layers or `"*.Cu"`), either `points` or `rect` geometry in millimetres, optional `name`, and KiCad's five keepout flags (`keepout_tracks`, `keepout_vias`, `keepout_pads` defaulting to `true`; `keepout_copper_pours`, `keepout_footprints` defaulting to `false`).
- Requires a live KiCad PCB Editor holding the exact requested board (no closed-board file fallback in this increment); validates enabled layers before opening a commit, verifies post-commit readback by KIID, refills copper zones afterward (reporting an `applied_but_not_refilled` partial outcome if refill fails), and uses the shared `outcome` envelope.
- Creating a rule area does not run autorouting. The [Specctra rule-area profile](specctra-rule-areas.md), added by [#81](https://github.com/Copperpour/Copperpour/issues/81), supports straight routing keep-outs with whole-micrometre vertices and lists the forms still refused by export.

### `add_schematic_net_label` refuses a label shape KiCad cannot load or draw (patch release)

`shape` is now one of `input`, `output`, `bidirectional`, `tri_state` and
`passive`, the shapes the sheet-pin tools take. Any other value is refused as
`invalid_argument` on `shape`, through the schema and in the handler, and
nothing is written. On a global or hierarchical label it used to be written as
given: KiCad 10 then could not load the sheet, and a parent sheet's netlist left
its parts out, or, for `dot` or `round` on a hierarchical label, kicad-cli
crashed plotting it. On a `net_label` it was ignored; it is refused
there too.

Omitted, `shape` is still `input`, and a `net_label` given one of the five
still writes none, so a call that sends the schema's default with a plain label
is unchanged. Issue: [#479](https://github.com/Copperpour/Copperpour/issues/479).

### `add_via` validates drill and pad geometry before IPC (patch release)

`drill` and `pad_size` must be numbers that round to positive nanometre sizes,
with `drill` strictly smaller than `pad_size` after rounding. Invalid sizes and
malformed present values (including null) now return `invalid_argument` naming
the field before any IPC call. Previously these could reach KiCad or silently
select defaults. Omit the fields to keep the 0.4 mm drill and 0.8 mm pad defaults;
otherwise supply valid geometry. Board design rules may require larger sizes
or annular rings; this check does not replace DRC.

### `apply_copper` makes blind, buried and micro vias (minor release)

Each item of `vias` takes three optional keys, named as `query_vias` reports
them ([#423](https://github.com/Copperpour/Copperpour/issues/423)): `via_type`,
one of `through` (the default), `blind`, `buried` or `micro`, and `start_layer`
and `end_layer`, the copper layers the drill spans, in either order: both, or
neither for a through via. KiCad tells a blind via from a buried one by its
span, and keeps a through via on F.Cu to B.Cu, so the span must be the type's:
`blind` has exactly one end on F.Cu or B.Cu, `buried` neither, and `through`
spans F.Cu to B.Cu; `micro` may join any two layers. A span that is not its
type's, a layer the board does not have, a missing end, or one layer at both
ends is refused as `invalid_argument` on that item's field before anything is
sent.

Each via in the dry run's plan and in the applied read-back carries `via_type`,
`start_layer` and `end_layer`, the end nearer F.Cu first; the read-back takes
them from KiCad. These keys were refused before as additional properties. Calls
that leave them out make the through vias they made before, though a
`plan_revision` that a dry run returned before this change no longer matches.

### `apply_specctra_ses` checks the candidate under the project's rules (minor release)

The candidate DRC of `apply_specctra_ses` now applies the source board's
project: the netclasses of its `.kicad_pro`, the custom rules of its
`.kicad_dru`, and its schematic for the parity check. Before, kicad-cli read
the project from files named like the candidate, which a new candidate never
has, so the candidate was checked against KiCad's defaults and `drc.clean` could
be `true` on a board whose project DRC fails (#312).

kicad-cli runs on a temporary view of the project directory (symbolic links)
in which the board under the source board's name is the candidate; nothing in
the project is written. `drc` adds `project_rules_applied`: `true` when the
project's rules applied, `false` only without symbolic links (some Windows
setups), where the candidate is checked on its own as before.

Callers may see violations, unconnected items or schematic parity differences
that the previous candidate DRC did not report. They are the ones `run_drc` on
the source project reports for the same board.

### `apply_specctra_ses` reports DRC `errors`, `warnings`, and `findings` separately (minor release)

`apply_specctra_ses` previously set `drc.clean` to `false` whenever `drc.violations` was non-zero, even if the only finding was a warning (such as `via_dangling`), and omitted the findings list.

`drc` now separates errors from warnings and lists the findings (in the same `rule`, `severity`, `description`, `pos`, `items` shape as `run_drc`), ordered with errors before warnings and bounded to 50 entries:

- `drc.errors`: count of `error`-severity findings across `violations`, `unconnected_items`, and `schematic_parity`.
- `drc.warnings`: count of `warning`-severity findings across those categories.
- `drc.clean`: `true` when `drc.errors == 0`, `unconnected_items == 0`, and `schematic_parity_violations == 0` (warnings alone no longer mark `clean` as `false`).
- `drc.findings`, `drc.shown`, `drc.truncated`: bounded list of findings and truncation metadata.
- `drc.violations` remains the total count of entries in KiCad's `violations` array for backwards compatibility.

Callers gating on `drc.clean` will now see `true` when an import has only warnings; callers that want to inspect or fail on warnings should check `drc.warnings` and `drc.findings`.

### `apply_specctra_ses` takes `skip_creepage` (minor release)

`apply_specctra_ses` accepts the optional boolean `skip_creepage` that
`run_drc` already takes (default `false`). The candidate DRC now reads the
source project's custom rules (#312), creepage constraints included, and
KiCad checks creepage single-threaded over every net pair: on a large board
that one DRC can take minutes to hours. When `true`, the candidate is checked
against the project's `.kicad_dru` without its `creepage` constraints; every
other rule, the netclasses and the schematic parity check still apply, and the
project directory is never written.

`drc` adds `creepage_skipped`, `true` only when constraints were actually left
out: `false` when `skip_creepage` is off, when the project has no creepage
constraint, and without symbolic links (some Windows setups), where the
candidate is checked on its own. A non-boolean `skip_creepage` is refused as
`invalid_argument` before KiCad is asked. Calls that leave `skip_creepage` out
are unchanged; run the full DRC before manufacturing.

### `apply_template` executes its connection map (minor release)

`apply_template` previously only placed components despite promising wiring. It now
connects resolvable map entries using short wires and local net labels, with the
same collision checks as `batch_connect_to_net`. Component spacing allows room
below tall symbols; the response reports the resulting positions.

Inspect `connections_made` (verified against saved connectivity),
`connections_unresolved` (entries or pins that could not be wired), and
`unconnected_pins` (placed pins not wired by this call). An unresolved entry does
not prevent independent entries from being connected. `joined_existing_nets`
names verified joins to nets present before placement, including generated
internal names. Applied explicit mappings acknowledge their destination names.
Shared-pin chains are normalized into one net; a connected explicit name takes
precedence over generated names. Generated names use the actual reference and
pin number, so distinct pins with the same library name stay separate. Malformed
`net_mappings` and blank destination names now produce `invalid_argument` before
placement instead of silently using the original net.

`connections_to_wire` remains as a deprecated copy of the mapped connection guide
for compatibility; it is **not** a list of outstanding work. Use the new result
fields before deciding what still needs wiring. Pins absent from a connection
map remain unconnected; this tool does not infer missing circuitry or add
no-connect flags.

Placement and each net are separate revision-checked writes. A failure after
placement reports `mutation_outcome_uncertain` with placed references and any nets
already written. Inspect the saved sheet and finish remaining connections; do
not reapply the template, which would place another copy.

### `audit_decoupling` picks supply pins by electrical type (patch release)

`audit_decoupling`, and with it `run_design_review`, chose the pins to audit
by name alone. It now audits every `power_in` pin, whatever its name
(`IOVDD`, `VREG_IN`), and applies the name list only to pins typed
`unspecified` or `passive`. A pin of any other type is not audited even when
its name matches, so a bidirectional `3V3_EN` and a `power_out` `3V3` are no
longer reported. Nor is a pin under a no-connect flag, a ground pin (its name
or a name of its net is a ground name as KiCad's ERC reads one: containing
`GND`, or `EARTH`, `EARTH_…`, `VSS` or `VSSA`), or a power symbol's pin. A
hidden `power_in` pin is on the net its name names, as KiCad nets it, so a
capacitor on that rail decouples it. `total_power_pins` and `pass_count` count
the pins audited. Issue: [#120](https://github.com/Copperpour/Copperpour/issues/120).

### Unmodified Claude/Codex guidance auto-updates on upgrade and drift notices use a runnable `init` path (patch release)

`copperpour init` now writes a sibling manifest
(`~/.copperpour/.installed-manifest-claude.json` or
`~/.copperpour/.installed-manifest-codex.json`) recording the SHA-256 digests of
installed skills, references, and agents plus managed Claude hook entries
([#731](https://github.com/Copperpour/Copperpour/issues/731)).

When an upgraded MCP server scans an `out_of_sync` Claude or Codex install whose
manifest version matches `.installed-<client>`:

- Unmodified files and managed sole-handler hook entries matching the recorded
  manifest (and newly bundled files) are updated atomically in place. If all
  items become `current`, `.installed-<client>`, `.installed-manifest-<client>.json`,
  and `.guidance-checked-<client>` advance without emitting `guidance_notice`.
- User-modified files, user-deleted files, user-edited or shared hook entries,
  symlinks, and legacy installs without a matching manifest are left untouched.
  Remaining drift emits `guidance_notice` naming any locally modified files
  (`(locally modified: ...)`).
- Drift notices now include the serving executable's runnable path (quoted when
  needed) instead of bare `copperpour init` whenever bare `copperpour` on `PATH`
  does not resolve to the serving binary (e.g. KiCad PCM plugin installs).
- `copperpour init` skips rewriting unchanged files and reports updated vs.
  unchanged skills, agents, and hooks in its progress output and final summary.

### `batch_add_schematic_net_label` adds several labels in one write (minor release)

The new `sch_batch` tool takes `labels`, each entry with the arguments
`add_schematic_net_label` takes: `net`, `x`, `y`, `rotation`, `label_type`
and `shape`. A sheet's interface of hierarchical labels is one call instead of
one per signal:

```json
{
  "schematic": "interface.kicad_sch",
  "labels": [
    {"net": "SDA", "x": 96.52, "y": 99.06, "rotation": 180,
     "label_type": "hierarchical_label", "shape": "bidirectional"},
    {"net": "SCL", "x": 96.52, "y": 101.6, "rotation": 180,
     "label_type": "hierarchical_label", "shape": "input"}
  ]
}
```

`shape` is one of `input`, `output`, `bidirectional`, `tri_state` and
`passive`, the shapes `add_sheet_pin` takes, and defaults to `input` on a
global or hierarchical label; a `net_label` given one is refused. Every entry
is checked before the file is read, so a malformed one refuses the whole call
with `invalid_argument` naming it (`labels[1].x`) and nothing is written.

The labels are written in one revision-checked write. A save made after the
read, or KiCad's lock on the sheet or on its project's root, refuses the call as
`conflict` with nothing written. `added` is read back from the written file: per
label its `net`, `label_type`, `shape` (`null` on a `net_label`), `x`, `y`,
`rotation` and `uuid`, in request order, with `added_count`. A write that fails
after it may have replaced the file, a file that cannot be read back, or one that
does not hold exactly the requested labels beside the ones it had returns
`mutation_outcome_uncertain`; reload and inspect it before retrying.

### `batch_add_power_symbol` dots the corner a two-pin group's symbol sits on (patch release)

A group whose pins end on two points puts its power symbol on the first stub's
end, a corner of the bar, where the stub, the bar and the power pin meet. That
corner now gets a junction dot, as eeschema draws one at three or more exits,
and it is listed in `junctions_added`. KiCad joined the pins there without the
dot, so no net changes. Groups of one point, or of three or more, are
unchanged. Issue: [#376](https://github.com/Copperpour/Copperpour/issues/376).

### `batch_rotate_labels` counts only the labels it rotates (patch release)

A `labels` item missing `rotation`, or naming a label that is not at its
`x` and `y`, used to be counted in `rotated` and left as it was
([#724](https://github.com/Copperpour/Copperpour/issues/724)). An item missing
`net`, `x`, `y` or `rotation` is now refused as `invalid_argument` naming it,
such as `labels[1].rotation`, as `batch_add_junction` refuses a position
missing a coordinate. A label that is not there refuses the call naming the
item, and two items naming the same label are refused as `invalid_argument`.
In every case nothing is written: the batch is now one read and one write, as
its description says. A call whose labels are all found, each named once,
behaves as before.

### Batch schematic edits support field text placement and unit selection (minor release)

Each `batch_edit_schematic_components.edits[]` entry now accepts
`field_placements` and `unit`, with the same schema, placement parser, and writer
as `edit_schematic_component`. This moves field text without moving symbol bodies
or changing field values. For example:

```json
{
  "schematic": "amplifier.kicad_sch",
  "edits": [
    {"reference": "R1", "field_placements": {"Value": {"x": 12.5, "y": 34.5, "rotation": 90}}},
    {"reference": "U1", "unit": 2, "field_placements": {"Value": {"hide": true}}}
  ]
}
```

Coordinates are absolute schematic millimetres, not offsets from the symbol.
Omitted placement attributes retain their existing values. A position (`x` or
`y`) on a part with several placed units requires a positive `unit` selector.
Rotation and visibility changes without `unit` apply to every placed unit.
A supplied unit selects field text only: values, footprints, custom fields, and
DNP remain whole-component edits. A DNP edit with `unit` but no nonempty
`field_placements` is refused for that entry, as in the single-component tool.

Successful placement entries add `unit_count` and `units` from the saved-file
readback. Each unit includes `field_placements`, keyed by field name with
`x`, `y`, `rotation`, and `hide`, plus its UUID and other existing component
readback data. `units` includes every placed unit, even when only one was edited.
Existing batch response fields, including the per-field `fields` array and
`updated_count` (successful edit entries), keep their meanings. Multiple entries
can place text on different units of the same component; their readbacks show the
final saved state, including values or DNP set by later entries.
`updated_unit_copies` and `created_unit_copies` continue to count property-value
writes and creations; text placements are reported through `changes` and `units`
rather than added to those counters.

The batch plans in memory and commits once through the existing revision-checked
atomic writer. Atomic publication does **not** mean every requested edit succeeds:
inspect `errors` as well as `updated`. Missing fields/units, empty per-field placement specs,
and a missing selector for a multi-unit position are reported in `errors`; other
valid field changes and entries can still apply. With `field_placements`,
non-positive or out-of-range integer unit selectors refuse that entry before its
value/DNP edits. Invalid schema types or unknown placement attributes refuse the whole call
before the handler runs. No missing field is created by `field_placements`; use
`fields` with `create_missing: true` to create a custom field in the same batch.

A field on a particular placed unit may receive a placement assignment only once
per batch. An overlapping assignment refuses **all placements in the later
entry**; its otherwise valid value/field/DNP edits may still apply. Combine that
field's position, rotation, and visibility in one placement spec.

Malformed source placement data or a failed prospective readback refuses the
whole write. A mismatch in the component/placement readback after publication returns
`mutation_outcome_uncertain`, naming the schematic and entries carried in the
write. Reload and inspect those entries before retrying; do not replay the whole
batch merely because verification failed. Editor-lock and revision-conflict
protections remain in force, including locks held on a parent/root sheet.

### DRC and library responses expose consistent truncation metadata (minor release)

`get_drc_violations` now accepts `limit` (default **50**), like `run_drc`.
Both return `shown` (the number of entries in `violations`) and `truncated`
(whether more severity-filtered violations exist). `limit: 0` returns counts
without violation rows. Raise `limit` when a larger sample is needed, or supply
`output` to publish the complete JSON report, including all reported categories
and severities. The limit affects presentation only: the complete DRC still runs,
`filtered_count` and existing total/category/error/warning counts remain complete,
and missing categories remain unchecked (`null`), not checked zeroes.

`list_symbols_in_library` adds `shown` alongside its existing `truncated` flag.
Its `count` remains the full number of symbols; the default limit remains **100**.

`search_symbols` and `search_footprints` add `shown` and `truncated`, with the
existing default limit of **50**. Their existing `count` still counts returned
matches and equals `shown`; it is **not** a full catalogue total. Search looks
for one extra match to establish `truncated`, including across libraries, then
stops. If no extra match exists, proving that may visit all remaining registered
libraries. `limit: 0` returns no matches and reports whether any match was found.
Search metadata covers the libraries successfully searched; unreadable or missing
libraries retain the existing skip behavior. Result order and search scope are
unchanged; this adds neither pagination nor exhaustive catalogue accounting.

All five tools accept non-negative integer limits, including JSON spellings such
as `1.0`. Negative, fractional, null, boolean and string limits produce `invalid_argument`
at both the served schema and direct handler boundaries. In particular, negative
limits no longer fall back to a default, and `1.0` applies a one-entry limit.
Limits bound entry counts,
not response bytes or nested violation items.

### `open_schematic_viewer` uses the browser service; standalone Tauri viewer deprecated (minor release)

> Later superseded by [standalone viewer retirement](retire-standalone-viewer.md):
> the owner waived the planned release grace period and approved immediate removal.
> Statements below about retention describe this earlier phase only.

The tool name and required `schematic` argument remain. An optional `open_browser`
boolean defaults to true; false starts/reuses the service without launching a
browser. Malformed present values, including null, are refused. The service uses
the server's configured `kicad-cli` or normal discovery and reads saved files.
It applies the hierarchy/snapshot restrictions documented in the viewer README.

Successful responses retain `launched`, `viewer`, `schematic` and `note`.
`launched: true` now means the loopback browser service started or was reused;
`viewer` is `"browser"` rather than a standalone executable path. `schematic`
is the canonical root path. New fields are `url`, `reused`,
`browser_launch_requested`, `browser_launch_succeeded` and `browser_launch_error`.
The browser fields report the OS opener's observed exit, not proof that a browser
window became visible. A failed/timed-out opener leaves the service usable and
returns its URL with a diagnostic; use it manually rather than starting duplicates.
With `open_browser: false`, both launch booleans are false and the error is null.
Root resolution, render or bind failure returns a tool error without a success URL.

The URL includes a fragment credential; treat it as a private local capability.
One viewer is reused per canonical root in a `ToolContext`. It lives until that
MCP server context ends; closing a browser tab does not stop it. Dropping the
context signals shutdown of listener, watcher and render processes. With stdio, the
context ends when its connection closes. The stateless HTTP transport shares one
context across clients; disconnecting a client does not stop viewers, which remain
until HTTP server shutdown. A later server context gets a new URL; old URLs cannot reconnect to it. For headless/SSH use, forward the
printed port to the same local port, preserving the printed `127.0.0.1` origin;
Host/Origin validation rejects other ports or hostnames. The service makes no
outbound network requests and remains available in offline builds.

The documented `schematic-viewer` binary and `--kicad-cli` flag remain available
but are deprecated. Keep its packaging and CI through one shipped release
containing this notice. A later removal PR must wait until that release has
shipped; this change does not remove Tauri or provide an automatic fallback to it.

### Shared browser viewer service (minor release; phase two of #242)

> Later superseded by [standalone viewer retirement](retire-standalone-viewer.md):
> the owner waived the planned release grace period and approved immediate removal.
> Statements below about retention describe this earlier phase only.

This phase adds a development browser service in `copperpour-viewer`, accessible
through its Rust API and `browser_viewer` example. It does not switch
`open_schematic_viewer`, deprecate Tauri, remove `schematic-viewer`, or change
that binary's arguments. The MCP default switch and its response migration belong
to phase three; Tauri retirement must wait for the release/deprecation window.

The existing `crates/schematic-viewer/frontend/index.html` source path remains
available; it and the adjacent new `viewer.js` are shared by both adapters.
The installed Tauri binary still embeds its frontend and needs no external HTML
files. There is no source-asset path retirement in this phase.

Browser instances use a launch-bound root, authenticated loopback endpoints and
SSE snapshots. They read saved files through temporary snapshots. Outside-root or
parent-traversing child references and failed snapshot copies are refused in this
path. The legacy Tauri engine retains its existing behavior. See the viewer crate
README for the service, lifecycle, security boundaries and local testing command.

### Browser viewer startup and listener recovery (minor release)

The browser viewer retries transient listener accept errors instead of terminating
its HTTP service. Connection errors retry immediately; other errors are logged
and retry after one second. Shutdown interrupts the backoff. Authentication,
loopback binding and bounded ownership of accepted connections are unchanged.

An unchanged hierarchy renders once at startup. Source timestamps and lengths
are observed before discovery reads and compared after watcher installation;
changed sources receive a catch-up refresh. Changed project settings invalidate
all sheets, and unavailable revision metadata conservatively triggers catch-up.
Treat `revision` as an opaque change marker: it remains 1 at unchanged startup
until an actual update or explicit refresh, rather than advancing to 2 for a
redundant second full render. SSE still sends the current snapshot on connection.

Existing refusal of child paths containing `..` remains, including paths such as
`sub/../child.kicad_sch` that resolve inside the root. Tests now pin that refusal
and its diagnostic independently of the later outside-root check.

### Bus connections and pin analysis require a unique placed pin (minor release)

`get_pin_connections` and its alias `get_pin_net_name` now return
`ambiguous_target` with candidates (unit, UUID and position) when a reference and
pin number name more than one placed pin. Missing components, pins or embedded
symbol definitions return `stale_target`. Previously these tools reported the
first matching pin, or a free-text missing-target error.

`connect_pins_to_bus` uses the same strict resolver and honors sheet-local
`lib_name` definitions. Ambiguous or stale items are skipped and described in
`errors`; valid items still connect. A call with no valid items leaves the file
unchanged. Its arguments and response shape are unchanged.

Give placed symbols unique references before retrying ambiguous items. For a
pin shared by multiple placed units, use explicit-coordinate wiring and bus
entry tools for the intended candidate instead. Retry only skipped bus items;
successful items have already been applied.

### New always-visible meta-tool `call_tool` (minor release)

`call_tool(name, arguments)` runs a tool by name, whether or not its toolset is
loaded and whether or not the client lists it, and returns that tool's own
result. Arguments are checked against the target's schema as in a direct call,
and the call log records the target's name. `tools/list` does not change. A
client that never re-fetches its tool list after `load_toolset` can reach the
read-only tools of every toolset through it. No caller needs to change
anything.

By default `call_tool` runs only tools on a reviewed read-only list: queries,
listings and checks that write no file, change nothing in KiCad or in the
server's session, and send nothing over the network. Any other target is
refused before it runs, with the new error kind `tool_not_permitted` (`tool`,
and `reason` `writes_not_enabled`). That includes `load_toolset` and
`unload_toolset`, a tool that plans by default and applies on request, and a
check that can write its report to a path the caller names. Call such a tool
directly under its own name. `call_tool` never runs itself or `reload_server`
(`reason` `never_through_call_tool`).

The new server configuration key `call_tool_allow_writes` (default `false`)
lets `call_tool` run the other tools as well. Only the operator can set it, in
`copperpour.toml` or `settings.json`; no argument of `call_tool` does. An MCP
client asks for approval per tool name, so allowing the gateway may allow
mutating targets without their usual individual approval prompts. Turn the key
on only for a client that cannot call those tools directly and whose user
accepts that.

### `check_schematic_overlaps` reports a wire through a symbol's body as `wire_overlap` (minor release)

`check_schematic_overlaps` compared symbol drawings and pins, labels and text,
and nothing looked at wires, so a wire drawn straight through a symbol's body
reported clean. A wire whose ends sit on a crystal's two pin tips, drawn through
its body, joins both pins in KiCad and reported clean. It now reports each schematic
wire that passes through the drawn body of a placed symbol as an entry of
`"type": "wire_overlap"`. `a` names the wire, `{"kind": "wire", "uuid", "x1",
"y1", "x2", "y2"}` as `list_schematic_wires` writes its ends, and `b` the symbol,
`{"kind": "symbol_body", "reference", "unit", "symbol_uuid"}` as `text_overlap`
does, with `bounds_b` the body's bounds. Wires in any direction are checked.

The body is the symbol's drawings without its pins, the box `text_overlap`
compares text against. A wire counts when the middle of its run inside that box
lies strictly within it, so a wire along an edge, one that only touches an edge,
and one ending on a pin tip outside the body are not reported. Nor is a
wire with an end on one of the symbol's own pin tips that does not head back
toward the pin's root, because a symbol's drawing can reach past a pin's tip,
as KiCad's stock `Device:LED` does with its emission arrows. A wire from the
tip toward the root is reported, and a zero-length pin, such as a power
symbol's, exempts nothing, so a wire from its tip into its own drawing is
reported. A wire leaving a tip square to the pin is exempt even where it crosses the drawing that reaches past the tip, as one from the LED's cathode tip down across an emission arrow would; that is the limit of the rule. Buses and graphic lines are not checked, and a symbol whose geometry is not resolved is
skipped, as it is for `text_overlap`; it is still named in `bounds_unresolved`.

Entries are counted in `overlap_count` and listed in `overlaps` with the others,
so a sheet that reported clean may now report. `component_overlap`,
`label_overlap` and `text_overlap` entries are unchanged. Issue:
[#504](https://github.com/Copperpour/Copperpour/issues/504).

### Schematic writes respect the root editor lock (patch release)

Every tool that writes a schematic sheet now also checks the KiCad editor lock of
the project that owns it, `~<project>.kicad_sch.lck` beside the root schematic,
before it writes. Eeschema locks only that root and saves every sheet from memory,
so a child sheet was written while the project was open, and the change was lost
on Eeschema's next save. A present or unreadable root lock now refuses the write
with `conflict` naming the root, and leaves every file unchanged.
`delete_schematic_component` refuses earlier, in its document binding, with
`stale_target`. A sheet that more than one project could own is refused while any
of those projects is open. If the owning project cannot be determined, for
example because its directory is unreadable, the write is refused as well.

Save and close Eeschema before retrying a refused write. Remove a stale lock only
after confirming no editor owns it. Annotation dry runs and calls that change
nothing remain read-only and can run while the project is open.

### Claude `PreToolUse` hooks and bundled agents match `mcp__Copperpour__*` (patch release)

`copperpour init` now writes Claude `PreToolUse` hook matchers of the form
`mcp__(copperpour|Copperpour|konnect|Konnect)__(...)` instead of
`mcp__(copperpour|konnect)__(...)`, and both bundled Claude agents
(`kicad-schematic-build-agent` and `kicad-design-review-agent`) now allow
`mcp__Copperpour__*` and `mcp__Konnect__*` alongside the lowercase prefixes
([#737](https://github.com/Copperpour/Copperpour/issues/737)).

Claude Desktop's `.mcpb` extension registers tools under its capitalized
`display_name` (`mcp__Copperpour__<tool>`) in Claude Desktop's Code tab, which
missed the previous case-sensitive lowercase-only matchers and agent tool
allowlists. Unmodified installs carrying a matching install manifest upgrade
automatically on the next server start; older or customized installs report
`stale_matcher` / `different` and refresh on `copperpour init`.

### Closed-board footprint edits report late KiCad locks as `unsafe_file_fallback` (patch release)

`move_component`, `rotate_component`, `set_component_placements`,
`flip_component`, `place_decoupling_caps`, and
`refine_placement_force_directed` now hand writer and I/O errors from
`ClosedBoardError::into_result` back to dispatch instead of wrapping them in an
untyped error. When a `~<board>.kicad_pcb.lck` file appears between the
pre-write gate and the atomic rename, callers now receive
`unsafe_file_fallback` (matching other board writers). Other failures to read,
parse or write the board surface as `handler_error`. Retry only after confirming no KiCad
process holds the board and the lock is gone.

### Placement and component-list results are compact by default (minor release)

`add_schematic_component`, `add_power_symbol` and `batch_place_components` now
return a compact entry per placed part: `reference`, `uuid`, `lib_id`,
`value`, `footprint`, `x`, `y`, `rotation` and `unit`, plus `mirror_x`,
`mirror_y`, `dnp`, `unit_count`, `in_bom` or `on_board` only when they differ
from the default. The single-part tools also keep `schematic`, and their
`junctions_added`, `added_power` and `lcsc_catalog`. The full readback
(`fields`, `units`, `field_placements`, `instance_paths`, `project`, `added`,
`datasheet`) is still verified after every write; pass `verbose: true` to have
it returned as before.

`list_schematic_components` now leaves out of each row's `properties` the
fields the row already carries (`Reference`, `Value`, `Footprint`), empty
fields, and `Datasheet` and `Description`. Pass `verbose: true` for every field,
or read one part with `get_schematic_component`.

### A reference on another sheet is refused with that sheet's file (patch release)

`get_component_nets`, `get_pin_connections`, `get_connected_items`,
`get_schematic_component`, `get_schematic_pin_locations` and
`replace_component` read one sheet file. When the reference is not on it but
is on another `.kicad_sch` file in the same directory, the refusal still starts
with `Component '<ref>' not found`, now followed by ` on this sheet (…)` and
the file to pass as `schematic`. A reference recorded only for another
instance of a sheet placed more than once also names the reference that file
saves the symbol as. A reference on no sheet keeps the plain message.

### `connect_passthrough` refuses a stub that joins another net (patch release)

`connect_passthrough` now checks its 2.54 mm stub with `clear_stub_direction`
before writing, matching `connect_to_net`:

- With `direction: "auto"` (the default), a stub that would end on or run over a
  terminal of another net turns across instead of joining it.
- With a named `direction`, or with `direction: "auto"` when every direction
  would join another net (or when placed symbol pin geometry cannot be
  resolved), `connect_passthrough` refuses with `plan_blocked` and writes
  nothing.
- A stub that meets a pin or label already on the requested net still succeeds.

### Generate approximate package STEP models with `create_3d_model` (minor release)

The new `model_3d` toolset contains `create_3d_model`. It creates a new AP214
STEP file from an explicit rectangular body and optional pin-row boxes, or
from an unambiguous saved footprint F.Fab rectangle (F.CrtYd fallback only
when F.Fab has no outline) and a supplied height. No CAD runtime dependency,
network access or existing-tool rename is introduced.

Output uses millimetres, Y-up coordinates and independent, unfused solids.
The tool reports the source used and solid bounds/colours; it never attaches
the model or modifies the source footprint. Existing output paths are
refused with `conflict`; a failed post-write readback reports
`mutation_outcome_uncertain`. `call_tool` requires write permission.

Read [the usage and validation guide](../3d-model-generation.md) for exact
limits, approximation semantics, defaults, recovery and the independent
KiCad/OCCT test. Runtime success establishes exact file readback, not CAD
validation or physical fit. Existing callers need no changes.

### `create_footprint` refuses an existing `.kicad_mod` unless `replace_existing` is true (minor release)

`create_footprint` now refuses with `conflict` when `output` already exists on
disk instead of silently overwriting it. Pass `replace_existing: true` to
replace an existing `.kicad_mod` file (checked with `write_if_unchanged` against
the bytes read before writing). `import_jlcpcb_part` passes
`replace_existing: true` automatically so part re-imports keep working.

### `create_schematic` reports ownership and unlinked schematic conflicts carry a structured remedy (minor release)

`create_schematic` now resolves project ownership after writing the new `.kicad_sch` file and adds an `ownership` object to its JSON response (`status`: `"standalone"`, `"owned"`, `"unlinked"`, `"conflict"`, or `"unknown"`). Creating an unlinked sheet inside a project directory remains a success (not an error); when `status` is `"unlinked"`, the response includes `candidate_roots` and `next_calls` with the exact `add_hierarchical_sheet` invocation(s) required before writes to that sheet will succeed.

When a schematic write or read fails with `error.kind == "conflict"` because the sheet is unlinked inside a project whose candidate hierarchies parsed cleanly, the conflict error now preserves `error.kind` and `error.paths` while adding a structured `recovery` object (`cause: "unlinked_schematic"`, `next_steps` naming `add_hierarchical_sheet`, and `evidence.next_calls`) and naming `add_hierarchical_sheet` in `message`. Callers that only match `error.kind == "conflict"` require no changes.

### Custom pad shapes and the corner ratio in the library footprint tools (minor release)

As agreed on [#675](https://github.com/Copperpour/Copperpour/issues/675):

- **`edit_footprint_pad`:**
  - `roundrect_rratio` (0..0.5) sets a roundrect pad's corner ratio and is reported as `roundrect_rratio` with `previous` and `current`. It is refused on a pad of another shape unless the same call sets `shape: "roundrect"`.
  - `shape` also takes `"custom"`, which needs `custom_shape`: `anchor` (`circle` or `rect`), `clearance` (`outline`, the default, or `convexhull`) and `primitives` in `set_footprint_graphics`' items, in the pad's own coordinates.
  - `custom_shape` alone redraws a pad that is already custom. It always replaces the whole primitive list, written as KiCad writes it, with coordinates, radii and widths rounded to KiCad's 1 nm grid. The answer reports `custom_shape` as read back from the pad as written.
  - `custom_shape` on a pad that is not custom, without `shape: "custom"`, is refused on `custom_shape`. So is `shape: "custom"` without it.
- **`create_footprint`:** a pad with `shape: "custom"` needs `custom_shape`, and `custom_shape` is refused on any other shape. Before, `shape: "custom"` wrote a custom pad without primitives.
- **`get_footprint_info` with `include_pads`:** a custom pad also has `custom_shape`, in the form the two tools take. A pad the model cannot read has `custom_shape_unreadable` with the reason instead, for example a polygon with arc segments in its outline.

KiCad's single-polygon rule for custom pads is left to its DRC (`DRCE_PADSTACK_INVALID`). Callers that never use the new arguments need not change their calls.

### Live placement and Update PCB place footprints with custom pads (minor release)

As agreed on [#679](https://github.com/Copperpour/Copperpour/issues/679), part 1:

- **Now placed:** footprints with custom-shape pads, by `place_component`, `place_component_array` and `duplicate_component` while the board is open in KiCad, and by `update_pcb_from_schematic` when it adds a part. Each pad's anchor and primitives are built as KiCad builds them from the library. Before, all of these refused such a footprint ("custom-shape pads are not supported by KiCad 10's typed placement path").
- **Still refused:** a custom pad with `(clearance convexhull)` refuses the footprint as before, now naming the pad ("custom pad '1': its (clearance convexhull) has no field in KiCad 10's pad message ..."), with `unsupported_library_footprint` in Update PCB. So does a primitive the shared model does not read, such as a polygon with arc segments in its outline.
- **No longer in `not_carried`:** live placement no longer lists a custom pad's shape, options or primitives there.
- **Unchanged until part 2:** `update_footprints_from_library`, and Update PCB's footprint replacement, still refuse custom pads by name ("pad shape 'custom' is not supported by typed library refresh").

Callers that refused or worked around such footprints can now place them directly.

### Portable skills installation target (patch release)

`copperpour init`, `status`, and `uninstall` now accept `--skills-dir <absolute path>`
and the `COPPERPOUR_SKILLS_DIR` environment variable. The flag takes precedence,
and the variable is ignored when `--client` is given.
With either override, these commands operate on portable skills only, without
writing Claude agents, hooks, or client-profile installation markers. Without an
override their client-specific behavior is unchanged. The option is not a server
startup or double-click installer setting.

Keep the target's `.copperpour-skills.json` ownership marker. Custom uninstall
preserves modified/unowned files and the target root. Custom init refuses to
replace locally edited files and any file the marker does not list. Files that a
later release no longer bundles are removed by init or uninstall unless edited. See README's Managing skills
section for other clients and update instructions.

Installer-only `--client gemini`, `--client cursor`, and `--client antigravity`
(alias `agy`) select ownership-checked, skills-only roots at `~/.gemini/skills`,
`~/.cursor/skills`, and `~/.gemini/config/skills` respectively. Precedence is
explicit `--skills-dir`, then the preset directory; `COPPERPOUR_SKILLS_DIR` applies
only without `--client`.
Use the same target when checking or removing an installation. These presets do
not register an MCP server or change server-startup client selection. Existing
`claude` means Claude Code; existing `codex` also covers the desktop app. Claude
Desktop skill uploads remain a separate workflow.

### DRC no longer writes or deletes `<board>.drc.json` (patch release)

`run_drc`, `get_drc_violations`, `design_review`,
`validate_for_manufacturing`, `pcb_board_outline` and the candidate DRC of
`apply_specctra_ses` now have `kicad-cli` write its report to a temporary
directory of their own. An existing `<board>.drc.json` beside the board is left
untouched; before, the DRC overwrote it and then deleted it, or, through a
project view (`skip_creepage`, the SES candidate), overwrote the project's copy
with the view's report.

`apply_specctra_ses` no longer refuses with `conflict` when
`<candidate>.drc.json` exists, and no longer deletes that file on rollback; it
still refuses when the candidate board itself exists. Nothing for callers to
change.

### `run_drc` reports per-rule counts and accepts a rule include list (minor release)

`run_drc` adds `by_rule`, keyed by exact reported rule names:

```json
{"by_rule": {"clearance": {"errors": 7, "warnings": 2}}}
```

Counts cover the complete result after severity and rule filtering, before the
display `limit`. `limit: 0` still returns the breakdown. Each bucket counts errors
and warnings, as the existing global counters do; informational rows remain in
`filtered_count`/`violations` when requested with `severity: "info"`. A rule with
only informational rows has a bucket with zero errors and zero warnings.

Pass an optional `rules` array to inspect a subset without repeated findings
from other rules consuming the returned sample:

```json
{"board": "/absolute/project/board.kicad_pcb", "rules": ["clearance"], "limit": 10}
```

Names match case-sensitively against the response's `rule` values. Omitted
`rules` includes all names; `rules: []` includes none. Duplicate names are
coalesced and unknown names yield no matching rows. A malformed present list,
including `null` or any non-string item, returns `invalid_argument` before DRC
or report work. The new `rules_filter` field is the sorted unique effective
selection, or `null` when the argument was omitted.

`filtered_count`, global `errors`/`warnings`, `by_rule`, the displayed list,
`shown`, and `truncated` all use both filters. Existing calls without `rules`
keep their previous counts/order and gain the new metadata. Full-report
`total_violations` and category counts retain their existing unfiltered meanings;
missing categories remain `null`/`categories_not_reported`, not checked zeroes.

Rule selection does not reduce KiCad's checks, change synchronization/refill
behavior, or filter the complete optional `output` report. It reduces response
content; it does not make the DRC engine run faster.

### `edit_component` sets DNP and the BOM and position-file exclusions (minor release)

`edit_component` changed only a placed footprint's value. It now also takes
`dnp`, `exclude_from_bom` and `exclude_from_pos_files`, each optional; every
property not named is kept. Before, these flags could be set on a board only
through the schematic and `update_pcb_from_schematic`.

An edit is one KiCad undo step, and the answer is a fresh read of the
footprint, as `edit_footprint_field` answers. This applies to a value edit as
well, which before was neither a single undo step nor read back:

- The answer keeps `reference`, `value` and `footprint`, and adds `dnp`,
  `exclude_from_bom`, `exclude_from_pos_files`, `linked_to_schematic`,
  `status` (`applied` or `unchanged`), `applied`, `source`, `changed`,
  `before`, `undo` and `warning`.
- A request the footprint already holds sends nothing and answers `unchanged`.
- A fresh read that does not hold the request is a `mutation_outcome_uncertain`
  error instead of success.
- `warning` says that KiCad 10 replaced the footprint whole, and, for a
  footprint linked to a schematic symbol, that `update_pcb_from_schematic` sets
  `dnp` and `exclude_from_bom` from the symbol again.

A call naming no property reports the footprint, as before, with the new keys.

`reference`, `value` and `footprint` keep their names and formats (`footprint`
is `Library:Entry`, as `list_footprints` writes it), and a missing reference
or a footprint without a value field is refused with the same text as before.
One refusal is new: a reference that more than one footprint carries is
refused before anything is sent ("Footprint reference 'R1' is ambiguous:
KiCad returned 2 matches"); before, the first of them was reported or edited.

### `edit_component` sets and reports a footprint's lock (minor release)

`edit_component` takes `locked`, optional: `true` locks a placed footprint and
`false` unlocks it, as KiCad's own lock. Before, no tool set or cleared the
lock, though `auto_place_from_schematic` and `refine_placement_force_directed`
hold a footprint KiCad marks locked. As with the other properties, the edit is
one KiCad undo step and the answer is a fresh read.

The answer adds `locked`, at the top level and in `before`, and `changed` can
name it. A call naming no property reports the lock too. Issue:
[#39](https://github.com/Copperpour/Copperpour/issues/39).

### New `pcb_text` toolset lists and edits placed board text and footprint fields in place (minor release)

No tool could change a piece of text already on the board without deleting
and re-adding it, and none could change a placed footprint's Reference or
Value text beyond its position. Four tools make up a new `pcb_text` toolset
(`load_toolset("pcb_text")`). All need the board open in KiCad; each edit is
one undo step, is read back from KiCad afterwards, and keeps every property it
was not asked to change.

- `list_board_texts` lists board-level text items with their UUID, raw text
  (`${VARIABLE}` kept), position, angle, size, thickness, font, layer,
  alignment and KiCad's bounding box.
- `edit_board_text` moves, rotates, resizes, re-layers, re-words or restyles
  one of them by UUID: `mirrored` (or `mirror: "auto"` to match its layer
  side), `horizontal_alignment`, `vertical_alignment`, `bold`, `italic`,
  `keep_upright`, `knockout`, `locked` and `font`.
- `list_footprint_fields` lists the Reference, Value and other fields of one or
  every placed footprint, with the same attributes.
- `edit_footprint_field` makes the same edits to one placed footprint's
  Reference or Value field, and can show or hide it.

No existing call changes. `pcb_board`, already at the 20-tool cap, keeps its
tools.

### Schematic component edits preserve private fields (patch release)

`edit_schematic_component` now updates existing private fields in place instead of planning a public duplicate and returning `ambiguous_target`. `batch_edit_schematic_components` also updates existing private fields with the default `create_missing: false`.

When an edit adds a missing copy to another unit, the new copy is private if all existing copies are private. If the existing copies disagree and a copy is missing, the tool reports an error for that field and writes none of its copies. A unit with duplicate field names is still refused. `remove_fields` still refuses to remove a private field.

### `edit_sheet` relinks symbol instances and resolves owning `project_name` (patch release)

`edit_sheet` with `new_file` previously changed only the parent sheet's
`Sheetfile` property, leaving symbol instance paths in the old and new child
files stale so subsequent writes failed with `stale_target`. It now validates
that `new_file` is an existing relative `.kicad_sch` path whose subtree is
readable and acyclic, updates symbol instance paths across both the old and new
subtrees in one journaled transaction (leaving an unplaced old file
untouched), and verifies readback on every relinked file. Its answer gains
`relinked_symbol_instances`, the number of symbol instance records it changed
in those files (0 when `Sheetfile` is unchanged).

`edit_sheet`, `get_sheet_hierarchy`, and `renumber_sheet_pages` also now
default `project_name` to the owning project instead of the schematic file's
stem, so nested parents report and renumber page entries under the owning
project and its hierarchy path. `edit_sheet` and `renumber_sheet_pages` refuse a
`project_name` that disagrees with the owning project, and
`renumber_sheet_pages` refuses a parent schematic placed more than once.

### `edit_symbol_pin` shows or hides a pin (minor release)

`edit_symbol_pin` takes `hidden`: `true` hides the pin and `false` shows it,
as KiCad's pin dialog does. Without it, a pin's visibility is left as it is.

- **Write:** every hide marker of the pin is removed, and a hidden pin gets
  one where KiCad writes it, after `(length …)`: `(hide yes)`, or a bare
  `hide` in a library older than format 20241004, which KiCad 8 can still
  open. A pin already as asked is left as written.
- **Read:** as KiCad reads a pin, a bare `hide` hides it and the last
  `(hide yes|no)` decides.
- **Answer:** `changes.hidden` gives the previous and current visibility when
  it changed.
- **Refused** as `invalid_argument` on `hidden`: anything but `true` or
  `false`.

A hidden pin stacked on another, such as an exposed pad's extra pin, is drawn
over that pin once shown; move it with `x` and `y` in the same call.

### `edit_symbol_pin` chooses between pins that share a number (minor release)

`edit_symbol_pin` chose its pin by `pin_number`, narrowed by `unit` and
`body_style`, and refused two pins with one number in the same unit and body
style as pins it "cannot tell apart". Such pins are the duplicates KiCad's
symbol checker reports, and renumbering one of them is the fix.

- New selectors, which only choose the pin and change nothing: `pin_name`, the
  pin's current name, and `pin_x` with `pin_y`, its current connection point
  in mm. `name`, `x` and `y` still set new values.
- Pins sharing a number in one unit and body style are now an
  `ambiguous_target` whose candidates give each pin's name and connection
  point, so a caller can pick one. Before, this was a `handler_error`.
- Pins that share number, name and connection point stay refused as before.
- A selector that matches no pin is `invalid_argument` on `pin_name` or
  `pin_x`; `pin_x` without `pin_y`, or the reverse, is `invalid_argument` on
  the one missing.
- The answer also carries `pin_name`, the chosen pin's name before the edit.

### `export_manufacturing_package` lists JLCPCB hand-fitted parts on their own (minor release)

A footprint with KiCad's `exclude_from_pos_files` attribute that is not DNP is
fitted, but not by the placement machine: a through-hole connector soldered
after JLCPCB assembly, for example. The JLCPCB CPL already left it out, but the
BOM, read from the schematic, still listed it, so the package ended with
`JLCPCB BOM/CPL designators do not match: missing from CPL [...]` and was
reported incomplete.

For `fab_house` `jlcpcb` with assembly, such a part now leaves the BOM as it
leaves the CPL, and is written to `BOM-hand-fitted-<board>.csv` beside it, with
the BOM's columns, so it is still bought. A BOM row grouping both kinds is cut
in two, each with its own quantity. `files_generated` describes the new file
with the type `bom_hand_fitted` and its `designators`; it is not in `files`,
as it is not uploaded, and `generate_jlcpcb_order_link` uploads the BOM without
these parts.

A footprint that also has `exclude_from_bom`, or is not in the schematic (a
mounting hole), is not a hand-fitted part. The BOM is unchanged when the board
has no hand-fitted part, and for every other fab house. A
`BOM-hand-fitted-<board>.csv` left in the output directory by an earlier JLCPCB
export is removed when the board no longer has a hand-fitted part. If the list
cannot be written, the BOM is left as KiCad wrote it, a warning says so and the
package is reported incomplete.

Callers that marked hand-fitted parts DNP to keep them out of the assembly
files can set `exclude_from_pos_files` instead (`edit_component`), and clear
DNP.

### `export_netlist` exports IPC-D-356 from the board and KiCad netlists from the schematic (patch release)

`export_netlist` ran `kicad-cli sch export netlist` on the given file whatever
the format. That command does not load a `.kicad_pcb` and has no IPC-D-356
format, so both documented formats failed on a board
([#155](https://github.com/Copperpour/Copperpour/issues/155)).

- `format: "ipc"` (also `ipcd356`, `ipc-d-356`, `d356`) now runs
  `pcb export ipcd356` on the board. Given a `.kicad_sch` it is refused as
  `invalid_argument` on `board`.
- A schematic netlist format (`kicad`, `kicadxml`, `cadstar`, `orcadpcb2`,
  `spice`, `spicemodel`, `pads` or `allegro`, or the aliases `kicadsexpr`,
  `sexp`, `xml` and `orcad`) given a `.kicad_pcb` is exported from the
  `.kicad_sch` of the same name beside the board, and the response names that
  file in a new `source_schematic` field. When no such
  schematic exists the call is refused as `invalid_argument` on `board`
  before kicad-cli runs. A `.kicad_sch` path works as before.
- Any other format name is refused as `invalid_argument` on `format` before
  kicad-cli runs. It used to reach kicad-cli, which failed with
  `Invalid format`.

### `export_schematic_svg` honours the `output` filename (minor release)

`export_schematic_svg` used `output` only as a directory: kicad-cli names SVG
files after the schematic (`<stem>.svg`, one per sheet), so the caller's
filename was discarded and two exports of same-stem schematics into one
directory overwrote each other. The tool now behaves like
`export_schematic_pdf`: `output` names the root SVG file, and each child sheet
of a hierarchy is written beside it as `<output stem>-<sheetname>.svg`,
keeping kicad-cli's per-sheet convention under the caller's name
([#25](https://github.com/Copperpour/Copperpour/issues/25)).

The response keeps `exported`, now the requested root path, and adds `files`:
every published path, root first, then child sheets sorted by name. Callers
that reconstructed `<schematic stem>.svg` must read `exported` or `files`
instead. `get_schematic_view` also writes into its per-schematic
`<temp>/copperpour-schematic-views/<hash>/<stem>.svg` slot directory rather than
the shared parent directory, so two schematics with the same file stem no longer
overwrite each other's view.

### `field_placements` entries now report `drawn_orientation` and document symbol-relative rotation (minor release)

`units[].field_placements.<name>` in schematic component responses (such as
`edit_schematic_component` and `batch_edit_schematic_components`) now includes
`"drawn_orientation"`: `"horizontal"` or `"vertical"`. KiCad stores a field's
`rotation` relative to its parent symbol, so on a 90° or 270° symbol
`rotation: 90` draws horizontally and `rotation: 0` draws vertically; the
`field_placements` schema now documents this explicitly.

### `edit_schematic_component` and `batch_edit_schematic_components` normalise `field_placements` rotation and return `malformed_file` on corrupt placement/unit data (minor release)

- `field_placements.<name>.rotation` is now normalised into `[0, 360)` with `rem_euclid(360.0)` before writing to the schematic and before readback verification (for example, `450` writes and reports `90`, and `-90` writes and reports `270`), the form `rotate_schematic_component` writes. KiCad itself keeps an angle such as `450` as written.
- Unit-scoped `field_placements` now reads each placed symbol's direct `(unit N)` child instead of scanning raw block text, ignoring `(unit …)` text inside quoted property values. A missing, duplicated, or non-positive/unparseable `(unit …)` child is refused instead of silently defaulting to unit 1.
- Malformed-file aborts during `field_placements` planning now return a structured error with `error.kind: "malformed_file"` (`path`, `reason`) instead of an untyped plain-text error (`handler_error`), while keeping the existing message text. Callers branching on `error.kind` can distinguish a corrupt schematic from a concurrent-edit `conflict` or a missing target.

### `file_not_found` reports `absolute_path` and `get_installation_info` reports `working_directory` (patch release)

When a tool refuses on a missing file or directory, the structured `file_not_found`
error now reports `absolute_path` alongside the requested `path`. Relative paths
resolve against the server process's working directory (`std::env::current_dir()`),
which the MCP client sets upon launching Copperpour. When `path` is relative,
`absolute_path` reports `cwd.join(path)`; when `path` is already absolute, both fields
report that path.

Additionally, `get_installation_info` now includes `working_directory` in its `runtime`
response section, allowing clients and agents to discover the server's working
directory without guessing.

Callers should pass absolute filesystem paths to avoid resolving against an unexpected
working directory. Existing callers matching on `error.kind == "file_not_found"` or
reading `error.path` require no changes.

### Tool calls surface installed-guidance drift notice (`guidance_notice`) and defer recording until a tool runs (patch release)

When installed Claude or Codex guidance is `out_of_sync` for a new
`(install marker, embedded guidance bundle)` pair, the MCP server now attaches
the one-line drift notice as a top-level `guidance_notice` string on
non-`get_guidance` tool call responses in that drifted session (in addition to
including it in MCP `initialize` `instructions`), and writes
`~/.copperpour/.guidance-checked-<client>` for that `out_of_sync` client only
when the session's first non-`get_guidance` tool call delivers the notice
([#722](https://github.com/Copperpour/Copperpour/issues/722)).

Previously, `.guidance-checked-<client>` was written during `initialize` and
the notice appeared only in `initialize` `instructions`, where clients with
large multi-server system prompts could overlook it and where a non-KiCad
session that initialized the server without calling any Copperpour tool consumed
the one-time notice before any KiCad work ran.

`get_guidance` responses are unchanged (preserving byte-for-byte parity with
`resources/read`), and subsequent sessions whose installed guidance is
`current`, `not_installed`, or already recorded for the current
`(marker, bundle_digest)` pair see no `guidance_notice` field.

### `set_footprint_metadata` accepts `allow_soldermask_bridges` (patch release)

`attributes` now accepts `allow_soldermask_bridges`, which KiCad writes in a
footprint's `(attr …)` block, as on its stock solder jumpers. It was refused, so
an attributes edit on such a footprint removed the flag
([#119](https://github.com/Copperpour/Copperpour/issues/119)). `attributes` still
replaces the whole block: list the flag to keep it. Leaving it out drops it
without the warning given for an attribute the argument does not accept;
`changes` still shows it under `previous`.

### Footprint refresh refuses pads and graphics on missing inner copper layers (patch release)

`update_footprints_from_library` reports `footprint_update_conflict` if a selected footprint has a library pad, graphic, custom property, or reference point on an inner copper layer that the live board does not have. This now applies to front-side footprints as well as back-side footprints. No footprints in the batch are written. Correct the library layers or the board stack as appropriate, then run a new dry run before applying.

`update_pcb_from_schematic` reports `unsupported_library_footprint` for the same conflict when replacing an existing footprint. New footprint placement is unchanged.

### Library refresh compares keep-upright text angles modulo 180° (patch release)

`update_footprints_from_library` now compares angles of `keep_upright` footprint user texts, custom properties, and reset `Reference`/`Value` fields modulo 180° instead of 360°. Back-side footprints flipped left-to-right (`FLIP_DIRECTION::LEFT_RIGHT`, KiCad 10's default) no longer plan spurious `graphics`, `metadata`, or `text_effects` updates when KiCad keeps a keep-upright text 180° from the angle refresh builds. Text with `keep_upright` disabled (`(unlocked yes)`) continues to compare modulo 360°.

### Library refresh refuses footprints that own groups (patch release)

`update_footprints_from_library` now reads the live board before it reports or applies a replacement, and returns `status: "conflict"` with `footprint_children_not_carried` if a footprint it would replace owns a group, which KiCad 10.0.8's API drops. Nothing in the plan is applied. Refresh such footprints with KiCad's Update Footprints from Library. A footprint that is only a member of a board-level group is unaffected.

### Footprint unit-mapping warnings name the affected KiCad versions (patch release)

The descriptions for `edit_component`, `edit_footprint_field`, and `set_footprint_field_style`, and the live warnings from `edit_component` and `set_footprint_field_style`, now explain that whole-footprint IPC writes can drop symbol-unit mapping on KiCad 10 before 10.0.8. The warning is still returned after the same live writes; its text is not a report that the mapping was lost. Tool inputs, response fields, and edit behavior are unchanged. Callers need not change their calls.

The live warnings link to [Troubleshooting](https://github.com/Copperpour/Copperpour/blob/main/docs/TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit) for checking a board edited on an affected version and recovering missing mappings. Upgrading does not restore missing mappings.

### `check_freerouting` and `route_specctra_dsn` start errors keep the exception header (patch release)

When Freerouting's MCP server fails to start, no `.freerouting.log` keeps its
stderr, and the error ended with the last 12 lines of it. A Java stack trace
names the failure in its first lines, so a long trace lost them. The error now
puts the header of the last stack trace in front of those 12 lines when they
leave it out: the last line that names an error (`ERROR`, `Exception` or
`Error`), with the line just before it when that one names an error too, each
cut to 500 characters, then a `...` line. Errors of a run that wrote a
`.freerouting.log` are unchanged.

### `get_board_info` exposes live query failures and source evidence (minor release)

`get_board_info` now uses the shared board-reader source policy. Once the exact
board is identified in KiCad, failures reading its title block, enabled layers or
nets return an error instead of a successful answer from a possibly older saved
file. Retry the live read, or explicitly request `board_source: "saved"` to inspect
the saved snapshot. The optional selector accepts `auto` (default), `live`, and
`saved`, matching other board readers.

Existing metadata fields and `source: "ipc" | "file"` remain. New `sources` keys
identify `metadata` and `paper` independently, and `source_evidence` explains the
board observation and any saved-file fallback. Paper settings still come only
from the saved file; on a live answer with an unreadable file their provenance is
`unavailable` and their values remain null. `board_source: "live"` requires live
metadata, not live paper settings.

This supersedes the `get_board_info` fallback-policy exception documented in
`require-net-response.md` for #464/#469.

### `get_installation_info` reports the IPC endpoint the tools dial now (minor release)

`get_installation_info` reported only the address resolved at startup, as
`source: "resolved_server_config"`. With socket rediscovery, a KiCad started or
restarted after the server is found on every IPC call, so the report could say
`endpoint: null` while every tool was talking to KiCad, or name a socket the
calls no longer used.

The `ipc` block now reports what the next IPC call dials, from the same
decision the calls make:

- `endpoint`: that address, redacted as before, or `null` when nothing is
  dialled.
- `source`: `resolved_server_config` (the startup address, as before),
  `detected_at_call_time` (a socket found now), `ambiguous` (several sockets
  qualify and not exactly one answered a Ping, so none is dialled), or
  `unresolved` (nothing to dial).
- `candidates`, new: the redacted sockets an `ambiguous` detection found;
  empty otherwise.
- `configured` is unchanged: whether an address was resolved at startup.

With rediscovery on, the report looks for KiCad's socket the way an IPC call
does: file metadata, and a Ping to each candidate only when several qualify.

A caller that read `source` as always `resolved_server_config` should accept
the three new values; one that read `endpoint: null` as "KiCad is not
reachable" can now rely on it meaning that nothing is dialled.

### New tool `get_placement_overview` in the `placement` toolset (minor release)

Describes a footprint placement as numbers: airwire (ratsnest) length in total,
per net and per footprint, the longest airwires with their pads, crossings
between nets, parts outside the outline, and per net group (the project's
netclasses, or `net_groups`) its nets, parts, region, the other parts inside
that region and the closest pads of other nets. `image: true` adds
a picture as an image block. It reads the board live from KiCad when open
there, else the saved file, as `score_placement` does. No caller needs to
change anything.


How nets are matched:

- **Netclass groups** (the default) use the patterns in the project file's
  `net_settings.netclass_patterns`, matched as KiCad matches them: against
  the whole net name, as a wildcard pattern or as a regular expression,
  whichever names the net. `VCC` does not name `/power/VCC`, and `A.B` names
  `AxB`. A net in several netclasses is in each of those groups.
- **Where it can name fewer nets than KiCad** (measured against pcbnew
  10.0.7): a pattern with a brace, and an expression the Rust `regex` crate
  does not accept (look-around, `\Q...\E`), are matched as wildcards only.
  Netclass directives placed in the schematic are not read, so a net assigned
  only that way is in no group.
- **`net_groups` and `skip_nets`** patterns also match a sheet-local net by
  the name after its last `/`: `G_A` names `/Output stage/G_A`.

`call_tool` can run it: it only reads.

### `get_symbol_info` reads a `.kicad_sym` file by path (minor release)

`get_symbol_info` took only `lib_id`, resolved through the project and global
symbol library tables, so a library no table registers could not be read; a
path in `lib_id` was taken as a library nickname. It now also takes
`library_path`, a `.kicad_sym` file, with `symbol_name`, as the library editing
tools do and as `get_footprint_info` takes a `.kicad_mod` path.

- `lib_id` is no longer required: give it, or `library_path` with
  `symbol_name`. Giving both, `library_path` without `symbol_name`, or
  `symbol_name` with `lib_id` is refused as `invalid_argument`; a missing file
  is `file_not_found`.
- An answer read by path carries `library_path` instead of `lib_id` and
  `library`, with the same `name`, `pin_count`, `pins` and `properties`.

Calls with `lib_id` are unchanged.

### `get_symbol_info` refuses `project_dir` with `library_path` (patch release)

`project_dir` resolves a `lib_id`'s library through the project's symbol
library table. With `library_path`, which names the file itself, it was
accepted and ignored. It is now refused as `invalid_argument` on
`project_dir`, as `lib_id` with `library_path` and `symbol_name` with `lib_id`
are. Leave `project_dir` out when naming the symbol by `library_path`.

### Installed-guidance drift probe re-checks once when the embedded bundle changes (patch release)

`~/.copperpour/.guidance-checked-<client>` now keys its cached drift state on
both the install marker and a SHA-256 digest of that client's embedded guidance
bundle (skills, references, and for Claude, agents and hook matchers).

Previously, the cache key included only the install marker, so upgrading the
`copperpour` binary (either to a newer release or a newer dev build) never
re-scanned installed guidance or emitted the MCP `initialize` drift notice.

Callers of `initialize` and `get_installation_info` see no schema changes. On
the first start of an upgraded binary whose embedded guidance changed (or when
upgrading over a pre-change cache file without `bundle_digest`), `checked`
reports `"now"` with per-file detail and `initialize` includes the one-line
drift notice if `out_of_sync`. Subsequent starts with the same marker and
embedded bundle stay quiet (`checked: "earlier"`).

### `add_hierarchical_sheet` and `duplicate_sheet` allocate project-wide free page numbers (patch release)

`add_hierarchical_sheet` and `duplicate_sheet` now walk the owning project's
entire sheet hierarchy (across all top-level sheets and every placement of
reused child sheets) and assign the lowest unused positive page number `>= 2`
(page 1 remains reserved for the root), instead of scanning only the immediate
parent schematic. Previously, adding or duplicating a sheet inside a child
schematic could collide with page numbers already used elsewhere in the project.

If the hierarchy contains a reference cycle or an unreadable top-level sheet,
both tools now refuse the write with `conflict`, naming that file, before modifying any file. Callers that relied
on parent-local numbering or expected gaps not to be reused should inspect the
committed `sheet.page` in the tool response or run `renumber_sheet_pages` to
impose depth-first sequential numbering.

### `import_jlcpcb_part` attaches 3D models and reports partial failures (minor release)

`include_3d` still defaults to `false`. Non-boolean values are refused with
`invalid_argument` before downloading or writing anything.

With `include_3d: true`, the importer resolves the model UUID and transform from
EasyEDA's `outline3D` SVGNODE. It prefers STEP payloads with complete Part-21 envelopes, otherwise
converts OBJ to KiCad-scaled VRML (`.wrl`). Both paths require finite OBJ bounds
to establish the model's placement; a STEP-only download is refused rather than
attached at a guessed origin. STEP bytes are preserved, including non-UTF-8 strings. Envelope checks detect incomplete responses;
they are not a full CAD geometry validator. WRL fallback provides mesh
visualization, not a STEP solid for mechanical exports.

Models are saved under `${KIPRJMOD}/3dmodels/<footprint>-<content-hash>.<ext>`
with verified, create-only atomic publication. Identical regular files can be
reused. Conflicting files, directories and symlinks are never overwritten.
The generated footprint receives its converted offset/rotation through the
existing revision-checked `set_footprint_models` writer.

Successful responses include `model_3d.status: "attached"`, `uuid`, `format`,
`kicad_path`, `path`, `offset_mm`, and `rotation_degrees`. An absent model UUID
reports `status: "not_available"` without downloading or publishing a model.

A failed requested 3D stage now returns `isError: true` and `success: false`,
rather than an overall success with a discarded download/write error. Existing
structured `conflict` and `mutation_outcome_uncertain` errors are retained.
`model_3d` reports `status: "error"` or `"uncertain"`, the failing `stage`
(`download`, `prepare`, `publish`, or `attach`), model identity/path when known,
`retry_safe: false`, and recovery guidance. Unclassified writer errors are
conservatively uncertain; they do not prove that nothing changed.

The import is a multi-file operation, not one atomic transaction. Its symbol
and footprint may already have been created when the model stage fails.
`imported` identifies those paths, states that rollback was not attempted, and
reports schematic placement as `"not_attempted"`. A successfully published model
is retained if footprint attachment fails. Inspect the reported library files,
model and footprint before retrying; repair only the incomplete stage rather
than blindly repeating the import or assuming rollback.

### `copperpour init` replaces Claude hooks from other Copperpour executable paths (patch release)

Running `copperpour init` for Claude now replaces existing Copperpour-managed `PreToolUse` hook handlers registered by another `copperpour` or `konnect` executable path (in either current `hook` or legacy `skill` form), rather than appending a second set of hooks alongside them. User-authored neighboring handlers and unrelated commands are preserved.

`copperpour status` and `get_installation_info` correspondingly report `different` (`duplicate_handler`) when a hook from another `copperpour`/`konnect` executable sits beside the current executable's hook. Running `copperpour init` removes the duplicate and restores `current` status. `copperpour uninstall` remains scoped to the current executable's own hook commands.

### Dev-build install markers, `copperpour status`, and guidance drift notices include build commit (patch release)

When Copperpour is built from a Git checkout, so that its build script reads
the commit from `.git/HEAD`, `copperpour init` now writes
`<version>+<8-char-commit>` (for example `0.16.1+f5b12f82`) into
`~/.copperpour/.installed-<client>` instead of the bare crate version.
`copperpour status` and MCP `initialize` guidance drift notices display the
same `<version>+<8-char-commit>` label.

Release builds, whose builders pass the commit in `COPPERPOUR_BUILD_COMMIT`,
and builds without Git metadata (such as Nix) keep the bare `<version>`
label. This distinguishes development builds on the same crate version from
releases and from one another while remaining a valid SemVer build-metadata
string accepted by older `copperpour` binaries. `get_installation_info`'s
`build.version` and `guidance.bundle_version` fields are unchanged.

### `get_installation_info` probes the configured CLI executable (patch release)

For a nonempty `kicad_cli`, `get_installation_info` now uses the same executable
resolution as CLI tools and the schematic viewer. An explicit missing path is
reported in `kicad.cli_path` with `probe_status = "launch_failed"` and a null
version, rather than reporting another installed KiCad. Bare names still use
PATH and installation discovery; an empty value retains installation discovery.

Correct an unusable configured path and restart the server before checking again.
No argument or response-key changes are required.

### Automatic IPC discovery selects the PCB editor socket beside Project Manager (minor release)

When KiCad's Project Manager listens on `api.sock` (answering `GetOpenDocuments` with
`AS_UNHANDLED`) alongside a PCB editor on `api-<pid>.sock`, automatic IPC discovery
(`detect_ipc_endpoint`, used when `ipc_address` and `KICAD_API_SOCKET` are unset) now
probes answering candidates with `GetOpenDocuments(DOCTYPE_PCB)` and selects the unique
PCB editor endpoint instead of refusing as `ambiguous_endpoint` (#87).

When multiple PCB editors are running and discovery remains ambiguous, board-targeted
IPC tools (`update_pcb_from_schematic`, `get_board_info`, `get_board_stackup`, etc.)
probe those candidates in parallel for the exact requested board: a unique holder is
used (when no other candidate is undecided), and multiple holders of the same board are
refused as `ambiguous_target`. Explicitly configured `ipc_address` and
`KICAD_API_SOCKET` values remain authoritative and are never overridden.

### IPC recovery reports per-item create and delete rejections (minor release)

On tools that return shared IPC recovery, look in `recovery.evidence` for the failed command and KiCad's per-item status. `CreateItems` reports `rejected_items` with zero-based `reply_index`, status names, and KiCad's messages, along with `requested_count` and `reported_created_count`. `DeleteItems` reports `rejected_items` with their KIIDs and status names, as well as `reported_deleted_kiids`, `unclassified_kiids` for rows with unknown statuses, and sorted `unreported_kiids` for requested items with no result row. Reported success describes the reply, not persistence. A missing row does not prove KiCad skipped the item.

Use `recovery.mutation_outcome` and `recovery.evidence.rollback` together. The overall outcome stays `unknown`. `not_established` does not rule out partial application. `confirmed` means KiCad acknowledged dropping the named commit, not that Copperpour independently verified the whole operation left the board unchanged. Both use `recovery.cause: "kicad_api_rejection"` and link to [item rejection troubleshooting](../TROUBLESHOOTING.md#kicad-rejected-items-during-an-ipc-write). `unconfirmed` returns `ipc_outcome_unknown` and retains the original rejection evidence. Inspect the live board before retrying any unknown outcome. Write, rollback and fallback policy are unchanged.

Tools using the shared live-write path may now return structured JSON instead of plain text for per-item rejections. These include `set_board_size`, `add_board_outline`, `delete_graphics`, `add_mounting_hole`, `add_board_text`, `add_zone`, `add_copper_pour`, and `import_svg_logo`. Continue accepting plain text for other IPC errors. Routing tools using shared IPC recovery may also change from `recovery.cause: "unknown"` to `"kicad_api_rejection"`; use the structured fields rather than matching the old message prefix. `update_pcb_from_schematic` returns structured IPC recovery for these rejections instead of labeling them preflight conflicts.

Delete evidence requires KiCad to send deletion result rows. The empty-reply behavior on KiCad 10 is unchanged, and the deletion evidence tests use simulated replies.

`apply_specctra_ses` also returns structured IPC recovery for item rejections. If compensating deletion fails after the import was published, its item evidence is under `recovery.evidence.compensating_deletion`. A confirmed drop there describes the cleanup commit only; the import outcome is still unknown.

### Verify live PCB readback before reporting success (minor release)

An empty, correctly typed `GetNets` reply still means there are no nets. A reply with a different protobuf type does not.

Deleting a trace, via, or footprint requires a readable inventory of the bound board showing that the original item is gone. An unreadable inventory does not prove absence. A footprint delete sends no write if the requested reference is empty, missing, or ambiguous, or if the inventory has an unreadable footprint reference. A KiCad status that prevents the initial read is retained as evidence in the no-write refusal.

After KiCad acknowledges a write, Copperpour checks the assigned KIID and pose for initial footprint creation, and the original KIID and pose for a single-footprint move. Via placement checks the assigned KIID, position, and net before reporting success. On a net mismatch it requires a readable position and keeps the existing cleanup behavior. Via moves check the original KIID, destination, and observed net. If required readback cannot verify the write, Copperpour returns `ipc_outcome_unknown` with the board and any known item identity. Inspect the live board before retrying.

These checks do not verify via dimensions, footprint children or groups, the subsequent flip or model lookup during placement, batch alignment, or straight-trace creation. Write acknowledgement handling and rollback policy are unchanged.

### Runtime IPC failures return structured `recovery` guidance (minor release)

Non-error health diagnostics (`check_kicad_ui`, `open_project`) that report
`ipc_failure` now include an additive `ipc_failure.recovery` object, and IPC
error responses returned through the shared helpers (`ipc_failure_result`,
`ipc_target_error_result`, `ipc_uncertain_result`, `ipc_recovered_result`,
`ipc_unserved_result`, and `editor_unavailable` IPC boundaries) now include an
additive top-level `recovery` object alongside `message` and `error`.

Each `recovery` object contains:
- `cause`: stable machine-readable cause (`not_configured`, `ambiguous_endpoint`,
  `no_listener`, `access_denied`, `handshake_failed`, `kicad_not_ready`,
  `no_open_documents`, `wrong_document`, `ambiguous_document`,
  `unresolved_document_identities`, `stale_document`, `ipc_outcome_unknown`,
  `ipc_batch_recovered`, `no_pcb_editor_at_endpoint`, `preflight_refusal`, `kicad_api_rejection`, or `unknown` when unclassified);
- `observed` and `evidence`: what was actually observed without guessing from
  message text;
- `mutation_outcome`: `"none"`, `"unchanged"`, or `"unknown"`;
- `retry_safe` and `retry_prerequisites`: `retry_safe` remains `false` whenever
  prerequisites apply or outcome/safety is unknown;
- `next_steps`: ordered steps tagged with `actor` (`"agent"` or `"user"`) and
  optional `tool`;
- `troubleshooting_url`: optional link to `docs/TROUBLESHOOTING.md` when an IPC troubleshooting step applies.

`ipc_failure.kind`, `ipc_failure.message` and `ipc_failure.note` are unchanged,
and so is `error.kind`, with these exceptions:

- `get_board_stackup`, `get_nets_list`, `set_placed_footprint_models` and the
  live-board synchronization behind DRC and zone refill reported an IPC request with an
  unknown outcome, or a dropped and restored batch, as `editor_unavailable`.
  They now report `ipc_outcome_unknown` and `ipc_batch_recovered`, as every
  other IPC caller does, because the board may have changed.
- Tools that returned an IPC failure as plain text with no `error` object
  (`set_footprint_field`, `set_pad_net`, `apply_copper`, `delete_trace`, the
  `pcb_components` and `pcb_routing` IPC tools) now return a structured error:
  `editor_unavailable` when KiCad or its board editor did not answer,
  other failures retain a specific error kind when one applies (for example, `ipc_outcome_unknown` after an accepted write whose verification request fails) or use `handler_error` at the generic boundary. Use `recovery` to distinguish an explicit KiCad rejection, a Copperpour refusal, and an unclassified failure.

### IPC recovery distinguishes Copperpour refusals from KiCad errors (minor release)

A typed Copperpour refusal reports `recovery.cause: "preflight_refusal"` and `mutation_outcome: "none"`. Address the reported problem before retrying. An explicit KiCad error status that is not part of a typed preflight refusal reports `cause: "kicad_api_rejection"` and includes the status in `recovery.evidence.status`. An unclassified failure still reports `cause: "unknown"` and does not say KiCad rejected the request. Neither an error status nor an unclassified error, by itself, proves that the board is unchanged.

Treat `recovery.troubleshooting_url` as optional. It is omitted when IPC troubleshooting would not address the failure. Existing editor, transport and uncertain-outcome diagnostics still include their applicable links. If a KiCad status prevents the placement copper check from completing, the existing `plan_blocked` error includes that status in `recovery.evidence.status`; no placement was written. A write whose verification request fails now reports `ipc_outcome_unknown` on the affected trace, via and footprint paths. No tool arguments changed.

### `launch_kicad_ui(wait_ready)` re-probes and IPC discovery backs off dead sockets (minor release)

`launch_kicad_ui` with `wait_ready: true` previously resolved `ipc_endpoint()` once
before its polling loop. When Copperpour was started before KiCad (unresolved startup
endpoint), the wait loop kept polling the empty address until timeout even after the
launched KiCad created its socket.

Two runtime behaviors change:

- `launch_kicad_ui` now resets any active ping backoff after spawning
  KiCad, and with `wait_ready: true` re-resolves the IPC endpoint on each 500 ms
  poll iteration, so it observes the socket created by the launched KiCad without
  restarting Copperpour.
- IPC rediscovery probes on `ToolContext` are now serialized. When several candidate
  sockets exist and none can be chosen (`Ambiguous`), the same set is re-pinged with
  bounded exponential backoff (250 ms doubling up to 2 s); a changed set, a found
  endpoint, `open_board`'s selection or a KiCad launch probes at once. No socket or
  a single one needs no ping and is never backed off, so a KiCad started after
  Copperpour is seen on the next call.

Explicitly configured `ipc_address` and `KICAD_API_SOCKET` values remain authoritative
and never trigger discovery or backoff. No tool arguments or response schemas changed.

### A detected KiCad socket is looked for again on every IPC call (minor release)

When `ipc_address` is not configured and `KICAD_API_SOCKET` is unset, the
server used to detect KiCad's socket once at startup and keep that address for
its whole life. It now looks again on every IPC call, reading file metadata,
so KiCad started or restarted after Copperpour is found without restarting the
server. On Linux and macOS the search also takes every `api-<pid>.sock` beside
`api.sock` whose process is running and that passes the same ownership check,
which is where KiCad 10 listens when a stale `api.lock` blocks `api.sock`.
When more than one socket qualifies, the one that answers an NNG Ping is used;
if not exactly one answers, none is adopted and the "not configured" error and
the startup log list every candidate. That holds on every call: the address
found at startup is never used in its place, and `open_project` then reports
an empty `ipc_address`. Telling several candidates apart costs one Ping each
before the call's own request, each bounded by the 30 s reply timeout, and a
candidate that times out counts as not answering. A configured or `KICAD_API_SOCKET`
address is never replaced. `get_installation_info` reports the address the
calls dial now (`get-installation-info-ipc-endpoint.md`).

### JLCPCB export preserves unserved board binding reasons (minor release)

When `GetOpenDocuments` returns `AS_UNHANDLED` or `AS_UNIMPLEMENTED`, `export_manufacturing_package` with JLCPCB assembly and `generate_jlcpcb_order_link` now report `editor_unavailable` with `recovery.mutation_outcome: "none"` instead of a generic `handler_error`. Neither refusal starts an export. Tool arguments and successful responses are unchanged.

`AS_UNHANDLED` reports `no_pcb_editor_at_endpoint`. `AS_UNIMPLEMENTED` does not establish whether a PCB editor is open. The shared binding formatter includes both statuses in `recovery.evidence.status`, including for other callers that already returned these refusals. The export message now identifies the binding problem and says that no package was exported or uploaded. `error.reason` no longer has the `native_midpoint_unavailable:` prefix for these two cases.

### CLI-backed tools report signal termination (patch release)

On Unix, when a signal kills `kicad-cli`, the failure message reports the signal instead of `exited with -1`. The error kind, diagnostic output and messages for ordinary exit codes are unchanged. Use `isError` and `error.kind` where present, and keep checking tool-specific warnings for incomplete results.

### The `konnect` executable is gone (minor release)

This completes the removal that "Renamed to Copperpour" announced.
Source builds, the standalone archives and the Nix flake no longer include
`konnect`, the shim that ran the `copperpour` beside it, and the flake drops its
`konnect` package and app. An MCP client configuration or hook that starts
`konnect` must name `copperpour` (`copperpour.exe` on Windows) instead. An
archive unpacked over an older one leaves the old `konnect` in place: delete it,
or it keeps such configurations working until the next clean install. A Nix
profile that installed `#konnect` needs `nix profile remove` and an install of
`#copperpour`. The PCM package never shipped it. `copperpour init` still replaces the Claude hooks an
install through the alias wrote, `uninstall` still removes them, and `status`
still reports them as `alias_handler`
([#296](https://github.com/Copperpour/Copperpour/issues/296)).

### `launch_kicad_ui` takes `display`; KiCad health messages name dialogs and `enable_server` (minor release)

`launch_kicad_ui` accepts an optional `display`, a local X display such as
`":0"` or `":1.0"`. KiCad starts with that `DISPLAY`, `GDK_BACKEND=x11`, and
without `WAYLAND_DISPLAY`. Any other value, a remote `host:N` display among
them, is refused as `invalid_argument`, and so is any value on Windows and
macOS. When KiCad exits during startup and the server has neither
`DISPLAY` nor `WAYLAND_DISPLAY`, the error adds a line saying so, and
`launch_kicad_ui` adds that `display` fixes it. The browser viewer's
`POST /open-in-kicad` `503` body adds the same line, and so does
`open_schematic_viewer`'s `browser_launch_error` when the opener failed (not
when it timed out).

When KiCad answers `AS_NOT_READY`, `ipc_failure` (from `check_kicad_ui` and
`open_project`) carries a `note` saying it is still starting or held by a
modal dialog. `launch_kicad_ui` returns that `note` in place of "IPC not yet
responsive" when its wait ends that way. The `no_listener` message names
`api.enable_server` in `kicad_common.json`.

From an SSH session, pass `display`. Treat a lasting `AS_NOT_READY` as a dialog
to close, not a start to wait for.

### `list_custom_rules` (minor release)

A new `verification` tool lists the rules in the board's sibling `.kicad_dru`
custom rules file: `list_custom_rules(board)`. `set_custom_rule` and
`delete_custom_rule` write the file, and `get_design_rules` reads only the
`.kicad_pro`, so until now no tool read it
([#685](https://github.com/Copperpour/Copperpour/issues/685)).

Each rule comes in file order with its `name`, its `line` and its `text`, from
its `(` to its `)` as written. The rules are those `set_custom_rule`'s check of
the file reads, the reading both writers use to find a rule by name, and
`faults` gives what the check finds; kicad-cli drops a rules file it cannot
parse. A fault the check cannot read past, such as a rule never closed, hides
the rules after it; a string left open, or a text variable, which KiCad expands
from the board and the check does not, hides them all. A rule with no name is
not listed; its fault gives its line. kicad-cli is not run, so the property
names the rules use come back unchecked in `unchecked_properties`. The
response also gives `rules_file` and `count`; a board with no rules file gives
`exists: false` and no rules. The tool changes no design file.

No existing tool's arguments or responses change.

### Live placement carries pad properties, zone settings and reference points, and reports `not_carried` (minor release)

Live IPC footprint placement (`place_component`, `place_component_array`, `duplicate_component`, `add_mounting_hole`, `repair_corrupted_footprints`, and added footprints in `update_pcb_from_schematic`) now carries pad zone connections, thermal relief settings, and (on KiCad 10.0.7+) pad fabrication properties and reference points.

When a library footprint includes clauses or children that typed IPC placement cannot carry (or requires KiCad 10.0.7+ on an older KiCad), placement still succeeds (`isError: false`) and reports the omitted items:

- Placement and repair tools include a non-empty `not_carried` array of strings in the JSON response.
- `update_pcb_from_schematic` emits non-blocking `not_carried` entries in `diagnostics`, replacing `pad_properties_not_carried`.

### Live placement and Update PCB carry pad and footprint solder margins (patch release)

[#723](https://github.com/Copperpour/Copperpour/issues/723):

- **Now carried:** a library footprint's own `solder_mask_margin`, `solder_paste_margin` and `solder_paste_margin_ratio` (or the older `solder_paste_ratio`), and each pad's own, by `place_component`, `place_component_array`, `duplicate_component` and `add_mounting_hole` while the board is open in KiCad, and by `update_pcb_from_schematic` when it adds a part. `repair_corrupted_footprints` gives the pads it rebuilds their library margins and keeps the board's footprint margins, as before. Before, the board's values applied and each clause was listed in `not_carried`.
- **No longer in `not_carried`:** those clauses.
- **Now refused:** a margin clause that is not one number, which KiCad's parser does not load either, refuses the footprint in each of these tools, as a malformed `zone_connect` does; Update PCB reports it as `unsupported_library_footprint`. Before, it was listed in `not_carried`.

### `load_toolset` no longer repeats the tools of a toolset that is already loaded (minor release)

Loading a toolset that is already loaded used to return every one of its tools
with its description again. Now the single-name form adds
`"already_loaded": true` and returns `tools` as an empty array, and the array
form lists such toolsets in a new `already_loaded` array and leaves their tools
out of `tools`. `loaded` and `tools_added` keep their meaning: `tools_added`
still counts the tools of every toolset named in `loaded`.

A caller that read tool descriptions from a repeated `load_toolset` result
should take them from `tools/list`, where a loaded toolset's tools already are.

### `load_toolset` returns a `hint` (patch release)

A `load_toolset` reply that adds tools now carries a `hint` string: if the client
still rejects the new tools as unknown, it caches its first `tools/list`, and
`"eager_toolsets": true` plus a client restart makes them callable. A reply that
loads nothing new has no `hint`. Callers that ignore unknown keys need no change.

### `validate_for_manufacturing` and `export_manufacturing_package` put the verdict first and bound their lists (minor release)

Both responses had their keys sorted, which put `validate_for_manufacturing`'s
`verdict` after every issue and the package's `summary` and `warnings` after
its placement evidence. Both grew with the board: one `issues` entry per DRC
error, and the JLCPCB placement evidence with an entry per placed part, carried
twice ([#69](https://github.com/Copperpour/Copperpour/issues/69)).

`validate_for_manufacturing`'s keys now come in this order: `verdict`,
`summary`, `fab_house`, `board_info`, `drc`, `issues_count`, `issues_shown`,
`issues_truncated`, then `issues`. `issues` holds the first `limit`, 50 unless
the call names another; `issues_count`, the verdict and `summary` cover them
all. The issues that are not a single DRC error, such as a DRC category
kicad-cli did not report, now come before the DRC errors, so the bound cannot
hide them. `output` names a file to receive the whole response, every issue
listed, as `run_drc` writes its report. A path that is the board, a directory,
or under something that is not a directory is refused before the board is
read; a write that fails after that check fails the call.

`export_manufacturing_package`'s keys now come in this order: `complete`,
`summary`, `warnings`, `next_steps`, `fab_house`, `output_dir`, `files`,
`files_generated`, `gerber_layers`, `position_units`, `position_side`,
`board_source`, `project_settings_source`, then `placement_orientation`. For a
JLCPCB assembly, the whole placement evidence, the object `placement_orientation`
held, is written to `CPL-<project>.orientation.json` beside the CPL. It is not
in `files`, which lists what to upload; a failed write is a warning, and the
package is incomplete. The file stays in `output_dir` with the other outputs,
and nothing removes it: a later export into the same directory replaces it
only when it writes evidence again, so with another fab house, without
assembly, or after a failed write, an earlier file stays there unnamed. Export
into a fresh directory, as for the CPL and BOM.

`placement_orientation` now holds `status`, `physical_validation`, `note`,
`evidence_path` (that file's path, null when it was not written),
`geometry_count`, `unmatched_footprints_count`, `unverified_footprints_count`,
`applied_corrections_count`, `applied_corrections_shown`,
`applied_corrections_truncated`, `policies`, and the first `limit`
`applied_corrections`, 50 unless the call names another. Read `geometry`,
`unmatched_footprints` and `unverified_footprints` from the file; `warnings`
still names every part they call to review. The pick-and-place entry of
`files_generated` no longer carries its own copy of `placement_orientation`.

`generate_jlcpcb_order_link` repeats the package's `placement_orientation`
without `evidence_path`, since its package is written to a temporary directory
removed when the call returns. Run `export_manufacturing_package` for the file.
A failed evidence write leaves its package incomplete, which it refuses to
upload, as it refuses any incomplete package. It takes no `limit`, and repeats
the first 50 applied corrections.

`validate_for_manufacturing` and `export_manufacturing_package` take `limit` as
`run_drc` does: a non-negative integer, `0` for counts only.

### `modify_trace` changes the track in place (patch release)

`modify_trace` now updates the track it names, so the track keeps its UUID and
`modified_uuid` names a track that is still on the board, and the change is one
KiCad undo step ([#366](https://github.com/Copperpour/Copperpour/issues/366)).
It used to delete the track and create a new one with a new UUID; when KiCad
refused the new one, the old track was already gone.

It now refuses, with the board unchanged, when the UUID is not a trace segment
on the board (`stale_target`; it used to delete whatever item had that UUID),
when the board has no net named `net_name` (`invalid_argument` on `net_name`),
when the board has no such copper layer (`invalid_argument` on `layer`), and
when `width` is not greater than zero (`invalid_argument` on `width`; it used to
be sent to KiCad). If KiCad accepts the update but the track does not read back
as sent, the answer is `ipc_outcome_unknown`: inspect the live board before
retrying.

### `move_schematic_component` accepts `unit` (minor release)

`move_schematic_component` moved the lowest-numbered unit of a component to
`x`, `y` and translated every other placed unit by the same delta, so one unit
of a multi-unit symbol could not be repositioned alone
([#44](https://github.com/Copperpour/Copperpour/issues/44)). The new optional
`unit` moves only that placed unit to `x`, `y`; every other unit stays where the
file has it, and the saved-file readback holds each of them to that. A `unit`
the component has no placement for is refused as `invalid_argument` with the
placed units named, before anything is written. A unit carried by more than one
placement is refused as `ambiguous_target`, as every component edit already
refuses it.

Calls without `unit` behave as before. With `unit`, `moved_units` is 1; the
moved unit's new position is its entry in `placements`, which still lists every
unit. As in every component tool, the top-level `x` and `y` describe the
lowest-numbered unit.

### `move_schematic_objects_between_sheets` moves objects between sheets (minor release)

`move_schematic_objects_between_sheets` is a new tool in `sch_hierarchy`. It moves
UUID-selected symbols, wires, junctions, local and global labels, no-connect flags, text
and text boxes from one sheet of a saved project to another, offset by `offset_x_mm` and
`offset_y_mm`. Each sheet must be placed only once. Symbol and pin UUIDs, references, unit
membership, properties and footprint assignments don't change. Instance paths change to
the destination sheet.

- It defaults to a dry run. To apply, pass the dry run's `plan_revision` as
  `expected_plan_revision`. A missing revision is `invalid_argument`. Apply rebuilds and
  checks the plan again, so failed validation can return `plan_blocked`. If the plan
  revision no longer matches, including because the verification result changed, apply
  returns `stale_target`. None of these writes anything.
- Without `kicad-cli`, a dry run returns the plan with `verification.status: "not_run"`
  and apply refuses with `plan_blocked`. A check that can't be read is a refusal, not a
  pass.
- The two sheets may sit under different top-level schematics of one project. A sheet path
  is the top-level instance UUID followed by child sheet-symbol UUIDs; use the
  project-declared top-level UUID. A lone top-level sheet whose declared UUID differs from
  its file's UUID is refused. Projects with several top-level schematics need KiCad 10.0.7
  or newer.
- It doesn't write the PCB. It refuses a move that would leave a footprint path stale,
  reuse a path or net name the PCB already has, change an effective netclass or inherited
  simulation exclusion, or need dependent net names edited. A net rename is also refused
  when the project has a netclass pattern other than `*`, because Copperpour can't prove
  the patterns still match, or custom PCB rules, because the schematic checks don't
  evaluate them.
- It checks every project file it read, and that files it expects to be missing still are,
  before it writes and again during recovery.
- A failure before the journal is written is a refusal and changes nothing:
  `invalid_argument`, `stale_target`, `unsafe_file_fallback`, `conflict` or
  `plan_blocked`. An inspected older unfinished journal is a `conflict` that names it. A
  failure after that is `mutation_outcome_uncertain`. It names the journal and, when
  available, the file where the write stopped, or, if the move finished but reading it
  back didn't match, the transaction ID. Run `copperpour transaction status` on the
  project directory, and `copperpour transaction recover` only if a journal remains.
  Recovery checks that each sheet matches its contents before or after the move, writes
  only sheets still in their before state, and requires the other read files unchanged,
  expected-missing files still missing and no KiCad editor lock present.
- Its journals record files that must stay missing, so recovering one needs a Copperpour
  version that reads version 2 journals. Recover any pending journal before downgrading.

See [cross-sheet transfer][move-objects-guide] for the refusals and the recovery steps.

[move-objects-guide]:
  https://github.com/Copperpour/Copperpour/blob/main/docs/CROSS_SHEET_TRANSFER.md

### Native Specctra export no longer runs Rust-only geometry checks first (minor release)

`export_specctra_dsn` keeps `native_bridge_mode: "disable"` as the Rust-only
default. All modes first validate the stable live IPC snapshot and shared
reverse-manifest/import requirements. On KiCad 10, `require` then attempts the
authenticated native bridge and refuses with a native-path reason if unavailable
or semantically invalid, without running or falling back to the Rust writer.
`prefer` also attempts native first; it uses Rust only if native is unavailable
or invalid, and only if Rust-specific checks pass. A refused fallback reports
the native diagnostic as well as the Rust reason.

The native correlator now uses identities built directly from the validated
snapshot model. The Rust writer alone requires a single closed outline of
Edge.Cuts lines and circular arcs (flattened within `max_error`). KiCad-native
circular Edge.Cuts cutouts can therefore export through `prefer`/`require`.
Native output is still checked against components, pads, nets, layers/types,
routing classes/vias and planes, and the live source revision is rechecked after
receiving a native DSN. Source/hash binding, non-replacing paired publication
and SES validation/apply are preserved.

The native manifest’s `supported_profile.outline` now truthfully labels a
KiCad-native boundary rather than Rust flattening. Manifest schema 4 and older
supported schemas remain readable; this is a label change, not a schema change.
Existing method strings, counts, paths and response keys remain unchanged.

Shared restrictions still apply: representable SMD and all-copper through-hole
pads (including the current per-layer geometry limitation), supported copper stack and zones, locked-routing
inventory, complete effective rules, no rule areas and no custom rules. The
`.kicad_dru` presence refusal now names its SES-import reason explicitly; #75 owns
future custom-rule support. This does not add KiCad 11 native export, repeated-pad
native adoption (#386), or relax existing native parser limitations (#98).
Native export remains an explicit opt-in through the KiCad 10 legacy plugin.

### `get_teardrop_settings` and `set_teardrop_settings` read and set native pad and via teardrops (minor release)

Two new tools in the `pcb_copper` toolset work on KiCad's own pad and via teardrop settings on the board open in KiCad ([#555](https://github.com/Copperpour/Copperpour/issues/555)). Both need KiCad 10.0.7 or newer, read from the bound editor; older or unknown versions get `unsupported_capability` before anything is read or written.

- `get_teardrop_settings(board, references?, nets?, kinds?, limit?)` lists each pad and via with its UUID, reference, pad number, net, position and settings, plus the board's teardrop zones by net and `legacy_teardrops`. `references` selects pads only. `total` and `truncated` say when `limit` cut the list.
- `set_teardrop_settings(board, targets, settings, refill_zones?, dry_run?)` changes the settings of the selected pads and vias in one KiCad undo step, without saving. `targets` is the union of `pads` (`{reference, pad_number}` or `{uuid}`), `vias` (UUIDs), `references` (their pads), `nets` (pads and vias), `all_pads` and `all_vias`; a selector that matches nothing refuses the whole call. `settings` takes `enabled`, `curved_edges`, `allow_multiple_track_segments`, `prefer_zone_connection`, `max_length_mm` and `max_width_mm` (`null` for no limit), `best_length_ratio` (0.2 to 2), `best_width_ratio` (0.6 to 1) and `max_track_width_ratio` (0 to 1); omitted fields keep their value.

KiCad rebuilds the edited items' teardrops in the same undo step. `refill_zones` defaults to `false`; set it to request a board-wide refill afterwards. The response reports `targets` and `verified_changes` from settings readback separately from `copper.commit` and `copper.refill`. The latter describes teardrop copper observed after the refill request once the board accepts reads; `completion_verified: false` means that observation does not prove the queued fill finished. If the refill request or subsequent read fails, the error retains the confirmed settings and commit evidence under `applied`.

Boards in legacy teardrop mode, and pads of a footprint that owns groups, are refused with `plan_blocked` and nothing written: pads are written inside their whole footprint, which would drop such a group.

### Net queries refuse a name nothing on the sheet spells (patch release)

`get_net_connections`, `get_net_connectivity` and `get_net_components` now
return a structured `invalid_argument` error on `net` when no label or power
symbol on the sheet spells the name asked for. Previously they answered it as a
net with nothing on it: no pins, labels, wires or components, and no error.
Where a name on the sheet differs only in case, or by the sheet path KiCad's
netlist puts before a local label's name (`/SDA` for `SDA`), the reason offers
it. A net no label or power symbol names has no name these tools take; reach it
with `get_connected_items` or `trace_from_point`. Arguments, and the responses
for a name on the sheet, are unchanged. Issue:
[#101](https://github.com/Copperpour/Copperpour/issues/101).

### Net queries report each label's uuid (patch release)

`get_net_connections`, `get_net_connectivity`, `get_connected_items` and
`trace_from_point` now add `uuid` to each net, global or hierarchical label
they return, the UUID `list_schematic_labels` lists and `batch_delete` takes.
An entry of type `PowerSymbol` has no `uuid`: it stands for a power symbol's
pin, and the symbol's own UUID would delete the symbol. `get_net_connectivity`'s
wires also carry `uuid`, as `get_connected_items`' wires and
`list_schematic_wires` do. Every existing field is unchanged. Issue:
[#93](https://github.com/Copperpour/Copperpour/issues/93).

### Schematic placement and rotation normalize orientation to KiCad's canonical pair (patch release)

`add_schematic_component`, `batch_place_components`, `add_power_symbol`, and `rotate_schematic_component` now normalize each symbol's `(rotation, mirror)` pair to the canonical form eeschema writes (`180° x` -> `0° y`, `180° y` -> `0° x`, `90° y` -> `270° x`, `270° y` -> `90° x`, and quarter-turn angles folded into `[0, 360)`), matching `mirror_schematic_component`.

The drawn body, pins, and field text are unchanged, and KiCad's next save no longer rewrites the stored pair. Callers that chain `rotate_schematic_component` should read the stored `rotation`, `mirror_x`, and `mirror_y` from the tool response rather than assuming the requested pair was stored verbatim.

### New tool `open_board` in `verification` (minor release)

`open_board(board, display?, timeout_seconds?)` opens a `.kicad_pcb` in
KiCad's PCB editor and waits until a KiCad IPC endpoint reports that exact
board, as asked on [#23](https://github.com/Copperpour/Copperpour/issues/23).

- **Looks first:** every endpoint detection chooses among, or the configured
  one alone, is asked for its open documents and then a read of the board.
  One holding the board is used, and no editor is started.
- **Starts:** otherwise `pcbnew <board>` of the KiCad installation Copperpour
  resolves, and waits for it. `kicad -f pcb` is not used: it exits on a
  segmentation fault in KiCad 10.0.8.
- **Then:** when several sockets answer, as a Project Manager beside a PCB
  editor does, the calls that follow use the one the last `open_board` found
  holding its board, while it is among them; `get_installation_info` reports
  it as `source: "selected_by_open_board"`.
- **Answer:** `requested_board`, `requested_open`, `launched`, `ipc_address`,
  `timed_out`, `timeout_seconds`, `elapsed_ms` and `endpoints`, each with
  `ipc_address`, `requested_open` and `error`; a `note` when none reported
  the board in time.
- **No editor is started** while an endpoint answers `AS_NOT_READY` or gives
  no clear answer (a request timed out, or a reply that does not parse), lists
  the board without serving a read of it, or has a request still running;
  while KiCad's lock beside the board is present or cannot be checked,
  refused as `conflict` with the lock in `paths`; or when a configured
  `ipc_address` or `KICAD_API_SOCKET` answers without the board or is not
  KiCad's own socket, refused as `editor_unavailable`. Each says what to do
  instead.
- **Two endpoints holding the board:** refused as `ambiguous_target`, the
  board as `target` and the endpoints as `candidates`; neither is used.
- **Windows:** detection looks at the default pipe alone, so an editor started
  beside a running KiCad is not found there, and the call times out.

### A pin on a net no label names reports KiCad's name for that net (patch release)

`get_component_nets`, `get_pin_connections` and `get_pin_net_name` answered
`net: null` for a pin on a net that no label, power symbol or sheet pin names,
the answer they give a pin on nothing. They now give the name KiCad's netlist
gives that net after one of its pins, such as `Net-(R1-Pad2)`, `Net-(U1-VIN)`
or `Net-(U2A-+)`, `unconnected-(…)` when a no-connect flag is on it, and a
hidden `power_in` pin's own name, as KiCad 10.0.8 does, whose unannotated
pins drive their nets. `null` is left for a pin KiCad nets alone, for a pin
whose net a sheet pin carries to the sheet below, and for a pin on a net KiCad
names after an unannotated symbol's pin, whose name carries a hash of that
symbol's UUID. A name a label
gives is unchanged, and the tools that take a net by name still take only
those. Issue: [#618](https://github.com/Copperpour/Copperpour/issues/618).

### `place_component` places Reference and Value as the library has them (patch release)

Placed live, a footprint's Reference and Value fields were built without the
library's text style: no stroke width, a line spacing of 0, not kept upright,
both on the silkscreen, and the Value hidden
([#157](https://github.com/Copperpour/Copperpour/issues/157)). They now carry
the library's layer, visibility, font, stroke, line spacing, justification and
keep-upright, placed as `update_pcb_from_schematic` places the footprints it
adds.

A library footprint whose properties cannot be read, such as one that writes
a property twice, is still placed with the default fields, and the response
then has a `warning` saying so. Placement into a closed board file is
unchanged.

### Placed symbols take the library symbol's exclusions (patch release)

`add_schematic_component`, `batch_place_components`, `apply_template`,
`add_power_symbol` and `batch_add_power_symbol` copy the library symbol's
`exclude_from_sim`, `in_bom`, `on_board` and `in_pos_files` onto the symbol
they place, as eeschema does. A `Mechanical:MountingHole`, `(in_bom no)` in
KiCad's library, is now placed out of the BOM; it used to be placed with
`(in_bom yes)`. A library symbol that sets all four, as KiCad 10 writes them,
now also gives its placed symbol `(exclude_from_sim …)` and `(in_pos_files …)`.
A flag the library omits keeps its default. Issue:
[#124](https://github.com/Copperpour/Copperpour/issues/124).

### Live placement gives a footprint its library's Datasheet, Description and custom properties (patch release)

`place_component`, `place_component_array`, `duplicate_component` and `add_mounting_hole` (with a library footprint), placing while the board is open in KiCad, now give the footprint every field its library footprint has: Datasheet and Description where the library has them, and every custom property, such as `KiLib_Generator`. They are placed as `update_pcb_from_schematic` places them on the footprints it adds. Before, these fields were left out without saying so, and a library refresh dry run right after placing reported `metadata` changed.

When the library's properties cannot be read, the part is still placed with default Reference and Value fields, and the existing warning and `not_carried` entry now also say that Datasheet, Description and custom properties were left out. Tool inputs and response fields are unchanged; closed-board placement already copied the whole library footprint. Callers need not change their calls.

### Placement refuses a numbered designator already in use (minor release)

`add_schematic_component`, `batch_place_components` and `apply_template` wrote a
placement under a numbered designator that another placement of the project
already held, on the same sheet or on another sheet of the hierarchy, and
reported success. eeschema's annotation check reports the pair as duplicate
items, and `kicad-cli sch export netlist` warns that the schematic has
annotation errors.

Such a placement is now refused as `invalid_argument` on `reference`, or on
`ref_start` for `apply_template`, naming the placements that hold the
designator (three, and a count of any more), and nothing is written for it.
Only the other units of one multi-unit part, with one value, share a
designator, as `annotate_schematic` keeps one; this also refuses units of two
different parts with one value and unit count, which eeschema does not report.
An unannotated designator (`R?`, the default) is never refused.
`batch_place_components` refuses only that entry, an earlier entry of the same
batch counting as a holder, and places the others; `apply_template` refuses the
whole call. Omit the reference, or give one no placement holds.
`apply_template` with `ref_start` left out still numbers from one above the
highest number on its own sheet, so on a sheet of a hierarchy it can now be
refused; give a `ref_start` no placement holds.

The other sheets are read as `add_power_symbol` reads them for its numbers; a
sheet no project owns is checked against itself alone. A sheet file that could
not be read is named in `other_sheets_unreadable` in the reply of a call that
places, present only then, and its designators were not checked. A placement
on a sheet placed more than once holds its designator once, though each
instance record carries it, as before. Issue:
[#615](https://github.com/Copperpour/Copperpour/issues/615).

### Power symbols take the library's reference prefix, so `PWR_FLAG` is `#FLG` (patch release)

`add_power_symbol` and `batch_add_power_symbol` gave every symbol they placed a
`#PWR` designator. KiCad's `power:PWR_FLAG` has the `Reference` `#FLG`, and
eeschema numbers `#FLG01`, `#FLG02`, … apart from `#PWR`, so a flag disagreed
with the library definition the tool had just embedded and used up a `#PWR`
number.

The prefix is now the embedded library symbol's `Reference`, as eeschema takes
it when it places a symbol, and each prefix is numbered on its own: the lowest
number free among that prefix's designators across the hierarchy. A
`PWR_FLAG` gets `#FLG01` and a `GND` placed after it still gets `#PWR01`. A
library symbol with no `Reference` prefix still gets `#PWR`. Flags already
placed as `#PWR` are left as they are, and their numbers stay held in the
`#PWR` series. Issue:
[#110](https://github.com/Copperpour/Copperpour/issues/110).

### Power symbols are numbered across the whole hierarchy (minor release)

`add_power_symbol` and `batch_add_power_symbol` gave a new `#PWR` the lowest
number free on the one sheet file, so a number already held on another sheet of
the project was issued again, and a sheet placed more than once got the same
designator on every instance. eeschema checks annotation over the whole
hierarchy, power symbols included: the netlist export warns that the schematic
has annotation errors.

The number is now the lowest free among every `#PWR` this sheet holds and the
project's instance records on every other sheet file of the hierarchy, read as
`annotate_schematic` reads them. Each instance of a sheet placed more than once
gets its own number, in hierarchy order as eeschema numbers its pages, and the
`Reference` property carries the one eeschema saves for that file, the record
of its first instance in page order, so the lowest. A compact reply gives that
property; the per-instance numbers are in the file and in the `verbose` reply's
`instance_references`. A batch numbers each symbol after the previous one.

A sheet no project owns is numbered against itself alone, as before. The reply
now has `other_sheets_consulted` and `other_sheets_unreadable`, the keys
`annotate_schematic` uses. A sheet file that could not be read is named in
`other_sheets_unreadable`, and its numbers were not seen. When the hierarchy's
page order could not be loaded, the file it stopped at is named there too, the
other sheets' numbers are still taken, and the instance paths are numbered in
sorted path order instead of page order. Issue:
[#505](https://github.com/Copperpour/Copperpour/issues/505).

### A failed readback after a write is `mutation_outcome_uncertain` (patch release)

A file tool whose write replaced the file, but whose readback then found
something else there (another program saved it in between), now answers
`mutation_outcome_uncertain` with the tool's name as `operation`. It used to
answer as if nothing had been written: `conflict` from `set_footprint_metadata`,
`set_footprint_graphics`, `set_footprint_models` and `copy_routing_pattern`,
`stale_target` from `delete_schematic_component`, `batch_delete`,
`batch_delete_schematic_components`, `enrich_datasheets` and `fix_connectivity`,
and a plain or `handler_error` message, "write conflict: … changed since it was
read", from the others.

`conflict` and `stale_target` now mean nothing was written, so a retry is safe.
On `mutation_outcome_uncertain`, reload the file and inspect it before retrying.

### `update_footprints_from_library` keeps field text and fabrication attributes by default (minor release)

A refresh used to write the library's Datasheet, Description and custom
property text over the placed footprint's, and the library's mounting type and
BOM and position-file exclusions over its attributes. It now keeps them, as
KiCad's Update Footprints from Library dialog does by default
(`dialog_exchange_footprints.cpp`, update mode), and offers two optional
booleans beside `reset_text_sizes_and_styles` and `reset_text_positions`, each
mapped onto one of the dialog's checkboxes:

- `reset_text_content` ("Update text content") gives Datasheet, Description
  and each custom property the library also has the library's text.
- `reset_fabrication_attributes` ("Update fabrication attributes") gives the
  footprint the library's mounting type and BOM and position-file exclusions.

Both are `false` by default. DNP and "not in schematic" are kept either way,
as before. Custom properties the library has still take the library's
position, layer and style; only their text is kept.

What a dry run plans changes: a footprint that differs from its library only
in that text or those attributes is now `unchanged`, so its plan revision
differs too. A caller that relied on a refresh setting them back passes
`reset_text_content: true` and `reset_fabrication_attributes: true`. An unset
flag is left out of the plan revision, so calls that set neither keep their
revision inputs.

### `update_footprints_from_library` carries solder margins and takes `reset_clearance_overrides` (minor release)

[#723](https://github.com/Copperpour/Copperpour/issues/723):

- **Now carried:** a library footprint's own `solder_mask_margin`, `solder_paste_margin` and `solder_paste_margin_ratio` (or the older `solder_paste_ratio`), and each pad's own, by `update_footprints_from_library`, which refused such a footprint by name ("pad clause 'solder_mask_margin' is not supported by typed library refresh"), and by `update_pcb_from_schematic` when it replaces a footprint.
- **Pads:** a refreshed pad takes its library pad's margins, as KiCad replaces every pad, so a pad whose margins differ plans `pads`.
- **New argument:** `reset_clearance_overrides`, a boolean, `false` by default like the other reset flags, is KiCad's "Update/reset clearance overrides", which KiCad's dialog checks by default. Set, it gives each footprint the library's own clearance, zone connection, solder mask and paste margins and solder mask bridge setting; the refresh refuses a library footprint with its own clearance or zone connection, so those two become none and inherited. A footprint whose clearance, zone connection or margins change plans the new `changed_domains` value `clearance_overrides`. Left unset, it is left out of the plan revision.
- **Changed by default:** a refresh keeps the placed footprint's solder mask bridge setting unless `reset_clearance_overrides` is set, as KiCad does with the option unchecked. Before, it took the library's and planned `attributes`.
- **What to do:** to refresh as KiCad's dialog does by default, pass `reset_clearance_overrides: true`. It takes the library's bridge setting, as a refresh did before, and also its clearance, zone connection and margins, which a refresh kept before; no argument takes the one without the other.

### New tool `rename_footprint` in `library_maintenance` (minor release)

`rename_footprint(library_path, footprint_name, new_name)` renames a footprint
in its `.pretty` library, as KiCad's footprint editor does, and as agreed on
[#633](https://github.com/Copperpour/Copperpour/issues/633).

- **Write:** `new_name.kicad_mod` holds the footprint with the new name in its
  header, and the Value changed when it equals the old name; then
  `footprint_name.kicad_mod` is removed. Nothing else in the file changes. A
  rename that changes only the case renames the one file, then rewrites it.
- **Refused** as `invalid_argument` on `new_name`, with nothing written: a name
  already in the library, or one that differs from a footprint in the library
  only in case; an empty name, one with spaces at either end, or one with a
  character KiCad does not accept in a footprint name (`% $ < > " \ / :`, tabs
  or line breaks); on Windows also `* ? |` and device names such as `CON` or
  `NUL`. `footprint_name` not in the library is refused on `footprint_name`. A
  path that is not a `.pretty` directory is a handler error; a missing one is
  `file_not_found`.
- **Concurrent change:** if the old file changes before it is removed, the new
  file is removed again and the call answers `conflict`; in a case-only
  rename, the file gets its old name back. A rename interrupted after writing
  the new file, or a case-only one interrupted after renaming the file, is
  finished by the next call (`finished_earlier_rename`), with the same `value`
  as an uninterrupted rename.
- **Answer:** `previous_name`, `name`, `footprint_path`, `value` (`previous`
  and `current`) when the Value changed, and `kept_references`: symbols'
  Footprint fields and footprints placed on boards keep the old library id.

### New tool `rename_symbol` in `library_maintenance` (minor release)

`rename_symbol(library_path, symbol_name, new_name)` renames a symbol in its
`.kicad_sym` library, as KiCad's symbol editor does, and as agreed on
[#634](https://github.com/Copperpour/Copperpour/issues/634).

- **Write:** the symbol and its unit blocks (`NAME_<unit>_<body style>`) take
  the new name, symbols derived from it extend the new name, and its Value
  changes when it equals the old name. The name is stored as KiCad stores it
  from its rename dialog: `\ < > : "` escaped as `{backslash}`, `{lt}`,
  `{gt}`, `{colon}`, `{dblquote}`, line breaks dropped. A library in KiCad's
  save order (inheritance depth, then name) keeps it: the symbol moves to its
  place. A library in another order keeps that order.
- **Refused** as `invalid_argument` on `new_name`, with nothing written: a
  name already in the library, the symbol's own name, an empty name, one with
  spaces at either end, or one with a tab. `symbol_name` not in the library is
  refused on `symbol_name`. A file that is not a `.kicad_sym` is a handler
  error, as is a unit block not named after its symbol; a missing file is
  `file_not_found`.
- **Answer:** `previous_name`, `name` (as stored), `escaped` when the stored
  name differs from `new_name`, `value` (`previous` and `current`) when the
  Value changed, `derived_symbols` whose `extends` changed, `moved` when the
  symbol moved, and `kept_references`: symbols placed in schematics keep the
  old library id.

### Inline renders arrive as image content, not a base64 string (minor release)

`render_schematic_png` with `inline: true` no longer adds `png_base64` to its
JSON report, and `compare_visual_baseline` with `inline_diff: true` no longer
adds `current_png_base64`. The report stays the first content block, and the
PNG follows as a second block of type `image` (`mimeType` `image/png`), the
form `get_board_2d_view` already used. Read the image from that block; the PNG
file named in `rendered` is unchanged.

### `repeat_layout` copies the routing in the reference placement's area (minor release)

`repeat_layout` copied the routing on the reference placement's own nets, those
with a pad on it and none elsewhere, and replaced a target's routing on the nets
the copies took.

It now copies the tracks, arcs and vias that lie in the reference placement's
area, as KiCad's Repeat Layout copies what lies in a rule area. The area is the
outline KiCad's Generate Placement Rule Areas draws: the boxes of the
placement's footprints, of every item in the groups they are in, and of the
routing on its own nets, 0.1 mm out, around their convex hull in horizontal and
vertical edges. Routing on a net the placement shares, such as GND or a net that
reaches another sheet, is now copied when it lies in the area. A copy takes the
net of the target's matched pads, or keeps its own when no pad of the placement
is on it. The copies replace the routing that lies in the target's own area or
in the reference's area carried onto the target, whatever its net; an item the
reference copies is never removed. A net local to the reference must still
become one net local to the target, and every copied net one net there, or the
call is refused as `plan_blocked`. A closed board whose area needs the box of an
item the saved file cannot measure, such as a text in a group, is refused as
`plan_blocked` naming it. `routing.targets[].nets` lists every net a copy goes
on.

`copy_routing: false` keeps the footprint-only behaviour. Issue:
[#608](https://github.com/Copperpour/Copperpour/issues/608).

### `repeat_layout` copies the reference placement's routing (minor release)

`repeat_layout` placed footprints only; a target placement's tracks and vias
were the caller's to draw.

It now copies the reference placement's tracks, arcs and vias onto each target
placement, turned and moved as its footprints are. It copies the routing on the
placement's own nets, those with a pad on it and none elsewhere; KiCad's Repeat
Layout copies what lies in a rule area instead. Each copy takes the net of the
target's pads matched to that net's pads, and the copies replace the target's
own routing on those nets. A net whose routing is copied and that does not
become one net of the target's own is refused as `plan_blocked`, with a reason
for each, and nothing moves. The response adds `routing`: `copied_count`,
`removed_count` and, for each target, `target_reference`, its counts and the
`nets` it mapped. On a live board the routing joins the placements' one KiCad
commit, the copper check leaves out the routing that commit removes, and a copy
not confirmed on its net after the commit is listed in `routing.unconfirmed`
with a `warning`. On a closed board whose tracks name their net by number, as
boards before KiCad 10 do, the call is refused unless `copy_routing` is false.

`copy_routing: false` keeps the footprint-only behaviour. Issue:
[#608](https://github.com/Copperpour/Copperpour/issues/608).

### New `repeat_layout` places every other placement of a sheet as one is placed (minor release)

A sheet placed more than once took a `set_component_placements` batch whose
poses the caller worked out: each footprint's counterpart's pose, turned and
moved by the change between two anchor footprints.

`repeat_layout`, in the `placement` toolset, takes an `anchor_reference`, a
footprint on the placement already laid out. Every other footprint of the
anchor's symbol anchors a placement to lay out, or only those named in
`target_references`. Footprints are matched by symbol instance path over the
anchor's sheet and the sheets below it, and each takes its counterpart's pose,
turned by the two anchors' difference in rotation and moved onto its own
anchor, with the arithmetic of KiCad's Repeat Layout. A placement whose footprints
differ from the anchor's in number, library footprint, pad count or side is
refused as `plan_blocked`, with a reason for each footprint, and nothing
moves. Its arguments `dry_run` and `allow_copper_conflicts` and its response
are `set_component_placements`'. Routing, zones and other board items are not
copied. Issue: [#608](https://github.com/Copperpour/Copperpour/issues/608).

No existing call changes.

### Live net readers require a `GetNets` response payload (patch release)

`get_nets_list` now returns a structured `editor_unavailable` error naming the
requested board when KiCad supplies no net response payload, instead of reporting
`count: 0` and `nets: []`. Malformed responses also return this query failure.
Retry the live query after checking KiCad; no saved board is substituted.
An explicitly present, empty `NetsResponse` still returns a successful empty list.
Successful response fields and tool arguments are unchanged.

The IPC `get_nets` and `get_nets_in` methods now require a response, just like
`get_nets_required_in` (retained as an alias). Their consumers propagate a missing
payload through their existing query/planning failure paths rather than deriving
empty net tables, unknown-net decisions, or empty routing rules. In particular,
`get_netclasses` refuses the failed live read without saved-file fallback.
`get_board_info` retains its existing fallback policy for failed live queries;
this change does not migrate that tool's source selection.

### Standalone Tauri schematic viewer retired (minor release)

The standalone `schematic-viewer` / `schematic-viewer.exe` command and its
`--kicad-cli` flag are removed. There is no shim or Tauri fallback. Use the
`open_schematic_viewer` MCP tool, which already runs the browser service embedded
in the MCP server. Configure the server’s `kicad_cli` when automatic KiCad
CLI discovery is unsuitable. `open_browser: false` returns the private URL
without launching a browser; opener failure leaves a successfully started
service available. See [the MCP contract](browser-viewer-mcp.md) for response
fields, lifetime, saved-file behavior and headless forwarding.

The earlier plan retained Tauri for one shipped deprecation release. The
[owner explicitly waived that grace period](https://github.com/Copperpour/Copperpour/issues/242#issuecomment-6825001324)
after the MCP default moved to the browser service in #439. This retirement
supersedes the retention text in the earlier phase migration notes.

`crates/schematic-viewer`, its lockfile, frontend source path, platform icons,
Tauri configuration and separate `viewer` CI job are removed. Browser assets
now live in `crates/copperpour-viewer/frontend` and are embedded at build time.
HTTP paths and authentication/sandbox behavior are unchanged. README uses the
shared PCM logo. No GUI runtime is needed by the server; rendering still needs
`kicad-cli`, and viewing needs a browser. KiCad itself retains its own native
runtime requirements.

PCM and release builds package only the MCP server, which includes the viewer.
Remove `--viewer PATH` from `packaging/build-pcm.sh` calls and `-ViewerPath PATH`
from `packaging/build-pcm.ps1` calls; these retired arguments explicitly refuse
rather than silently dropping an expected binary. PowerShell retains its old
parameter slot solely for this refusal, preserving other positional meanings.
Old PCM archives remain readable by the merge/validation tooling. Existing
installs may still contain an old viewer executable; it is no longer launched
by the MCP tool. Update scripts or shortcuts that call it directly.

Tauri-only GTK/WebKit development dependencies and universal viewer builds are
removed from release CI and the Nix development shell. Workspace tests cover the
engine/service; browser execution checks remain separate. The live required
`Schematic viewer` check and Local CI configuration need maintainer coordination
when this PR lands; deleting its workflow does not itself edit branch rules.

### A file that changed after a tool read it is a `conflict`, not `handler_error` (patch release)

When a file changes between a tool's read and its write, the writer refuses
before replacing it, so the file holds only the other program's save. Tools that
passed the writer's error on unchanged, such as `add_schematic_net_label`,
`add_no_connect`, `batch_add_wire`, `batch_connect_to_net`,
`move_labels_by_offset`, `edit_footprint_pad` and `delete_symbol`, answered
`handler_error` ("write conflict: … changed since it was read", or "loaded").
The closed-board footprint edits (`move_component`, `rotate_component`,
`flip_component`, `set_component_placements`, and `place_decoupling` and
`force_directed` through them) answered it as an untyped error. They now answer
`conflict` with `paths` naming the file, as the tools that refused it themselves
already did: reload the file and reapply the change. A multi-file transaction's
`TransactionConflict` is unchanged.

`create_schematic` and `add_power_symbol` write the file once before their last
write, so a failure of that last write, including a save or KiCad's editor lock
found only then, is now `mutation_outcome_uncertain`, not `handler_error` or
`conflict`: reload and inspect the file before retrying. Issue:
[#480](https://github.com/Copperpour/Copperpour/issues/480).

### `rotate_schematic_label` and `batch_rotate_labels` take a label's `uuid` (minor release)

`rotate_schematic_label`, and each `batch_rotate_labels` item, names its label
by `uuid` alone, as `list_schematic_labels` reports it, or by `net` with `x`
and `y` as before, as `rename_schematic_net_label` names one label. Of two
labels of one name on one anchor, a position finds only the first; each can now
be rotated by its uuid. The schema now requires only `rotation` beside
`schematic`; the handler checks the rest.

A `uuid` given with `net`, `x` or `y` is `invalid_argument` on `uuid`; neither
`uuid` nor `net` is `invalid_argument` on `net`, `give uuid, or net with x and
y`; `net` with neither `x` nor `y` is `invalid_argument` on `net`, `goes with x
and y`; a missing `x` or `y` is `missing or not a number` on it, where the
schema gate refused a missing `net`, `x` or `y` as a required property. A batch
item's refusal names its field, such as `labels[1].uuid`. A uuid that is no
label's on the sheet is refused, `No label with uuid '…' in this schematic`,
in a batch with `labels[i]:` before it, and nothing is written. Two items
finding one label, by uuid, by position or one of each, are refused as two
found by position are. A call that names its labels by net and position, each
with its `x` and `y`, behaves as before.

Issue: [#747](https://github.com/Copperpour/Copperpour/issues/747).

### `rotate_schematic_label` reports the justify it writes (patch release)

`rotate_schematic_label`'s reply carries `justify` as the direction it
writes, `"left"` or `"right"`, as it did before
[#516](https://github.com/Copperpour/Copperpour/pull/516). Since then it was
the byte range of the label's old justify within the label's block, such as
`[88, 109]`, or `null` for a label with no justify. What the tool writes to
the file is unchanged.

Issue: [#746](https://github.com/Copperpour/Copperpour/issues/746).

### `route_differential_pair` creates the pair in one commit (patch release)

`route_differential_pair` now sends both traces in one `CreateItems` inside one
KiCad commit, so a routed pair is one undo step, and when KiCad refuses either
trace the commit is dropped and neither is left on the board
([#367](https://github.com/Copperpour/Copperpour/issues/367)). It used to create
them one after the other, each its own undo step, and a failure creating the
negative trace left the positive one behind.

`net_neg` equal to `net_pos` is now refused with `invalid_argument` on
`net_neg` before KiCad is asked anything; it used to route two traces of one
net. A failure is now reported in its own words; only an unreachable KiCad, or
one holding no board, still reads "KiCAD must be running". KiCad refusing to
open the commit, while busy or with one of this client's already open, fails
the call before anything is created. A create, commit or drop KiCad does not
confirm is `ipc_outcome_unknown`: inspect the live board before retrying. The
arguments and the successful response are unchanged.

### `route_specctra_dsn` keeps the routed board when its diagnostics log cannot be written (minor release)

`route_specctra_dsn` refuses to replace `<ses stem>.freerouting.log`, and it
already refuses before routing when that file exists. A file of that name
that appeared during the run made the call fail with only "Freerouting
diagnostics output already exists" once routing was over: the routed board was
left behind as a hidden temporary SES next to the requested output, and a
failed run's own cause was not reported.

The routing now decides the call:

- A routed board is written to `ses_output_path` as usual, no temporary SES
  stays behind, and the existing log is left untouched.
  `artifacts.diagnostics_path` is `null` and the new
  `artifacts.diagnostics_error` says why Freerouting's stderr was not saved.
  It is `null` whenever the log was written or there was nothing to write.
- A failed run reports its own cause first, then "(Freerouting stderr not
  saved: …)", then the end of Freerouting's stderr as before.

Callers that treated the log error as a routing failure should read `success`
and, if they need the log, `artifacts.diagnostics_error`.

### `route_specctra_dsn` takes `neck_to_min_track_width` (minor release)

A Specctra DSN only carries the net class widths, so Freerouting never routes
a trace narrower than its class, even where the board's minimum track width
would let a connection through. The new optional boolean
`neck_to_min_track_width` (default false) sends the KiCad project's
`min_track_width` as Freerouting's `neck_width_um`: a connection that fails at
its class width is retried at that width.

- Freerouting narrows the whole connection, on any net, power and mains
  classes included, and KiCad's DRC does not flag a track narrower than its
  net class. Check the widths of power nets after routing.
- The width is sent as if the caller had set `router_settings.neck_width_um`:
  if Freerouting does not apply it, the run is refused before routing, like
  any other requested setting, rather than listed in
  `project_rules.not_applied`.
- An explicit `router_settings.neck_width_um` wins, and nothing is then read
  from the project.
- With the argument set, a call is refused with `invalid_argument` on
  `neck_to_min_track_width`, before routing, when no project is read (none or
  several found, see `project_path`) or when its `min_track_width` is absent
  or not above 0.

The answer's `project_rules` object has a new `neck_width_um`: the width sent
from the project, in um, or null.

`min_track_width` is not sent by default: calls without the new argument route
as before.

### Routing refuses missing live nets and disabled copper layers (patch release)

`route_trace`, `route_pad_to_pad`, and `route_differential_pair` now validate the
requested copper layer and nets on the bound live board before creating tracks.
`add_via` also validates its requested net before creating the via. A missing net
returns `invalid_argument` naming `net_name`, `net_pos`, or `net_neg`; a disabled
copper layer names `layer`. Previously these cases were reported as KiCad being
unreachable, and a missing negative differential-pair net could leave the positive
trace behind. Both pair nets are now checked before creating either trace.

Choose an existing live-board net and enabled copper layer, then retry. IPC read
failures, missing net response bodies, and empty enabled-layer evidence remain
failures, not evidence that a requested net or layer is missing. The IPC client
adds `get_nets_required_in`; existing `get_nets_in` fallback behavior is unchanged. No arguments or
successful response formats change. This preflight does not make separate IPC
creation calls atomic against later errors or intervening editor changes.

### Routing widths and differential-pair gaps must be positive (patch release)

`route_trace`, `route_pad_to_pad`, `modify_trace`, and
`route_differential_pair` refuse non-positive widths as `invalid_argument` before
IPC. `route_differential_pair` applies the same rule to `gap`. Values that round
to zero nanometres and malformed supplied values are also refused. The schemas
advertise `exclusiveMinimum: 0`; handlers additionally check nanometre precision.

Supply positive numeric dimensions that round to at least one nanometre, or omit
them to retain the existing defaults (0.25 mm width, or 0.1 mm width and gap for
pairs). Existing valid calls and success responses are unchanged.

### `run_drc` names the checks a run ignored (minor release)

A DRC check whose severity is `ignore` is not run, so it never appears among
`run_drc`'s violations or counts, and a clean summary could not be told from a
board whose check was switched off
([#519](https://github.com/Copperpour/Copperpour/issues/519)). KiCad lists
those checks in its own DRC report; `run_drc`'s summary now gives their keys
as `ignored_checks`, in KiCad's order, for example
`["missing_courtyard", "footprint_filters_mismatch"]`. It is `null` when the
report carries no such list, which is not the same as `[]`, nothing ignored.

Read a count of zero beside `ignored_checks`. `set_drc_severity` turns an
ignored check back on. Nothing `run_drc` returned before changes.

### Placement, annotation and templates refuse a locked schematic as `conflict` (patch release)

`add_schematic_component`, `batch_place_components`, `annotate_schematic` and
`apply_template` reported a write that never happened as
`mutation_outcome_uncertain`, with `retry.safe: false`
([#427](https://github.com/Copperpour/Copperpour/issues/427)). That covered
KiCad's `~<name>.kicad_sch.lck` editor lock, and a schematic saved by another
program after the tool read it. Both are checked before the file is replaced
(for `annotate_schematic` over a hierarchy, before the transaction's journal is
written), so the files are unchanged.

They now return `conflict`, with `paths` holding the schematic, and write
nothing. For the lock, the message names the lock file: save and close the
schematic in Eeschema, then retry; a lock file that could not be inspected is
reported as that, with the reason. For a changed file, reload it and retry.
Where the tool returns an `outcome`, it is `status: "failed"` with
`retry.safe: true`. A readback that does not match after the replacement is
still `mutation_outcome_uncertain`.

### `add_schematic_shapes`, `list_schematic_shapes`; `edit_schematic_text` moves and resizes (minor release)

Two new `sch_batch` tools draw and read a sheet's graphic shapes, which carry
no electrical meaning
([#228](https://github.com/Copperpour/Copperpour/issues/228)); `sch_batch`'s
description now names them.

`add_schematic_shapes(schematic, shapes, revision?)` takes the shape types and
geometry keys `set_footprint_graphics` and `create_symbol` share: each shape is a
`line` (`start`, `end`), a `poly` (`points`, three or more, open unless its
last point repeats its first), a `rect` (`start`, `end`) or a `circle`
(`center`, `radius_mm`), with points as `{x, y}` in millimetres. Every shape
takes `stroke_width_mm` (default 0, the sheet's width), `stroke_type`
(`default`, `solid`, `dash`, `dot`, `dash_dot`, `dash_dot_dot`) and
`stroke_color`; a `poly`, `rect` or `circle` also takes `fill` (`none`,
`background`, or `color` with `fill_color`). A sheet has no `outline` fill
(KiCad 10 saves one as a colour fill in the stroke's colour), and KiCad's hatch
fills are listed but not written. Colours are `[r, g, b]` or
`[r, g, b, a]`, as `add_schematic_text` takes them. Coordinates are rounded to
KiCad's 0.0001 mm. Each shape's block is written as KiCad 10 saves it, so a
save in KiCad keeps its bytes, though KiCad may reorder the sheet's items.
Shapes go in the order given, before the sheet's first placed symbol, where
`add_schematic_text` writes a text, or before KiCad's trailing instance tables
on a sheet without one. A line or poly is a `(polyline …)`, as KiCad writes a graphic
line. Text is not a shape: `add_schematic_text` writes it.

All shapes are prepared as one `SchematicCommand` and committed through the
guard `edit_schematic_text` uses. One bad shape is `invalid_argument` naming
its field, for example `shapes[2].fill_color`, and nothing is written. A
`revision` the sheet no longer has, or a prepared sheet that changes anything
but the new shapes, is `stale_target`; `conflict` while KiCad holds the sheet
or its project's root schematic open, when the sheet changed after it was
read, or when its project cannot be told; `unsupported_capability` for a
sheet outside its project's directory. The sheet is read back: the response
holds `added_count`, each shape as listed from the file with its `uuid`, the
sheet's new `revision` and `source: "file"`; a file that is not the text
written is `mutation_outcome_uncertain`.

`list_schematic_shapes(schematic)` returns `count`, `shapes` and the sheet's
`revision` (the one `list_schematic_texts` returns), with `source: "file"`.
Each shape has `type`, `uuid`, its geometry, `stroke_width_mm`,
`stroke_type`, `stroke_color` and, but for a line, `fill` and `fill_color`,
a colour as `[r, g, b, a]` (whole-number channels) or `null`. A two-point
polyline is a `line`; an `arc` (`start`, `mid`, `end`) and a `bezier` (four
`points`) are listed though not written. Wires, buses, text and the shapes
inside library symbols are not listed. A listed line, poly, rect or circle
whose fill is `none`, `background` or `color`, its `uuid` and `null` values
dropped, can be passed back to `add_schematic_shapes`. A sheet holding a shape
that cannot be read, such as a polyline of one point, is `invalid_argument`
for both tools, as a sheet with an unreadable text item is for
`list_schematic_texts`. `batch_delete` removes a
shape by `uuid`, as it always has, without a revision or group check.

`edit_schematic_text` now also moves and resizes a text item, the rest of
#228's edit: `text` is optional, and the tool takes `x` and `y` (together,
rounded to KiCad's 0.0001 mm) and `size`, the font's height and width in
millimetres, both set to it, from 0.001 to 250 (KiCad loads a size outside
those as the nearest one). What is not given stays as written: the edit
replaces only the string, the `x y` of `(at …)` and the `height width` of the
font's `(size …)`, so the rotation, thickness, bold, italic, justification and
every other byte stay. A call with none of them, `x` without `y` or `y`
without `x`, a size outside that range, or one that would leave the item as it
reads is `invalid_argument`, and the prepared text must change nothing but
those tokens, to the values asked, or it is `stale_target`. A call that passes
only `text` behaves as before. The response adds `size` (`{height, width}`)
and the item's `old_x`, `old_y` and `old_size` to the fields it returned, and
`list_schematic_texts` adds each item's `size`, `null` for a font without one.

`sch_texts`'s write path is now shared with `add_schematic_shapes`.

### `score_placement` recognizes edge-mounted parts and measures connector distance from courtyard edges (minor release)

`score_placement` previously reported an `outside_outline` hard failure for any
footprint whose courtyard bbox crossed a rectangular `Edge.Cuts` outline, so
boards with castellated half-hole connectors or flush overhanging receptacles
could never reach `verdict: "pass"`. Separately, `connector_edges[].edge_distance_mm`
measured from the courtyard bbox centre instead of the nearest courtyard edge.

- Footprints with at least one drill hole on or crossing a rectangular
  outline edge (within `0.01 mm`) are now automatically recognized as
  edge-mounted and exempted from `outside_outline`. Copper pads alone on the
  edge are not, so a misplaced SMD part straddling it still fails.
- New optional argument `edge_mounted` (array of footprint reference strings)
  covers other footprints whose courtyard overhangs the outline (such as a
  flush USB-C receptacle, or an SMD module with castellated edge pads). Unknown references are refused with
  `invalid_argument`; listed footprints whose courtyard does not touch or cross
  the outline boundary (within `0.01 mm`) are not echoed as edge-mounted and
  emit a warning — wholly off-board parts still fail `outside_outline`, and
  wholly interior parts remain unexempted. Same-side `courtyard_overlap` checks
  still apply to every edge-mounted part.
- New response field `edge_mounted` lists each recognized edge-mounted part
  with `reference`, `reason` (`"hole_on_outline"`, `"explicit"`, or
  `"hole_on_outline_and_explicit"`), `edge_holes`, `overhang_mm`, and
  `exempted_outside_outline`.
- `connector_edges[].edge_distance_mm` now measures the shortest distance from
  the connector's courtyard bbox edge to the outline boundary (`0.0` when
  touching or crossing the outline; Euclidean distance when wholly outside),
  and each entry now also carries `center_edge_distance_mm` and
  `edge_mounted`. `connector_edge` deductions are evaluated against this
  courtyard-edge distance.

Issue: [#144](https://github.com/Copperpour/Copperpour/issues/144).

### `score_placement` reads an outline that a footprint draws (patch release)

A board whose only Edge.Cuts graphics belong to a footprint, as an imported
mechanical footprint's do, used to score as having no outline, and a courtyard
on that footprint's outline overlapped every part. When a board draws no
Edge.Cuts of its own, `score_placement` now reads the outline from footprints
as KiCad builds it: a footprint whose graphics close with a pad outside them,
or into more than one outline, cuts holes; the rest chain end to nearest end
within 0.01 mm into contours, classified as `outline_shape` always was. A
footprint with no electrical pads that is the outline is not scored as a part.

New response fields: `outline_source` (`"board"`, `"footprint"`, or `null`
when no outline is read), `outline_owners` (`reference`, `electrical_pads`,
`primitives`, `role`, `scored`), `outline_contours` (`role`, `references`,
`primitives`, `bbox`) and `warnings`.
When the footprints' graphics make no outline (an open end, a curve, cut-outs
only), the outline stays missing and a warning says why. The board is not
changed; moving the graphics to board graphics stays a manual step. Issue:
[#27](https://github.com/Copperpour/Copperpour/issues/27).

### MCP resource reads now return bundled guidance (minor release)

Every `initialize` response includes short operating rules, followed by any
existing one-time installed-guidance drift notice. Clients should no longer
expect `instructions` to be absent when there is no drift. This sends guidance;
it cannot force a client or model to follow it.

`initialize` advertises the static `resources: {}` capability. `resources/list`
lists bundled skill and reference Markdown at `copperpour://guidance/<path>`.
`resources/read` requires a string `uri` and returns MCP `contents` with `uri`,
`mimeType` and `text`, replacing the invalid empty `resources` read response.
Unknown resource identities return `-32002`; malformed parameters or a supplied
continuation cursor return `-32602`. There are no templates, subscriptions,
list-change notifications or pagination. `resources/templates/list` returns an
empty `resourceTemplates` list. Both list methods accept omitted parameters.

For clients that do not expose resources, the new always-visible `get_guidance`
meta-tool lists exact bundle paths when called with `{}`. Pass, for example,
`{"path":"copperpour/SKILL.md"}` or
`{"path":"copperpour/references/reliability-contract.md"}` to return the same
`contents`. Null/non-string paths, unknown paths and extra arguments are refused
as `invalid_argument`; only omission lists the catalog. Resource paths are exact
catalog identities, not local file paths. Resolve a relative reference from a
skill into that skill’s listed `references/` path before calling the tool.

Serving and native installation use the same embedded catalog. Server delivery
needs neither `init` nor filesystem/network access to guidance. It does not
create, overwrite or remove native skill files. `init` remains opt-in for native
skill discovery, local customization and Claude hooks; its arguments, existing
paths and drift reporting are unchanged. Custom skill roots and setup snippets
are separate later PRs for #461. Reconnect after upgrading the server to obtain
the new build’s catalog.

Protocol shape follows the [MCP 2025-06-18 resources specification](https://modelcontextprotocol.io/specification/2025-06-18/server/resources).

### `set_custom_rule` writes `bridged_mask` and `via_dangling` (minor release)

A constraint's `type` may now be `bridged_mask` or `via_dangling`
([#474](https://github.com/Copperpour/Copperpour/issues/474)). Neither takes a
min, opt or max: KiCad's DRC reads only the rule's severity, so with
`severity: "ignore"` a rule allows solder mask bridging or a dangling via on the
items its condition selects. The tool writes them bare, as
`(constraint bridged_mask)`, and refuses one given `min`, `opt` or `max` as
`invalid_argument` on that value's field, such as `constraints[0].min`,
writing nothing. Both types were refused before by the schema. `disallow`,
`zone_connection`, `min_resolved_spokes`, `assertion` and
`solder_paste_rel_margin` are still not written.

### `set_drc_severity` (minor release)

A new `verification` tool sets the severity of DRC checks in a project file
([#519](https://github.com/Copperpour/Copperpour/issues/519)), the sibling of
`set_erc_severity`: `set_drc_severity(project, severities)`. `project` is the
`.kicad_pro`; `severities` maps a check, by its key under
`board.design_settings.rule_severities` (for example `silk_edge_clearance`), to
`error`, `warning` or `ignore`. A check is one KiCad 10.0.6 has or one the file
already names, as a later KiCad writes the checks it adds. Only the named keys
are written, through the project file's revision check, and a project with no
`rule_severities` gets one holding only them. Any other check (KiCad's internal
ones and KiCad 8's `hole_near_hole` included) or severity, an empty map, a path
that is not a `.kicad_pro`, and a project KiCad holds (its
`~<project>.kicad_pro.lck`) are refused, and nothing is written.

The response gives `file`, `changed` (false when the file already held every
severity named, and then nothing is written) and `severities`, each check's
`previous` and `current`, `previous` null where the file did not name the
check. `run_drc` reports by the new severities from its next run, and a check
set to `ignore` is not run at all. No existing tool's arguments or responses
change. `verification` rises from 12 to 13 tools.

### `set_footprint_field_style` sets the layer, visibility and mirror flag (minor release)

`set_footprint_field_style` takes three more optional arguments, as KiCad's
Edit Text and Graphic Properties dialog sets them on a selection:

- `layer`: named as on a front-side footprint. A footprint on the back gets
  the flipped layer (`F.Fab` becomes `B.Fab`, `In1.Cu` becomes `In2.Cu` on four
  copper layers), as KiCad's Flip moves a footprint's own texts, so one call
  puts every Reference on its own side's Fab layer. A layer with no flipped
  counterpart on the board is refused as `invalid_argument` before anything is
  sent. Changing the layer keeps the mirror flag.
- `visible`: show or hide the field.
- `mirror`: `keep` (the default) or `auto`, which sets each field's mirrored
  flag from the side of the layer it ends on, as `edit_footprint_field` does.

All three need KiCad running with the board open. A call that names one of
them with no live KiCad holding the board is refused as `editor_unavailable`
and changes nothing; it does not fall back to the saved file.

Each field's `before` and `after` now also hold `layer`, `visible` and
`mirrored`, on both paths. On the file path `layer` is null for a field the
file stores no layer for.

Existing calls change only in those three added response keys.

### New `set_footprint_field_style` sets field text size and thickness on many footprints (minor release)

`update_footprints_from_library` with `reset_text_sizes_and_styles` gives
Reference and Value the library's text style. A style of the caller's own
choosing, such as a fabricator's minimum thickness above the library's or one
text size across the board, took one `edit_footprint_field` call per
footprint, with the board open in KiCad.

`set_footprint_field_style`, in the `pcb_text` toolset, sets `size_x`,
`size_y` and/or `thickness` on the Reference or Value field of the footprints
named in `references`, or of every footprint when it is omitted. With KiCad
holding the board it is one undo step, read back afterwards; with the board
closed it edits the saved file, only while the file still holds what it read.
Both paths answer with the same fields: `source` says which ran (`ipc` or
`file`), `undo` is set on the live path only, and `fallback_reason` (why the
file was edited) is null on the live path. `warning` says why the file was
edited on the file path; on the live path it is set only when footprints were
sent. The response lists each field's values before and after, and a whole-board
run reports fields it could not edit in `skipped`.

The live path sends each footprint back whole. See [footprint unit metadata loss before KiCad 10.0.8](../TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit).

No existing call changes.

### `set_footprint_graphics` accepts `selector.item_id` (minor release)

`set_footprint_graphics` previously accepted only `selector.layer`, so
`replace` and `delete` affected every supported graphic on that layer.

`selector` now accepts either `layer` or `item_id` (from
`get_footprint_info` with `include_graphics: true`). With `selector.item_id`:

- `replace` replaces that single graphic on its existing layer, preserving its
  `uuid` (or legacy `tstamp`) on a 1-for-1 replacement.
- `delete` removes that single graphic.
- `append` is refused with `invalid_argument`.
- Missing `item_id` returns `stale_target`; duplicate `item_id` or a graphic
  referenced by a `(group ...)` returns `conflict`.
- Responses include `"item_id"` alongside `"layer"` when selected by `item_id`.

Existing `selector.layer` calls are unchanged.

### `set_pad_net` proves the whole footprint after the update (patch release)

`set_pad_net` sends the pad's whole footprint back to KiCad, as KiCad 10 requires for a pad. Its read-back used to check only that the footprint kept its KIID and item count and that the edited pads held the net. It now shares the footprint update path with `set_teardrop_settings` and compares every field of the footprint, each pad and each other child with what was sent and with what was there before. A footprint that comes back changed anywhere else is reported as `ipc_outcome_unknown` naming the field, where it used to be reported as applied; KiCad reporting an error while the board holds the request is now reported as applied. Arguments and the success response are unchanged.

A target that changes before the write now returns `stale_target`, rather than an uncertain outcome; nothing was written. The public `IpcPadNetOutcome` enum adds `Stale { reason }` for this refusal.

### `set_symbol_properties` sets the Datasheet field (minor release)

`set_symbol_properties` takes `datasheet`, a URL or path for the symbol's
Datasheet field. Only the field's value changes; it keeps its position,
visibility and effects, and the answer reports its previous and current value
like the other fields. A `~` is written as an empty string, as `create_symbol`
writes it.

`fields` still refuses `Datasheet`, now pointing to the new argument, as it does
for Value, Footprint and Description. Before, a symbol's Datasheet could only be
set when `create_symbol` created it.

### `set_symbol_properties` sets the Reference prefix (minor release)

`set_symbol_properties` takes `reference_prefix`, the value of the symbol's
Reference field (such as `U` or `J`), as `create_symbol` takes it. Only the
field's value changes; the answer reports its previous and current value like
the other fields.

A value with nothing left once trailing digits and `?` are removed (`""`, `1`,
`?`) is refused as `invalid_argument`, as KiCad's Symbol Properties dialog
refuses it. A symbol without a Reference field is refused rather than given a
second one, as for Value.

`fields` still refuses `Reference`, now pointing to the new argument. Before, a
symbol's Reference could only be set when `create_symbol` created it.

### `set_via_positions` batches existing via moves (minor release)

Load `pcb_copper` for the new live-only `set_via_positions` tool. Existing
`move_via`, `query_vias` and `apply_copper` calls are unchanged.

Give either `placements: [{uuid, x, y}]` for absolute millimetre destinations,
or `uuids: ["..."]` and `offset: {x, y}` for one shared translation. The two
forms are mutually exclusive and must select at least one unique via. The tool
rounds to nanometres and refuses out-of-range coordinates and destinations,
missing/non-via IDs, and locked or unknown-lock-state targets.

`dry_run` defaults to `true`. Apply the identical plan with `dry_run: false` and
`expected_plan_revision` from the dry run. The revision binds the exact live
board snapshot, document, target preimages and normalized destinations. Changed
copper or target properties require a new dry run. An unchanged plan does not
open a commit.

Apply uses one KiCad commit and reads every selected via back, including all
native fields, not just position. If KiCad re-nets a via, a net-only mismatch
restores the entire batch in a second commit and verifies the original messages.
This is not a promise of atomicity: a failed or unverifiable write/restoration
returns `mutation_outcome_uncertain` with `uuids` and requires inspection before
retrying. A confirmed refusal is distinct from an uncertain mutation. Responses
from the handler include the shared `outcome` metadata (dispatch/schema refusals
retain their existing error contract); successful `vias` rows contain observed
UUID, X and Y, with `source: "ipc_readback"`. Dry-run rows are planned destinations
with `source: "ipc_plan"`, not observations of a completed move.

No board is saved and no connected track is moved or stretched. Zone refill and
DRC remain caller-controlled steps after a completed batch; refill before DRC.

### Hierarchical sheet writers use KiCad's autoplacement spelling (patch release)

Tools that rewrite hierarchical sheet items, including `edit_sheet`,
`move_sheet`, `renumber_sheet_pages`, `move_region` and sheet pin edits, now
write an enabled autoplacement flag as `(fields_autoplaced yes)` after the
sheet attributes and before the stroke and fill. Copperpour still reads the
legacy bare `(fields_autoplaced)` form. Callers that inspect raw schematic
text should accept both forms.

### Hierarchical sheet pins use KiCad's UUID field order (patch release)

When Copperpour writes a hierarchical sheet pin, its `(uuid …)` follows `(at …)` and precedes `(effects …)`, matching KiCad. UUID values and tool arguments and responses are unchanged. Callers do not need to change their requests.

### Whole-sheet writes pack `(pts …)` points as eeschema does (patch release)

A tool that rewrites a whole `.kicad_sch` used to put each `(xy …)` point of a
wire, bus or polyline on a line of its own. It now keeps a run of points on one
line while the line is shorter than 99 characters, a tab counting one, as
eeschema writes them, so the `(pts …)` blocks it did not edit come back as they
were. A sheet saved with one point per line is repacked on its next write. No
tool argument or response changes. Issue:
[#20](https://github.com/Copperpour/Copperpour/issues/20).

### `find_single_pin_nets` checks global and power nets across the project (minor release)

A net named by a global label or a power symbol is now checked across the
saved hierarchy of the project that owns the given sheet, as
`resolve_schematic_ownership` establishes it, over every top-level sheet the
project lists. Pins are counted once per sheet instance, as KiCad's netlist
counts them: a pin on a sheet placed twice is two pins, and this includes the
given sheet. When the design gives such a net a second pin, it is no longer
listed in `nets`; its name is in the new `cross_sheet_resolved` array.

`cross_sheet_unverified: false` on a listed net now means it was checked
project-wide, unless a hierarchical label also names it. The new
`sheets_checked` gives the number of sheet files read.

With no owning project, ambiguous or unproven ownership (for example two
`.kicad_pro` files that both could own the sheet), or a hierarchy that cannot
be walked, the response is as before and carries neither new key.
`single_pin_net_count` counts the nets still listed.

### `edit_footprint_pad` and `set_footprint_metadata` set solder mask and paste margins (minor release)

Both tools accept `solder_mask_margin` and `solder_paste_margin` (mm) and
`solder_paste_margin_ratio` (a fraction, as KiCad stores it: `-0.1` for
-10 %), as agreed on
[#637](https://github.com/Copperpour/Copperpour/issues/637).
`edit_footprint_pad` sets a pad's own margins, on every matching pad with
`match_all`; `set_footprint_metadata` sets the footprint's own.

- **Values:** a number writes the override where KiCad writes it. `0` is an
  override like any other value, since KiCad's format 20240202.
  `"inherited"` removes the override, so the footprint's or the board's
  value applies. Omitting an argument leaves the margin as it is.
- **Refused** as `invalid_argument` on the argument, with nothing written:
  anything else, such as `null` or a numeric string; a length beyond the
  largest KiCad keeps; and `0` in a file of format 20240201 or older, where
  KiCad reads a margin of 0 as inherited.
- **Answer:** `edit_footprint_pad` reports each margin set as
  `{"previous": …, "current": …}`, read as KiCad reads the pad, with
  `"inherited"` for no override; pads that disagree give a list of the
  distinct pairs. `set_footprint_metadata` reports a changed margin in
  `changed_fields` and `changes`, as it does its other fields.
- `set_footprint_metadata` now also accepts a call with only margins.

### The Specctra round trip takes bottom-side footprints (minor release)

`export_specctra_dsn` no longer refuses a footprint on `B.Cu`
([#430](https://github.com/Copperpour/Copperpour/issues/430), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). A bottom
footprint is written as KiCad's own exporter writes it: its front image, the
SMD pads on `F.Cu` and each pin's own turn mirrored, placed `back` at KiCad's
rotation plus 180 (normalized to 0..360). The router mirrors that image back
onto `B.Cu`. A bottom footprint's SMD pad must be on `B.Cu`, as a front
footprint's must be on `F.Cu`; any other layer is still refused. With the
KiCad 10 native bridge, KiCad's DSN must give every bottom footprint the same
side, rotation, pins and padstacks, or it is refused.

The reverse manifest keeps its schema. Each component's `side` is now the
side its footprint is on (`front` or `back`, it was always `front`), and its
`rotation_degrees` is the DSN `place` rotation. `supported_profile.component_side`
is derived from the footprints: `front`, `back` or `both`.
`apply_specctra_ses` and `plan_specctra_ses_import` refuse a manifest whose
component side differs from its footprint on the live board, or whose profile
side does not match its components, and still refuse a session that changes a
footprint's side, in every mode.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_both_sides_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes`,
and `capabilities.component_side` is new: `front`, `back` or `both`, from the
exported footprints.

### The Specctra round trip keeps copper zones as planes (minor release)

`export_specctra_dsn` no longer refuses a board with copper zones
([#430](https://github.com/Copperpour/Copperpour/issues/430), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Each zone is
written as its outline, one DSN `plane` of the zone's net on every copper
layer it covers, as KiCad's own exporter writes it; its fill is not
exported. Freerouting then reaches a plane through vias instead of routing
its net. Still refused, each with its own message: a rule area (keep-outs stay
with [#81](https://github.com/Copperpour/Copperpour/issues/81)), a zone
without a net, an outline with a cutout, an outline point that is not a
straight-edge corner, a zone on a layer that is not one of the board's copper
layers, a zone inside a footprint, and a zone whose net has no pad.

A copper layer the board's layer table marks `power` is now a DSN `power`
layer, where Freerouting lays no track, as KiCad's exporter writes it; it was
always `signal`. `signal`, `mixed` and `jumper` layers stay `signal`. With
the KiCad 10 native bridge, KiCad's planes must be the same outlines on the
same layers and nets, in any order, and its layer types the same, or the
native DSN is refused.

The reverse manifest moves to schema 3. `supported_profile.copper_zones` is
`true` when the board has zones, and the new `zones` list counts them per
copper layer and net (`{"layer", "net", "count"}`); a board without zones
omits it. `apply_specctra_ses` and `plan_specctra_ses_import` refuse a
manifest whose inventory differs from the live board's, and still read a
schema 1 or 2 manifest of a board without zones.

The import never creates, changes, deletes or refills a zone. Both tools'
responses have a new `zone_refill` object: `required` (whether the board has
zones), `zones` (the manifest's inventory) and `remedy` (what to run, or
`null`). It is a declared consequence of the import, not an observation of
the fills ([#71](https://github.com/Copperpour/Copperpour/issues/71)): the
candidate DRC of `apply_specctra_ses` runs on the fills as exported, so
refill them (`refill_zones`, or `run_drc` with `refill_zones` and
`sync_live_board`) before judging it.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes`,
and `capabilities.copper_zones` is new: the exported board's zone count.

### `export_specctra_dsn` adopts KiCad courtyard polygons and sub-micrometre pins (minor release)

Native Specctra adoption (`native_bridge_mode: "prefer"` or `"require"`)
previously refused KiCad DSNs from ordinary stock footprints because of two
parser/correlator limits ([#98](https://github.com/Copperpour/Copperpour/issues/98)):

- **Courtyard polygons:** KiCad exports a footprint's `fp_rect` courtyard as
  `(outline (polygon ...))`, which the underlying Specctra syntax validator
  rejected. Syntax validation now accepts `(outline (polygon ...))` while
  keeping the native DSN byte-for-byte unchanged.
- **Sub-micrometre pin positions:** Stock footprints such as `R_0805_2012Metric`
  place pads at ±0.9125 mm (±912.5 µm), and 45°-rotated footprints can store
  nanometre-rounded offsets such as ±499.999 µm. Copperpour's DSN model now
  preserves pad offsets at nanometre resolution—so both the default Rust
  exporter (`native_bridge_mode: "disable"` or `prefer` fallback, e.g. `-912.5`
  instead of `-913` and `±499.999` instead of `±500`) and native adoption write
  or match sub-micrometre pin coordinates, with native pins compared within one
  DSN resolution step (0.1 µm).

No tool names, arguments, or manifest schema fields changed. Callers comparing
Rust DSNs or routing results will see exact sub-micrometre pin coordinates
instead of whole-micrometre rounded ones, and boards with stock footprints that
previously fell back to Rust under `prefer` or failed under `require` can now
adopt KiCad's native DSN directly.

### The Specctra round trip takes 2 to 32 copper layers (minor release)

`export_specctra_dsn` no longer refuses a board whose copper is not exactly
`F.Cu` and `B.Cu` ([#430](https://github.com/Copperpour/Copperpour/issues/430),
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Any stack KiCad
gives a board is exported: `F.Cu`, `In1.Cu`..`In<N-2>.Cu` and `B.Cu`, N even,
written top to bottom as DSN signal layers 0 to N-1 whatever the order of the
board's layer table. A copper set with a gap, a repeat or a missing outer
layer is still refused. Through-hole pads must cover every copper layer
(`*.Cu`, or every copper layer listed once, in any order); a through-hole pad
on `F.Cu` and `B.Cu` only of a four-layer board is refused. A locked via must still be a through via, which
KiCad writes as `F.Cu` to `B.Cu` on any stack; its DSN padstack, like each
routing via's, has a shape on every copper layer. Component side, zones and
custom rules are unchanged.

The reverse manifest keeps schema 2: `supported_profile.copper_layers` is the
board's copper layer count instead of always `2`, and `layers` maps each of
them. `apply_specctra_ses` accepts such a manifest when its layer mappings are
exactly the stack of that count, indexed top to bottom; a manifest written by
an earlier release, with two layers, is read as before. A session wire on a
layer the manifest does not map is still refused.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`;
it was `two_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`.
`capabilities.copper_layers` is new: the exported board's copper layer count.
Callers that compared the profile string to the old value should read
`copper_layers` instead.

### `export_specctra_dsn` refuses KiCad's native DSN when it merges pads of different drills (patch release)

KiCad's native Specctra exporter names a pad's padstack after its copper
alone, so two pads with the same shape and size but different drills share one
padstack name. The native bridge adopted that DSN, and its reverse manifest
then held the same padstack name twice: `plan_specctra_ses_import` and
`apply_specctra_ses` refused it with "manifest contains an empty or duplicate
padstack name", only once the board had been routed. The export now refuses
the native DSN itself, with "KiCad native DSN gives one padstack '…' to pads
that the Rust export keeps apart: …", naming each pad with its drill. With
`native_bridge_mode` `prefer` the export uses the Rust DSN, which names one
padstack per shape and drill, and reports the refusal in
`native_bridge_diagnostics`; with `require` it fails before any routing. Boards
whose pads do not share a shape across drills are unaffected.

### `export_specctra_dsn` exports oval pads (minor release)

A pad with shape `oval`, as most through-hole connectors have, was refused with
`pad <reference>-<number> has shape 'oval'`
([#384](https://github.com/Copperpour/Copperpour/issues/384)). It is now
exported as KiCad's own Specctra exporter writes it: a `path` whose width is the
pad's shorter side, along its longer axis, on each copper layer of the pad. An
equal-sided oval is a path of zero length. An oval drill (a slot) is still
refused.

What changes for a caller:

- The response's `capabilities.supported_profile` is now
  `two_layer_front_side_circle_rect_roundrect_oval_no_existing_routing_or_zones`;
  it was `two_layer_front_side_circle_rect_roundrect_no_existing_routing_or_zones`.
- In the reverse manifest, `supported_profile.pad_shapes` lists `oval`, and an
  oval padstack has `"shape": "oval"`.
- With `native_bridge_mode` `prefer` or `require`, KiCad's oval padstacks are
  matched to Copperpour's. A stock footprint's native DSN still fails to parse
  on its courtyard polygon, so it isn't adopted
  ([#98](https://github.com/Copperpour/Copperpour/issues/98)).

### The Specctra round trip accepts pads on no copper layer (minor release)

`export_specctra_dsn` no longer refuses a footprint whose pads include one
on no copper layer, such as the unnumbered paste apertures over the exposed
pad or tab of KiCad's stock QFN, DFN, SO and TO footprints
([#458](https://github.com/Copperpour/Copperpour/issues/458)). An SMD pad
whose layers are all technical (`F.Paste`, `B.Mask`, ...) is left out of the
DSN image, as KiCad's own exporter leaves it out, so the router never sees
it. An unnumbered pad with copper, and an `np_thru_hole` pad, are still
refused.

The reverse manifest moves to schema 4. Each component lists such pads in
the new `non_copper_pads` array (`{"pad_number", "pad_uuid", "layers"}`;
`pad_number` may be empty); a component without them omits it.
`apply_specctra_ses` and `plan_specctra_ses_import` compare that list with
the live board's and refuse the session when a pad was added, removed,
renumbered or moved to other layers since the export. They still read a
schema 1 to 3 manifest, which never records one.

### `export_specctra_dsn` exports pads that share a number (minor release)

A footprint whose pads share a number, such as a connector's two `MP` pads, was
refused with `footprint '<reference>' has duplicate pad number '<number>'`
([#338](https://github.com/Copperpour/Copperpour/issues/338)). Each such pad is
now its own DSN pin, on its own net. The first, by pad UUID, keeps the number as
its name, and the others get `@1`, `@2`, as KiCad's own Specctra export names
them: `J1-MP` and `J1-MP@1`. Pads with a number of their own are named as
before.

The reverse manifest's `schema_version` is now 2. Each pin adds `pad_uuid`, the
pad's KiCad UUID, which says which physical pad a DSN pin is.
`plan_specctra_ses_import` and `apply_specctra_ses` now check every manifest pin
against the live board's pads, by UUID, number, net and name, and refuse a
mismatch before anything is planned. They still read a schema 1 manifest,
checked by number. A board with a pad without a UUID, or a UUID two pads share,
is refused, and so is a pad number equal to a name the suffix would give.

With `native_bridge_mode` `prefer` or `require`, a board with such pads is not
adopted from KiCad's native DSN: KiCad names them by file order, which isn't
proven to map one to one. `prefer` uses Copperpour's export; `require` refuses.

### `export_specctra_dsn` supports rounded board outlines (minor release)

The Rust exporter used to refuse `Edge.Cuts` arcs. It now flattens circular
arcs into line segments within the board project's `max_error` tolerance,
keeping one closed `Edge.Cuts` loop. Boards without a `.kicad_pro` file use
KiCad's 5 µm default. Unsupported outline geometry and multiple or open loops
remain refused.

The reverse manifest's `supported_profile.outline` now describes a closed
loop of lines and circular arcs flattened within `max_error`; callers should
not rely on the former straight-lines-only description.

### The Specctra round trip keeps rule areas as keep-outs (minor release)

`export_specctra_dsn` no longer refuses a board with rule areas
([#81](https://github.com/Copperpour/Copperpour/issues/81), part of
[#37](https://github.com/Copperpour/Copperpour/issues/37)). Each supported board
rule area is written as its outline per copper layer it covers:

- `tracks` and `vias` not allowed -> `keepout`
- `tracks` not allowed, `vias` allowed -> `wire_keepout`
- `vias` not allowed, `tracks` allowed -> `via_keepout`

What Specctra DSN keep-outs cannot express without approximation is still
refused, each with its own message: a rule area that allows both tracks and vias
(pour-, pad- or footprint-only keep-outs, which KiCad's own exporter turns into
full `keepout`s), a rule area with room `placement` enabled, a rule area with a
net, an outline with a cutout or non-straight edge, a rule area off the copper
stack, a zone inside a footprint, and any vertex off the whole-micrometre grid.
For example, `113.001` mm is supported but `113.0006` mm is refused. This check
runs before either writer; vertices are never rounded into a smaller keep-out. With the KiCad 10 native bridge, KiCad's
keep-outs must match the exported kinds, layers and closed outlines in any
order, or the native DSN is refused.

The DSN kinds encode the tracks/vias restrictions. Pad, copper-pour and footprint
flags do not change the DSN kind when routing is blocked: those objects and their
original rule-area settings remain in the revision-bound KiCad board, and SES
import cannot move components or change zones. Original copper zones still need
refill after routing. Areas that restrict only these non-routing objects remain
refused because translating them to full routing keep-outs would change intent.

The reverse manifest moves to schema 5. `supported_profile.rule_areas` is `true`
when the board has rule areas, and the new `rule_areas` list records each one's
`kind` (`keepout`, `wire_keepout`, `via_keepout`), `layers` and `outline_um`
points in canonical order; a board without rule areas omits it.
`apply_specctra_ses` and `plan_specctra_ses_import` refuse a manifest whose
rule-area inventory differs from the live board's, still read a schema 1 to 4
manifest of a board without rule areas, and reject any SES track, arc or via
whose copper overlaps an active rule-area keep-out on a covered layer.

In the `export_specctra_dsn` response, `capabilities.supported_profile` is now
`even_layer_both_sides_circle_rect_roundrect_oval_no_existing_routing_zones_as_planes_rule_areas_as_keepouts`,
and `capabilities.rule_areas` is new: the exported board's rule-area count.

### `plan_specctra_ses_import` and `apply_specctra_ses` accept negative footprint angles (patch release)

KiCad writes a footprint's angle in (-180, 180], and Freerouting returns it in
[0, 360): a footprint at -90 degrees comes back as 270. The import compared the
two numbers as written and refused every board with a footprint at a negative
angle with "SES changes placement of component '…'". It now compares placement
angles modulo 360, with the same 1e-9 degree tolerance. A session that really
turns a footprint (90 to 180, for example) is still refused with the same
error; position, side and image checks are unchanged.

### `export_specctra_dsn` accepts two-edge (D-shaped) outlines (minor release)

The Rust exporter used to refuse an `Edge.Cuts` loop of fewer than three lines
or arcs, so a D-shaped board (one line and one arc) failed with "need at least
three Edge.Cuts lines or arcs" although KiCad exports it. Two edges now form a
valid loop when it encloses an area. Two straight lines between the same two
points, or one arc drawn twice, are refused with "two Edge.Cuts edges enclose
no area". A board without any `Edge.Cuts` line or arc is refused with "no
Edge.Cuts lines or arcs". A lone full `gr_circle` outline is still refused.

### `split_wire_at_point` writes a junction only at a T that has none (patch release)

`split_wire_at_point` used to write a junction dot at every split point: a
second dot where a T already had one, a dot where only the wire's two halves
meet, and a dot where another wire crosses with nothing ending there, which
joined that wire to the split one. KiCad keeps such dots through a resave and
its ERC does not report them. Now the point gets a dot only where a wire end,
pin, sheet pin or bus entry there makes three or more distinct wire exits and
none stands, and the response adds `junctions_added_count`, 0 or 1. A caller
that split a wire at a crossing to join the two adds the dot with
`add_junction`. Issue: [#570](https://github.com/Copperpour/Copperpour/issues/570).

### `create_symbol` and `edit_symbol_pin` warn when a pin is off the 1.27 mm grid (patch release)

`create_symbol` now adds a per-unit entry to `warnings` when any resolved pin
connection point `(x, y)` does not lie on KiCad's 1.27 mm schematic grid, and
`edit_symbol_pin` now returns a `warnings` array when a position edit (`x` or
`y`) leaves the pin off the 1.27 mm grid. Both tools still write the requested
coordinates; callers that expect a clean `warnings`-free response should pass
multiples of 1.27 mm (or omit `x`/`y` on `create_symbol` for automatic
grid-aligned placement).

### Transaction conflict classification across project and schematic tools (minor release)

`SexpError::TransactionConflict` now carries a typed `TransactionConflictKind` (`BeforeImage`, `Recovery`, or `AfterImage`) so callers and MCP dispatch distinguish why a durable multi-file transaction failed:

- `create_project` still returns `conflict` naming the colliding project file when a target already exists (`BeforeImage`), now returns `conflict` naming the stuck journal and `copperpour transaction status` / `recover` / `abandon` when an older journal in the directory cannot be recovered (`Recovery`), and returns `mutation_outcome_uncertain` when after-image durability verification fails (`AfterImage`).
- `rename_project`, `set_schematic_title_block`, `annotate_schematic` (`hierarchy`), `upgrade_library`, and other transaction-backed schematic tools (`add_hierarchical_sheet`, `duplicate_sheet`, `renumber_sheet_pages`, sheet mutations) now classify `BeforeImage` as `conflict`, `Recovery` as `conflict` naming the stuck journal and recovery command, and `AfterImage` as `mutation_outcome_uncertain` instead of `handler_error` or a stale-target message.

Callers branching on `error.kind` should handle `conflict` (safe refusal; inspect `message` / `paths` for a stuck journal) and `mutation_outcome_uncertain` (reload and inspect before retrying).

### Finish pending version 2 transaction journals before downgrading (minor release)

Copperpour still writes version 1 journals for transactions that do not need to guard an absent path. A transaction that needs to make sure a path stays absent writes a version 2 journal. Earlier Copperpour releases cannot inspect or recover version 2 journals.

Before downgrading, use a release that supports version 2. Run `copperpour transaction status <project-dir>` to check for pending journals. If any remain, run `copperpour transaction recover <project-dir>`, then run `status` again and confirm it reports `No active transaction journals.`

If an editor lock blocks recovery, close KiCad and retry. If `status` lists a file as `divergent`, inspect the file and restore it to the expected state only if you can do so without losing edits. Do not force-abandon a journal just to downgrade: abandoning does not undo writes that already happened. If you already downgraded and the older release rejects the journal, return to a release that supports version 2 to finish recovery.

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

### `update_pcb_from_schematic` applies a routed pad net change the caller acknowledges (minor release)

A pad net change on a net that routed copper uses, as a renamed or split net
makes on a routed board, was a `routed_pad_net_change` conflict with no way
past it. A caller can now acknowledge each one:

- Each `routed_pad_net_change` diagnostic carries
  `net_change: { "pad", "from", "to" }`. The key is absent from every other
  diagnostic but the note below.
- `acknowledged_net_changes: [{ "reference", "pad", "from", "to" }]` names
  the changes to plan anyway, each copied from a diagnostic. A named change
  is planned as any other pad net change: the pad takes its new net, and
  Copperpour sends no copper. KiCad's commit then moves a track or via onto
  the new net when every pad it joins is on it, as after its own Update PCB,
  and leaves the rest. A `routed_pad_net_change_acknowledged` diagnostic with
  the same `net_change` notes it and refuses nothing.
- A named change the plan does not make is a
  `net_change_acknowledgement_unmatched` conflict. A routed change left
  unnamed, and every other conflict, still refuses the plan.
- The acknowledgement is part of the plan: dry run with it, then apply with
  the same acknowledgements and that dry run's `plan_revision`. The
  conflict's own revision is stale for the apply.
- The apply's response adds
  `routed_copper: [{ "net", "before_count", "after_count" }]`, each named
  net's tracks, arcs, vias and copper zones before the commit and after, as
  KiCad reads them back, and `drc`, kicad-cli's DRC of the board KiCad then
  holds under the project's rules, with the keys `apply_specctra_ses`
  reports. The board file is not saved. Both keys are absent from every other
  response.
- If the readback or the DRC fails after the commit, or a copper zone reads
  back with no net it can be counted on, the response is still `applied`, is
  an error, and carries `acknowledged_apply_unchecked`: the plan must not be
  applied again.

Without `acknowledged_net_changes` the tool behaves as before, but for
`net_change` on `routed_pad_net_change`.

### `update_pcb_from_schematic` places pads with their fabrication properties (minor release)

A footprint the sync added lost every pad's fabrication property,
`(property pad_prop_heatsink)`, `pad_prop_bga`, `pad_prop_mechanical` and the
rest, without saying so, and KiCad's DRC then reported
`lib_footprint_mismatch` for it
([#149](https://github.com/Copperpour/Copperpour/issues/149)). On KiCad
10.0.7 and later each added pad now gets its library property, and the
post-apply readback reports a pad property the board lost as
`board_readback_differs`.

KiCad before 10.0.7 can't take the property over its API. There the footprint
is still placed without it, and the dry run and the apply carry a new
non-blocking diagnostic, `pad_properties_not_carried`. It names the footprint in
`footprint_id`, the parts in `references`, and the pads, their properties and
the KiCad version in `message`. KiCad's Tools > Update Footprints from Library
restores the properties. The plan's `status` stays `ready`. Its
`plan_revision` includes whether the properties are carried, so a plan made on
one side of KiCad 10.0.7 is stale on the other.

### `update_pcb_from_schematic` gives footprints their sheet name, sheet file and footprint filters (minor release)

A footprint the sync placed had no sheet name, sheet file or footprint
filters until KiCad's own Update PCB ran, and KiCad's rule areas generated
from sheets, and Repeat Layout with them, find a sheet's footprints by their
sheet name ([#237](https://github.com/Copperpour/Copperpour/issues/237)). The
sync now sets all three as Update PCB does: the symbol's sheet path, its
sheet's file and its library symbol's footprint filters.

Each planned `add` and `update` in `changes` carries them as `sheet_name`,
`sheet_file` and `footprint_filters`. A footprint whose three differ from the
schematic's is an `update`, so the first sync of a board an earlier sync built
plans an update of every footprint that sync placed. A footprint that owns
groups, points or variants and differs only in these three leaves the plan
instead, with a `footprint_sheet_not_updated` diagnostic that refuses nothing:
KiCad's Update PCB sets its sheet in place. One that differs in anything else
as well gets a `footprint_children_not_carried` diagnostic, and its part is
skipped (`update-pcb-skips-unbuildable-parts.md`). The
post-apply readback reports a sheet the board did not keep as
`board_readback_differs`.

### `update_pcb_from_schematic` replaces a footprint the schematic names differently (minor release)

A footprint whose symbol names another library footprint stopped the whole
sync with a `footprint_id_changed` conflict. The sync now replaces it in place,
as KiCad's Update PCB does (`BOARD_NETLIST_UPDATER::replaceFootprint`):

- The planned change is an `update` with
  `replace_footprint: { "from": "<old Library:Footprint>", "to": "<new>" }`.
  The key is absent from every other update.
- The replaced footprint keeps its KIID, position, rotation, side, lock and
  symbol link, and each pad's net and schematic pin by pad number. It takes
  the new library footprint's pads, artwork, texts, custom properties,
  attributes, its own clearance, zone connection and solder mask and paste
  margins, and 3D models, and Reference and Value take the library's
  position and style when the library footprint states them as properties
  (KiCad 8 and later); a footprint in the older `fp_text reference` form
  leaves the placed text as it is. The schematic's value, fields, DNP and BOM exclusion are
  then set as for any update; every symbol field is set again, because the
  old custom properties are gone.
- A locked footprint is not replaced. Its other differences are still
  planned, and the plan carries a `footprint_locked_not_replaced` diagnostic
  that refuses nothing, as KiCad's Update PCB warns.
- The dry run builds each replacement once, so a planned replacement is one
  the apply can build. One it cannot build is reported when the new footprint
  does not resolve (`footprint_library_resolution_failed`), cannot be read
  (`footprint_library_read_failed`) or cannot be rebuilt losslessly
  (`unsupported_library_footprint`), when it lacks a pad the schematic
  connects (`footprint_pad_missing`), or when it would drop a pad that carries
  a net on the board (`footprint_replacement_drops_connected_pad`). Its part
  is skipped and listed under `skipped_footprints`; see
  `update-pcb-skips-unbuildable-parts.md`.

`footprint_id_changed` is no longer reported. A caller that read it as "this
part needs a manual footprint change" can review the planned replacement in
the dry run and apply it.

### `update_pcb_from_schematic` matches equivalent root-prefixed schematic identities (patch release)

`update_pcb_from_schematic` now normalizes only the requested root schematic's
UUID prefix (`/<root_uuid>/...`) when comparing exported root-relative netlist
identities against stored PCB footprint `symbol_path` values.

- Existing footprints carrying either `/<root_uuid>/<sheet...>/<symbol>` or
  `/<sheet...>/<symbol>` match the exported component (including reference
  renames, unassigned components, and `on_board=no` exclusions) and retain
  their stored PCB `symbol_path` and pose on updates.
- Newly added footprints continue to receive native root-relative paths from
  the netlist export.
- Foreign root UUIDs, other sheet instances, other symbol UUIDs, and boards
  carrying simultaneous rooted and root-relative aliases for the same identity
  remain refused (`reference_identity_conflict` / `duplicate_board_identity`)
  before any PCB mutation.

No caller changes are required.

### `update_pcb_from_schematic` skips a part it cannot build instead of refusing the sync (minor release)

One part the dry run found it could not build made the whole sync a conflict,
with nothing planned for the parts that could be built. Such a part is now
skipped and every other part is synced, as KiCad's Update PCB reports its
errors and still commits the rest (`BOARD_NETLIST_UPDATER`). KiCad handles
some of these parts differently: it adds a footprint that lacks a connected
pad and reports the pad, and it keeps the old footprint of a replacement it
cannot build. The sync leaves the whole part as it is instead.

- A part is skipped when its library footprint does not resolve, cannot be
  read or cannot be placed (`footprint_library_resolution_failed`,
  `footprint_library_read_failed`, `unsupported_library_footprint`), when its
  footprint lacks a pad the schematic connects (`footprint_pad_missing` for an
  added part), when its update would drop groups, points or variants the
  footprint owns (`footprint_children_not_carried`), or when its replacement
  footprint cannot be built (the replacement codes of
  `update-pcb-replaces-footprints.md`, including
  `footprint_replacement_drops_connected_pad`).
- New response key `skipped_footprints`: each skipped part's diagnostic, with
  the same fields and codes the conflict carried (`code`, `message`,
  `reference`, `references`, `footprint_id`). One diagnostic can name several
  parts that share a footprint, and one part can have several diagnostics.
  The apply answer carries it too.
- New count `coverage.skipped_footprint` (`planned`, `applied`): how many parts
  were skipped, by the dry run and by the apply (as `unassigned_footprint`
  counts parts left out). The other counts no longer include them, nor the pad
  net changes of a skipped update.
- The status is `ready` when anything else is left to apply, and the apply
  applies only that. The skipped parts are part of the plan revision.
- A note about a skipped part, such as `footprint_locked_not_replaced`, is
  dropped from `diagnostics`. So is `routed_pad_net_change_acknowledged`: an
  acknowledged routed pad net change (`acknowledged_net_changes`) goes with its
  part: the apply neither makes it nor runs the post-apply copper and DRC
  check for it.

Unchanged:

- With nothing else to apply, the dry run is a `conflict` as before, with the
  same diagnostics in `diagnostics`, counted in `coverage.conflicts`, and
  `skipped_footprints` empty. So is a plan whose updates cannot be checked
  because the board KiCad wrote cannot be read or lacks a planned footprint:
  that is not the part's fault, so nothing is skipped for it.
- The planner's own conflicts still refuse the whole sync: a duplicate
  reference, a pad net change on routed copper that `acknowledged_net_changes`
  does not name, an acknowledgement that matches no change, a pad a placed
  footprint lacks (without a replacement).

A caller that read a `conflict` as "nothing can be synced until every part is
fixed" should check `skipped_footprints` on a `ready` plan: those parts stay as
they are on the board until the cause is fixed and the sync is run again.

### New toolset `library_maintenance` with `upgrade_library` (minor release)

The new `library_maintenance` toolset holds `upgrade_library(library_path,
dry_run?, limit?)`. It upgrades a `.kicad_sym` symbol library or a `.pretty`
footprint library to the running KiCad's file format with `kicad-cli sym
upgrade` or `fp upgrade`. The upgrade runs into a temporary copy. Each file
older than the format kicad-cli writes is compared with its upgraded copy as
KiCad reads both, including the conversions KiCad's loader makes on older
files: overbars, `%R`/`%V`, footprint attributes before 20200826, margins and
clearances of 0 up to 20240201, net ties from keywords up to 20220815, De
Morgan body styles before 20250827, pad layer wildcards, and hidden footprint
texts as fields.

- **Not compared:** graphics, zones and groups are compared by count, not
  geometry, and text positions not at all.
- **Answer:** `kind` (`symbol` or `footprint`), `status` (`current`, `ready`,
  `blocked`, or `applied` after an apply), `format_version`, `counts` per file
  status (`current`, `upgraded`, `differs`), and `files`: up to `limit`
  (default 50) entries, those that differ first, then those with `notes`, then
  upgraded and current ones, each with `version`, `upgraded_version`, `status`,
  `differences` and, where there are any, `notes`. `shown` and `truncated`
  describe the list. `warning` says that older KiCad versions cannot open an
  upgraded file.
- **Dry run** (the default): writes nothing. An unfinished transaction in the
  library's directory is a `conflict`, since it could still change the files.
- **Apply** (`dry_run: false`): first completes an unfinished transaction in
  the library's directory and answers how many in `settled_transactions`; one
  it cannot complete is a `conflict` with nothing written. It then replaces the
  older files in one journalled transaction and answers `replaced`, the number
  of files. A file already in kicad-cli's format is left as it is. It is
  refused, writing nothing:
  - as `plan_blocked` when any file differs; `reasons` names the first
    differences;
  - as a `conflict` when a file changed after the read;
  - as a handler error when a file to replace is read-only, or when the
    library has more older files than the process may open at once.

  A failure after the transaction's journal is written is
  `mutation_outcome_uncertain`: the next apply completes the transaction, and
  `copperpour transaction status <directory>` shows what was applied.
- **Notes:** a footprint whose name inside the file is not its file name is a
  note, not a difference: KiCad uses the file name.

### Schematic viewer defaults to the local X display on Linux (patch release)

**Open in KiCad** now starts KiCad with `DISPLAY=:0` on Linux when both
`DISPLAY` and `WAYLAND_DISPLAY` are unset or empty. Existing display settings,
`XAUTHORITY`, and runtime variables are preserved; the fallback affects only the
KiCad child process. Other platforms and `launch_kicad_ui` are unchanged.

If the fallback display is unavailable or inaccessible, the viewer still reports
the startup failure, identifies the attempted fallback, and recommends configuring
Copperpour with the desktop's display and `XAUTHORITY`. To use another display,
set the server's display environment before starting Copperpour. The button label
and tooltip now use the official KiCad spelling.

### The schematic viewer renders with the server's `kicad-cli` (patch release)

`open_schematic_viewer` now renders with the `kicad-cli` the other kicad-cli
tools run. A bare `kicad_cli` (the default, `kicad-cli`) is looked up on `PATH`,
then in the install paths, where it used to be handed to the OS unresolved. An
empty one is looked up in the install paths, as `get_installation_info` does,
then as `kicad-cli` on `PATH`; it used to search a separate list that lacked
`~/Applications/KiCad` on macOS. An explicit path is still used as given.

On Linux the install paths now include `/snap/kicad/current/usr/bin/kicad-cli`,
which only the viewer used to try, so `get_installation_info` and the kicad-cli
tools find a snap install too. Nothing to change in calls.

### The viewer's Open in KiCAD honours `kicad_binary` (patch release)

**Open in KiCAD** in a viewer started by `open_schematic_viewer` now runs the
KiCad that `launch_kicad_ui` runs: the configured `kicad_binary`, else the GUI
beside the configured `kicad_cli`, else the usual install paths, else `kicad`
on `PATH`. It used to check only a fixed list of install paths and `kicad` on
`PATH`, so a Flatpak wrapper or an `/opt` install set in `kicad_binary` was
ignored.

On Linux, both now also try `/usr/bin/kicad` and `/usr/local/bin/kicad` after
`PATH`, so a server started with a reduced `PATH` still finds a stock install;
`launch_kicad_ui` used to fall back to a bare `kicad` there. Nothing to change
in calls.

### Schematic viewer's Open in KiCAD reports a KiCad that exits at startup (patch release)

`POST /open-in-kicad` on the browser viewer started by `open_schematic_viewer`
now waits up to a second for KiCad to exit, as `launch_kicad_ui` does. A KiCad
that exits in that time, such as one started with no `DISPLAY`, returns `503`
with its exit status and the end of its stderr, where it used to return
`200 {"spawned":true}`. A second request while one launch is being watched
returns `409`. A KiCad that stays up still returns `200 {"spawned":true}`, now
about a second later. KiCad no longer inherits the server's stdin and stdout.

The page shows the error in its status bar. Fix the cause it names, for example
the server's display, and click again.

### `open_schematic_viewer` identifies missing schematics (minor release)

> Later superseded by [standalone viewer retirement](retire-standalone-viewer.md):
> the owner waived the planned release grace period and approved immediate removal.
> Statements below about retention describe this earlier phase only.

A schematic path that cannot resolve because it does not exist now returns
structured `file_not_found` with the requested `path`, instead of a generic
`handler_error`. Other resolution failures retain their existing legacy error text; clients
should not interpret a permission error or symlink loop as a missing file.

Correct the path before retrying. This refusal starts no viewer service or browser.
Existing successful response fields, service ownership, browser-launch behavior,
and the standalone Tauri deprecation window are unchanged.

The shared browser handle adds `request_shutdown()`, a non-consuming shutdown
signal. `is_running()` becomes false when stop is requested; `shutdown().await`
waits for task teardown. The MCP registry replaces stopped services rather than
reusing their credential URLs. No new MCP tool or HTTP stop endpoint is added.

### The wiring tools refuse a pin that two placed symbols share (minor release)

`connect_to_net`, `connect_pins`, `batch_connect_to_net` and
`batch_connect_pins` resolved `reference` + `pin_number` to the first placed
symbol carrying the pin, in file order, and reported success
([#88](https://github.com/Copperpour/Copperpour/issues/88)). A duplicated
reference, or a pin drawn on more than one placed unit of one symbol, now names
more than one pin and is refused, as `add_power_symbol` already refused it.

`connect_to_net` and `connect_pins` return `ambiguous_target`, with `target`
(`R4 pin 1`) and `candidates` naming each pin by unit, UUID and position, and
write nothing. Give `connect_to_net` the chosen pin's `pin_x` + `pin_y`; wire
`connect_pins`' ends with `add_schematic_connection`. A missing component or pin
is now `stale_target` with a `reason` (`component R99 is not present`,
`U1 has no pin 9 on placed units 1, 3`) instead of a plain-text error. So is a
reference with a placed unit whose library definition is not embedded: that
unit may be the one carrying the pin, so it is no longer skipped.

The batch tools skip such a pin and list it in `errors`
(`R4 pin 1 names more than one placed pin: …`), and the rest of the batch still
lands, as for a pin that does not resolve. Their other `errors` lines now read
`R99 pin 1: component R99 is not present` instead of
`Component 'R99' not found`.

`add_power_symbol`'s `reason` for a missing pin now names the units searched:
`R1 has no pin 9 on placed unit 1`.

### `add_zone` and `add_copper_pour` refuse a vertex missing `x` or `y` (patch release)

A `points` item without `x` or `y` used to be left out of the outline, and a
zone of the remaining vertices was written and reported as added. Such a call
is now refused as `invalid_argument` naming the item and coordinate, such as
`points[3].y`, and nothing is written
([#611](https://github.com/Copperpour/Copperpour/issues/611)), as
`batch_add_junction` refuses a position missing either. A call whose vertices
all carry both coordinates behaves as before.

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
