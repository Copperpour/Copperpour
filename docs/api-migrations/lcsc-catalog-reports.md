### Catalog tools report a missing or unreadable JLCPCB catalog alike (minor release)

The tools that read the local JLCPCB catalog now find it the same way
(`db_path`, then the `jlcpcb_db_path` setting, then the default path) and
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
  and the catalog is opened read-only.
- `enrich_datasheets` keeps its fields. `local_catalog_error`, the skipped
  reason and the refusals of an explicit `db_path` use the wording above.
- `estimate_cost`'s `components.catalog.reason` uses the wording above.
