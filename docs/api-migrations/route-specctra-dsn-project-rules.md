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
