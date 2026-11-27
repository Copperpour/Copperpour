# Embedded symbol export metadata fixtures

Except for the legacy sheet below, each sheet is KiCad 10.0.8 output from
`kicad-cli sch upgrade --force`. The
baseline keeps one placed LM358 unit from `../pin_location_mirror_kicad10.kicad_sch`
and labels two of its pins. Every pair uses the same basename, preserves sheet
and symbol UUIDs, and changes one input to the export. A resave can also rewrite
derived pin records in the placed symbol.

KiCad 10.0.6 and 10.0.8 agree on the changes below. The `<nets>` records are
unchanged in every pair. Compare each case with `baseline` unless noted.

| Candidate | Edit | XML difference |
| --- | --- | --- |
| `duplicate-jumper` | Enable duplicate-pin jumper flag | Component flag |
| `jumper-group` | Group pins 4 and 8 in unplaced C | Component jumper groups |
| `unit-name` | Name unplaced unit B `Secondary` | Component unit B name |
| `unit-pin-number` | Renumber unplaced B's pin 7 to 17 | Unit B pin list and `<libparts>` pin |
| `pin-unit-x-order` | Move pin 8 past pin 4 in unplaced C | Unit C pin order |
| `pin-unit-membership` | Move B's embedded records to slot 4 | Component unit count and pin lists |
| `libpart-field` | Add embedded `AuditVendor` | `<libparts>` field |
| `libpart-pin-name` | Rename pin 8 in unplaced C | `<libparts>` pin name |
| `lib-name` | Rename the embedded definition and reference it by `lib_name`, keeping `lib_id` | Component `libsource`, library part identity and libraries |
| `alias-pin-type` | Change the type of pin 8 in `lib-name`'s local definition | `<libparts>` pin type |
| `drawing` | Change an outline stroke | None outside the `<design>` header |
| `pin-text-style` | Change a pin name font size | None outside the `<design>` header |
| `mixed-version-modern` (from `mixed-version-legacy`) | Save in format 20260306 with literal `~` on unplaced pin 2 | `<libparts>` pin name changes from empty to `~` |

The mixed-version pair uses a minimal two-unit symbol with one placed unit.
The legacy sheet was saved in Eeschema 9.0.9 using Save As. The modern sheet
was upgraded in 10.0.8, given a literal `~` in place of the now-empty name,
and resaved with a same-name project file so the saved instance key matches
the legacy sheet. The project file is not needed to compare or export the
frozen fixtures. Both saves contain `(name "~" ...)`, but KiCad reads that
token differently based on the format version.

The KiCad board updater reads jumper settings and ordered, nonempty unit pin
lists from the netlist. The XML also contains library fields and individual pin
attributes; the board netlist reader does not copy those onto footprints.

The served tests compare copies in separate directories. They assert the named
reason and that the tool changed neither input bytes nor directory entries.
