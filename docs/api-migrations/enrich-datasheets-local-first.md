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
- `attempted_sources`: list of sources queried, such as `["local_catalog"]` or
  `["local_catalog", "lcsc_api"]`.

Previous fields (`datasheets_enriched`, `lcsc_ids_found`, `updated`, and
`schematic`) remain present and preserve their behavior.
