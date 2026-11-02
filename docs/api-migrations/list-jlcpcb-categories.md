### `list_jlcpcb_categories` lists the values `search_jlcpcb_parts` accepts as `category` (minor release)

New tool in the `integration` toolset. Without arguments it lists the
top-level category groups of the local JLCPCB database, each with
`part_count`, `in_stock_count` and `subcategory_count`. With `contains`, it
lists every full category name (`Group / Subcategory`) containing that text,
case-insensitive, with `part_count` and `in_stock_count`.

`search_jlcpcb_parts`' `category` filter now matches `%` and `_` literally,
as the query words already did; it was a substring match before and remains
one. `get_jlcpcb_database_stats` adds `category_count`.
